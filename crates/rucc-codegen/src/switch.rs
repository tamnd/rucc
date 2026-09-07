//! What a `switch` becomes on the way to the machine, and how that shape is chosen.
//!
//! Design: `spec/optimizer/24-switch-lowering.md`. Section 24.4 puts the choice here, at the
//! boundary into the machine level and nowhere earlier, and section 24.2 says what the choice
//! looks like.
//!
//! # Why the shape is decided here and not in the front end
//!
//! What a `switch` should become is a target decision and not a language one. A chain of compares
//! is right for three cases and wrong for two hundred, where the answer is a jump table, and wrong
//! again for twenty spread over a million, where it is a binary search on the value. A front end
//! that picked one would be picking for every target at once, and the IR would no longer hold what
//! the program said. So the `switch` survives as far as here, and here is where it is given up.
//!
//! Keeping it whole that long buys something on the way as well. A `switch` is one node from which
//! the range on each outgoing edge is exact: on the edge to case five the operand is five, and on
//! the default edge it is outside the case set. A `switch` lowered early is a pile of branches that
//! every pass afterwards has to work those facts back out of.
//!
//! # A switch is a partition and not a shape
//!
//! The reason to sort the cases and cut them into runs, rather than pick one shape for the whole
//! statement, is that a real `switch` is more than one thing at once. A `switch` in a parser has a
//! dense stretch of ASCII values best served by a jump table, a few scattered large constants best
//! served by comparisons, and a set of aliased cases best served by a bit test, all in the same
//! statement. A design that picks one shape for the whole of it cannot say that. So the case list
//! is sorted, partitioned into clusters, and a decision tree is built over the clusters.
//!
//! Two of the four shapes are written. A `Cluster::One` is one case value and one equality test,
//! which is what every case was before this module existed. A `Cluster::Run` is a stretch of
//! consecutive values that all go to the same place, and it is one subtraction and one unsigned
//! comparison however long the stretch is, which is what makes `case 'a' ... 'z'` twenty six cases
//! in the IR and two instructions in the machine code.
//!
//! The two that are not written are the jump table and the bit test, and each of them is a variant
//! this enum gains rather than a rewrite of anything here. The jump table is waiting on
//! `Opcode::IndirectBr`, which is tamnd/rucc#353 and is the same thing a computed goto waits on,
//! and on a read only section to put the table in. The bit test is waiting on nothing.
//!
//! # Why the tree compares signed
//!
//! The IR gives a `switch` a width and not a signedness, because signedness in this IR is a
//! property of an operation rather than of a type, so there is nothing here to ask whether the
//! program switched on an `int` or an `unsigned`. What makes that harmless is that the sort and the
//! tree use the same order: the cases are sorted by their signed reading and the tree splits with a
//! signed comparison, so the tree is consistent with itself and every value comes down it to the
//! one cluster that can hold it. Sorting one way and comparing the other is the bug this is
//! written to not have.
//!
//! A run is not affected either way. Testing `x - low` against `high - low` unsigned is modular
//! arithmetic and gives the same answer whichever way the operand is read.
//!
//! # What it refuses to get wrong
//!
//! Section 24.6 lists the ways this goes wrong and two of them are arithmetic. The width of a run
//! is worked out in `i128`, which holds the difference of any two values of any type C can switch
//! on, so nothing here overflows the way the same computation in the switch's own type would. A run
//! that covers a whole type comes out as a width of every bit set, which read as an unsigned
//! comparison is a test that is true of everything, and that is exactly right for a `switch` no
//! value falls out of.
//!
//! The third is the default edge, and the rule is that it is never dropped. Every leaf of the tree
//! ends by branching to the default, so a value that matches nothing arrives there whichever way it
//! came down, and there is no path through any of this that leaves a block without saying where
//! control goes next.
//!
//! # What it does not carry yet
//!
//! Section 24.5 asks for document 11's `Frequency` on every cluster from the start, so that the
//! tree can lean towards the hot cases rather than be balanced, and so that adding it later is not
//! a change to every place a cluster is built. It is not here because there is nowhere to read it
//! from. Block frequencies are worked out in `rucc-opt`, which is above this crate rather than
//! below it, and what would carry the number down is the IR, which has nowhere to put it yet.

use rucc_diag::Span;
use rucc_ir::{
    Block, BlockCall, Builder, Extra, Flags, Func, Imm, Inst, IntPred, Opcode, Type, Value,
};

/// The most clusters a leaf of the decision tree tests one at a time, which is also the count below
/// which nothing new is built at all. A `switch` of this many clusters or fewer stays the chain of
/// compares it has always been, in the block it has always been in.
///
/// Thirty two, and the number is measured rather than picked. What it trades is not a comparison
/// against a comparison, which is what it looks like on paper and is the reason a small number looks
/// right. A walk of `n` clusters is `n` compares and a search is about `log2(n)`, so on paper the
/// search wins from about five cases upward and the threshold should be about five.
///
/// The machine does not agree, because the two kinds of comparison do not cost the same. Every
/// compare in a walk is a branch that is almost never taken, one case out of `n`, so the predictor
/// gets all of them right and the front end runs through them several per cycle. Every branch in a
/// search is a branch that goes each way about half the time, so the predictor gets a fair share of
/// them wrong and each of those costs the whole pipeline. Twenty compares nobody mispredicts are
/// cheaper than six branches that mispredict a third of the time, and that stays true further up
/// than it seems it should.
///
/// Measured on an interpreter loop dispatching on a sparse `switch`, four million iterations picking
/// a case at random, the walk is ahead up to about thirty two cases and the search is ahead above
/// about thirty six. At seventeen cases a search costs sixteen percent, at twenty four it costs
/// twenty two, at thirty six it saves nine, at fifty it saves twenty three and at a hundred it saves
/// half. Thirty two is where those two lines cross.
///
/// Two things would move it. The first is a jump table, which is what a dense `switch` this large
/// should become and which is waiting on `Opcode::IndirectBr`. Once dense cases stop reaching the
/// tree at all, what is left in it is sparser, and a sparser search may be worth starting sooner.
/// The second is knowing which case is hot, because a walk that tests the common case first is
/// cheaper than any search and the tree cannot use that ordering. That is document 11's `Frequency`
/// and it is not carried here yet.
///
/// gcc has the same knob under the name `case-values-threshold` and a small number in it, which is
/// the right number for gcc because gcc reaches for a jump table first and the tree is what it falls
/// back to on cases a table cannot hold.
pub const LINEAR: usize = 32;

/// Rewrites every `switch` in the function into branches, and leaves everything else alone.
///
/// The function is changed in place, which is what makes this the last thing that reads the IR as
/// the front end built it. `--emit=ir` prints before this runs, and nothing after this asks what
/// the program said, only what the machine has to do.
pub fn switches(func: &mut Func) {
    let found: Vec<Inst> = func
        .blocks()
        .filter_map(|block| func.terminator(block))
        .filter(|&inst| func[inst].opcode == Opcode::Switch)
        .collect();
    for inst in found {
        lower(func, inst);
    }
}

/// One `switch`, as the clusters its cases fall into and a decision tree over them.
fn lower(func: &mut Func, inst: Inst) {
    let block = func.block_of(inst).expect("a terminator is in a block");
    let span = func.span(inst);
    let Extra::Switch(info) = func[inst].extra else { return };
    let info = func[info];
    let Some(&value) = func[func[inst].args].first() else { return };
    // The lane, because a `switch` on a vector is not a thing C can write and the immediates are
    // an integer's either way.
    let ty = func[value].ty.lane();
    let calls: Vec<BlockCall> = func[info.targets].to_vec();
    let cases: Vec<Imm> = func[info.cases].to_vec();
    let Some((&default, arms)) = calls.split_first() else { return };
    let clusters = clusters(func, &cases, arms, ty);

    // Before anything is written, because the builder appends and the `switch` is where the
    // appending has to happen.
    func.remove_inst(inst);
    tree(func, &Lowering { value, ty, default, span }, block, &clusters);
}

/// What every test written for one `switch` shares.
///
/// The tree hands the same four things down to every leaf and every leaf hands them to every test,
/// so they travel together rather than as four more parameters at each step.
struct Lowering {
    /// The operand being switched on.
    value: Value,
    /// Its width, which every constant written here takes.
    ty: Type,
    /// Where a value that matches no case goes, which is every leaf's last edge.
    default: BlockCall,
    /// The source location of the `switch`, which everything written for it takes.
    span: Span,
}

/// A stretch of case values that one test separates from the rest of them.
///
/// This is the structure `spec/optimizer/24-switch-lowering.md` section 24.2 describes, with the
/// two variants that can be written today. It is an enum rather than a struct with a low and a high
/// in it because the two that are missing carry things these do not: a jump table carries a table
/// and a bit test carries a mask, and the point of the shape is that adding one of those is a
/// variant here and an arm in [`test`] rather than a change to how a `switch` is taken apart.
#[derive(Clone, Copy, Debug)]
enum Cluster {
    /// One case value, which is one equality test.
    One {
        /// The value the operand has to equal.
        value: i128,
        /// Where it goes when it does.
        call: BlockCall,
    },
    /// Every value from `low` to `high`, all of which go to the same place.
    Run {
        /// The lowest value in the run.
        low: i128,
        /// The highest, which is at least one above the lowest.
        high: i128,
        /// Where any of them goes.
        call: BlockCall,
    },
}

impl Cluster {
    /// The lowest value this cluster holds.
    fn low(self) -> i128 {
        match self {
            Self::One { value, .. } => value,
            Self::Run { low, .. } => low,
        }
    }

    /// The highest value this cluster holds.
    fn high(self) -> i128 {
        match self {
            Self::One { value, .. } => value,
            Self::Run { high, .. } => high,
        }
    }

    /// Where every value in it goes.
    fn call(self) -> BlockCall {
        match self {
            Self::One { call, .. } | Self::Run { call, .. } => call,
        }
    }

    /// Grows the cluster upwards to a value, which the caller has already checked is the one
    /// immediately above it and goes to the same place.
    fn grow(&mut self, value: i128) {
        *self = Self::Run { low: self.low(), high: value, call: self.call() };
    }
}

/// The case list sorted and cut into clusters.
///
/// Sorting is what makes the rest of this possible: a decision tree needs an order to split on, and
/// a run of consecutive values is only visible once the values are next to each other. It is
/// `n log n` and it is the most expensive thing in the module, which section 24.7 says is fine
/// because everything here is cheap next to the size of the construct.
///
/// # Panics
///
/// Panics on two cases of the same value. C forbids them and the front end rejects them, so
/// everything below is written believing the clusters are disjoint, and section 24.6 asks for that
/// belief to be recorded here rather than left implicit. Dropping the later of the pair instead
/// would leave its arm with nothing branching to it, which is a function the IR verifier refuses,
/// and quietly keeping both would put two clusters of the same value into a search that assumes it
/// can tell them apart. A `switch` that arrives with a duplicate is a bug above this, and stopping
/// on it is how it gets found.
fn clusters(func: &Func, cases: &[Imm], arms: &[BlockCall], ty: Type) -> Vec<Cluster> {
    let mut sorted: Vec<(i128, BlockCall)> =
        cases.iter().zip(arms).map(|(&imm, &call)| (imm.signed(ty), call)).collect();
    sorted.sort_by_key(|&(value, _)| value);
    assert!(
        sorted.windows(2).all(|pair| pair[0].0 != pair[1].0),
        "a switch with two cases of the same value reached the back end"
    );

    let mut clusters: Vec<Cluster> = Vec::with_capacity(sorted.len());
    for (value, call) in sorted {
        match clusters.last_mut() {
            // In `i128`, so that a run reaching the top of its own type is the addition it looks
            // like rather than an overflow.
            Some(last) if last.high() + 1 == value && same(func, last.call(), call) => {
                last.grow(value);
            }
            _ => clusters.push(Cluster::One { value, call }),
        }
    }
    clusters
}

/// Whether two edges go to the same block carrying the same values.
///
/// Both halves matter. Two cases whose arms are the same block but which pass it different
/// arguments are two different destinations, and merging them into a run would hand the block one
/// of the two whichever value arrived.
fn same(func: &Func, a: BlockCall, b: BlockCall) -> bool {
    a.block == b.block && func[a.args] == func[b.args]
}

/// A binary search over the clusters, ending in a chain of tests at each leaf.
///
/// The split is at the middle of the list and the test is whether the operand is below the lowest
/// value of the upper half. Everything the lower half holds is below that value because the list is
/// sorted and the clusters are disjoint, so an operand that is below it and matches anything at all
/// matches something in the lower half, and one that is not is either in the upper half or in
/// neither. Either way it reaches a leaf that tests what is left, and the leaf sends it to the
/// default when none of that matches.
fn tree(func: &mut Func, of: &Lowering, at: Block, clusters: &[Cluster]) {
    if clusters.len() <= LINEAR {
        chain(func, of, at, clusters);
        return;
    }
    let (below, above) = clusters.split_at(clusters.len() / 2);
    let pivot = above[0].low();
    let left = func.create_block();
    let right = func.create_block();

    let mut build = Builder::new(func, at).at(of.span);
    let want = build.iconst(of.ty, pivot);
    let under = build.icmp(IntPred::Slt, of.value, want);
    build.br_if(under, left, &[], right, &[]);

    tree(func, of, left, below);
    tree(func, of, right, above);
}

/// The clusters tested one after another, each falling to the next and the last to the default.
///
/// The block this starts in gets the first test, and each test after the first gets a block of its
/// own that the one before it falls to when its test failed. The last falls to the default, so the
/// default is not a block anything is created for and a chain of `n` clusters costs `n` less one.
fn chain(func: &mut Func, of: &Lowering, at: Block, clusters: &[Cluster]) {
    // A leaf with nothing in it is a jump. It is what a `switch` whose only label is `default` is,
    // and it is also what one whose cases a later pass folded away would be.
    let Some((last, rest)) = clusters.split_last() else {
        let args: Vec<Value> = func[of.default.args].to_vec();
        Builder::new(func, at).at(of.span).jump(of.default.block, &args);
        return;
    };

    let mut at = at;
    for cluster in rest {
        let next = func.create_block();
        test(func, of, at, *cluster, next, &[]);
        at = next;
    }
    let onward: Vec<Value> = func[of.default.args].to_vec();
    test(func, of, at, *last, of.default.block, &onward);
}

/// One cluster, as the comparison that decides it and the branch that acts on it.
fn test(
    func: &mut Func,
    of: &Lowering,
    at: Block,
    cluster: Cluster,
    next: Block,
    onward: &[Value],
) {
    let call = cluster.call();
    let taken: Vec<Value> = func[call.args].to_vec();
    let mut build = Builder::new(func, at).at(of.span);
    let matched = match cluster {
        Cluster::One { value, .. } => {
            let want = build.iconst(of.ty, value);
            build.icmp(IntPred::Eq, of.value, want)
        }
        Cluster::Run { low, high, .. } => {
            // `(unsigned)(x - low) <= high - low`, which is one comparison covering both ends of
            // the run: a value below the bottom wraps round to something enormous and fails the
            // same test a value above the top fails.
            let base = if low == 0 {
                of.value
            } else {
                let start = build.iconst(of.ty, low);
                build.binary(Opcode::Sub, of.value, start, Flags::default())
            };
            let width = build.iconst(of.ty, high - low);
            build.icmp(IntPred::Ule, base, width)
        }
    };
    build.br_if(matched, call.block, &taken, next, onward);
}

/// The blocks a leaf chain of `n` clusters needs beyond the ones the program already had.
///
/// Here so that a test can say the number rather than count it, and so that whoever writes the jump
/// table has one place to compare against. A `switch` that goes to a tree needs more than this,
/// since the tree's own nodes are blocks too, and a test that cares about one of those counts them.
#[must_use]
pub fn blocks_for(clusters: usize) -> usize {
    clusters.saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rucc_base::Interner;
    use rucc_ir::{
        Block, BlockCall, Builder, Extra, Func, Imm, InstData, IntPred, Module, Opcode, Signature,
        SwitchInfo, Type, Value,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{LINEAR, blocks_for, switches};

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// Where a `switch` over these cases can end up, as the arms in the order given and the default.
    struct Built {
        names: Interner,
        func: Func,
        operand: Value,
        arms: Vec<Block>,
        default: Block,
    }

    /// `int sw(int x) { switch (x) { case 1: return 10; ... default: return 0; } }` as the walk
    /// builds it, which is the program in issue 275.
    ///
    /// Every arm is a block of its own even when two cases would naturally share one, because a
    /// test that wants two cases going to one place says so by passing the same block twice, and
    /// [`built_sharing`] is how it does that.
    fn built(cases: &[i128]) -> Built {
        let arms: Vec<usize> = (0..cases.len()).collect();
        built_sharing(cases, &arms, Type::int(32))
    }

    /// The same, with `arms[i]` saying which arm case `i` goes to, so that several cases can share.
    fn built_sharing(cases: &[i128], arms: &[usize], ty: Type) -> Built {
        let mut names = Interner::new();
        let int = Type::int(32);
        let mut func =
            Func::new(names.intern("sw"), Signature::new().with_params(&[ty]).with_returns(&[int]));
        let entry = func.create_block();
        let x = func.append_param(entry, ty);

        let default = func.create_block();
        let count = arms.iter().copied().max().map_or(0, |top| top + 1);
        let blocks: Vec<Block> = (0..count).map(|_| func.create_block()).collect();
        let table: Vec<(i128, Block)> =
            cases.iter().copied().zip(arms.iter().map(|&at| blocks[at])).collect();
        Builder::new(&mut func, entry).switch(x, default, &table);

        for (index, &arm) in blocks.iter().enumerate() {
            let mut build = Builder::new(&mut func, arm);
            let what = i128::try_from(index).expect("a small number of arms");
            let v = build.iconst(int, (what + 1) * 10);
            build.ret(&[v]);
        }
        let mut build = Builder::new(&mut func, default);
        let v = build.iconst(int, 0);
        build.ret(&[v]);
        Built { names, func, operand: x, arms: blocks, default }
    }

    fn count(func: &Func) -> usize {
        func.blocks().count()
    }

    fn printed(func: &Func, names: &mut Interner) -> String {
        let module = Module::new(names.intern("sw.c"), &target());
        rucc_ir::print_func(&module, func, names)
    }

    fn verified(built: &mut Built) {
        let module = Module::new(built.names.intern("sw.c"), &target());
        rucc_ir::verify_func(&module, &built.func, &built.names)
            .expect("the rewrite builds valid IR");
    }

    /// Where the operand `x` ends up, worked out by running what the lowering wrote.
    ///
    /// This is the test the shape actually needs. Counting compares says the tree is small and says
    /// nothing about whether it is right, and a decision tree that sends one value down the wrong
    /// side is a miscompilation that no amount of counting finds. So the blocks the lowering built
    /// are interpreted for a concrete operand, and the answer is the block it arrives at.
    ///
    /// It understands the five things this module writes and nothing else, which is how it knows it
    /// has arrived: an arm ends in a `return`, so the walk stops at the block whose instructions it
    /// cannot follow.
    fn arrives(func: &Func, operand: Value, x: i128, ty: Type) -> Block {
        let mut at = func.entry().expect("an entry block");
        let mut held: HashMap<Value, i128> = HashMap::new();
        held.insert(operand, x);
        loop {
            let mut moved = None;
            for inst in func.insts(at).collect::<Vec<_>>() {
                let opcode = func[inst].opcode;
                let extra = func[inst].extra;
                let result = func[inst].first_result;
                let args: Vec<i128> = func[func[inst].args]
                    .iter()
                    .map(|value| held.get(value).copied().unwrap_or(0))
                    .collect();
                match opcode {
                    Opcode::IConst => {
                        let Extra::Imm(imm) = extra else { return at };
                        let value = func[imm].signed(ty);
                        held.insert(result.expect("a constant has a result"), value);
                    }
                    Opcode::Sub => {
                        let wrapped = Imm::int(args[0] - args[1], ty).signed(ty);
                        held.insert(result.expect("a subtraction has a result"), wrapped);
                    }
                    Opcode::ICmp => {
                        let Extra::IntPred(pred) = extra else { return at };
                        let unsigned = |v: i128| Imm::int(v, ty).unsigned();
                        let answer = match pred {
                            IntPred::Eq => args[0] == args[1],
                            IntPred::Slt => args[0] < args[1],
                            IntPred::Ule => unsigned(args[0]) <= unsigned(args[1]),
                            other => panic!("the lowering does not write {}", other.name()),
                        };
                        held.insert(result.expect("a comparison has a result"), i128::from(answer));
                    }
                    Opcode::Jump => {
                        let call = func.successors(inst).next().expect("a jump has a target");
                        moved = Some(call.block);
                    }
                    Opcode::BrIf => {
                        let mut targets = func.successors(inst);
                        let taken = targets.next().expect("a branch has two targets");
                        let other = targets.next().expect("a branch has two targets");
                        moved = Some(if args[0] != 0 { taken.block } else { other.block });
                    }
                    _ => return at,
                }
            }
            match moved {
                Some(next) => at = next,
                None => return at,
            }
        }
    }

    /// Every probe arrives where the case list says it should, whatever shape the lowering picked.
    fn routes(built: &mut Built, cases: &[i128], arms: &[usize], probes: &[i128], ty: Type) {
        switches(&mut built.func);
        verified(built);
        for &x in probes {
            let wanted = cases
                .iter()
                .position(|&case| case == x)
                .map_or(built.default, |at| built.arms[arms[at]]);
            let got = arrives(&built.func, built.operand, x, ty);
            assert_eq!(got, wanted, "the operand {x} went to the wrong block");
        }
    }

    /// Every case value, both sides of every one of them, and the ends of the type.
    fn around(cases: &[i128], ty: Type) -> Vec<i128> {
        let mut probes: Vec<i128> = Vec::new();
        for &case in cases {
            probes.extend([case - 1, case, case + 1]);
        }
        let bits = ty.bits();
        probes.extend([0, -1, 1, i128::from(i32::MIN) >> (32 - bits), (1 << (bits - 1)) - 1]);
        probes.retain(|&x| Imm::int(x, ty).signed(ty) == x);
        probes.sort_unstable();
        probes.dedup();
        probes
    }

    #[test]
    fn a_small_switch_is_a_compare_and_a_branch_for_each_case() {
        let mut built = built(&[1, 2]);
        let before = count(&built.func);
        switches(&mut built.func);
        assert_eq!(count(&built.func), before + blocks_for(2));

        let text = printed(&built.func, &mut built.names);
        assert!(!text.contains("switch"), "the switch is gone: {text}");
        assert_eq!(text.matches("icmp eq").count(), 2, "one compare per case: {text}");
        assert_eq!(text.matches("br_if").count(), 2, "one branch per case: {text}");
    }

    #[test]
    fn the_last_case_falls_to_the_default_rather_than_to_a_block_of_its_own() {
        let mut built = built(&[7]);
        let before = count(&built.func);
        switches(&mut built.func);
        // One case needs no chain block at all: the one compare goes to the arm or to the default.
        assert_eq!(count(&built.func), before);
        assert_eq!(blocks_for(1), 0);
    }

    #[test]
    fn a_switch_with_only_a_default_is_a_jump() {
        let mut built = built(&[]);
        switches(&mut built.func);
        let entry = built.func.entry().expect("an entry block");
        let term = built.func.terminator(entry).expect("a terminator");
        assert_eq!(built.func[term].opcode, Opcode::Jump);
    }

    /// The rewrite has to leave a function the verifier still accepts, since every check it makes
    /// is one the rest of the back end assumes and none of them is rechecked after this runs.
    #[test]
    fn what_comes_out_is_valid_ir() {
        let mut built = built(&[1, 2, 3, 4]);
        switches(&mut built.func);
        verified(&mut built);
    }

    /// Nothing else is touched, which matters because this runs over every function whether or not
    /// one has a `switch` in it.
    #[test]
    fn a_function_with_no_switch_is_left_exactly_as_it_was() {
        let mut names = Interner::new();
        let int = Type::int(32);
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_params(&[int]).with_returns(&[int]));
        let entry = func.create_block();
        let x = func.append_param(entry, int);
        Builder::new(&mut func, entry).ret(&[x]);

        let before = printed(&func, &mut names);
        switches(&mut func);
        assert_eq!(printed(&func, &mut names), before);
    }

    #[test]
    fn a_run_of_cases_going_to_one_place_is_one_range_test() {
        let cases = [3, 4, 5, 6, 7, 8, 9, 10];
        let arms = [0; 8];
        let mut built = built_sharing(&cases, &arms, Type::int(32));
        switches(&mut built.func);

        let text = printed(&built.func, &mut built.names);
        assert_eq!(text.matches("icmp").count(), 1, "eight cases, one test: {text}");
        assert_eq!(text.matches("icmp ule").count(), 1, "and the test is the range: {text}");
        assert_eq!(text.matches("sub").count(), 1, "one subtraction to bring it to zero: {text}");
    }

    #[test]
    fn a_run_that_starts_at_zero_needs_no_subtraction() {
        let cases = [0, 1, 2, 3, 4];
        let arms = [0; 5];
        let mut built = built_sharing(&cases, &arms, Type::int(32));
        switches(&mut built.func);

        let text = printed(&built.func, &mut built.names);
        assert_eq!(text.matches("icmp ule").count(), 1, "one range test: {text}");
        assert!(!text.contains("sub"), "nothing to subtract from zero: {text}");
    }

    /// The clusters are what the tree is built over, so a `switch` of forty cases that fall into
    /// three runs is a `switch` of three tests and not a search.
    #[test]
    fn the_tree_is_built_over_the_clusters_and_not_over_the_cases() {
        let cases: Vec<i128> = (0..30).collect();
        let arms: Vec<usize> = (0..30).map(|at: usize| at / 10).collect();
        let mut built = built_sharing(&cases, &arms, Type::int(32));
        switches(&mut built.func);

        let text = printed(&built.func, &mut built.names);
        assert_eq!(text.matches("icmp").count(), 3, "three runs, three tests: {text}");
        assert!(!text.contains("icmp slt"), "three clusters is under the leaf size: {text}");
    }

    /// The number the whole thing is for. Forty scattered cases used to be forty comparisons on the
    /// way to the last of them, and a binary search is the difference between that and seven.
    #[test]
    fn a_long_sparse_switch_is_a_search_rather_than_a_walk() {
        // Four leaves' worth, so the tree is two splits deep and the bound below is a bound on
        // something rather than a restatement of the leaf size.
        let count = 4 * LINEAR as i128;
        let cases: Vec<i128> = (0..count).map(|at| at * 7).collect();
        let mut built = built(&cases);
        switches(&mut built.func);

        let worst = deepest(&built.func);
        assert!(worst <= LINEAR + 2, "{count} cases in {worst} comparisons at worst");
        assert!(worst > LINEAR, "and the splits are being counted too");
    }

    /// The most comparisons on any path from the entry to an arm.
    ///
    /// A depth first walk over the blocks the lowering wrote, which is a directed acyclic graph
    /// because every branch it writes goes forward, so no path is walked twice and nothing loops.
    fn deepest(func: &Func) -> usize {
        fn walk(func: &Func, at: Block, seen: &mut HashMap<Block, usize>) -> usize {
            if let Some(&known) = seen.get(&at) {
                return known;
            }
            let here = func.insts(at).filter(|&inst| func[inst].opcode == Opcode::ICmp).count();
            let term = func.terminator(at).expect("a terminator");
            let onward: Vec<Block> = match func[term].opcode {
                Opcode::Jump | Opcode::BrIf => {
                    func.successors(term).map(|call| call.block).collect()
                }
                _ => Vec::new(),
            };
            let below =
                onward.into_iter().map(|block| walk(func, block, seen)).max().unwrap_or_default();
            seen.insert(at, here + below);
            here + below
        }
        walk(func, func.entry().expect("an entry block"), &mut HashMap::new())
    }

    #[test]
    fn every_value_reaches_the_arm_its_case_named_in_a_small_switch() {
        let cases = [1, 2, 3];
        let arms = [0, 1, 2];
        let ty = Type::int(32);
        let mut built = built(&cases);
        routes(&mut built, &cases, &arms, &around(&cases, ty), ty);
    }

    #[test]
    fn every_value_reaches_the_arm_its_case_named_in_a_search() {
        let count = 3 * LINEAR;
        let cases: Vec<i128> = (0..count as i128).map(|at| at * 7).collect();
        let arms: Vec<usize> = (0..count).collect();
        let ty = Type::int(32);
        let mut built = built(&cases);
        routes(&mut built, &cases, &arms, &around(&cases, ty), ty);
    }

    /// The one the signed sort and the signed split have to agree about. A `switch` whose cases sit
    /// on both sides of zero is where sorting one way and comparing the other goes wrong.
    #[test]
    fn every_value_reaches_its_arm_when_the_cases_straddle_zero() {
        let half = LINEAR as i128;
        let cases: Vec<i128> = (-half..half).map(|at| at * 3).collect();
        let arms: Vec<usize> = (0..2 * LINEAR).collect();
        let ty = Type::int(32);
        let mut built = built(&cases);
        routes(&mut built, &cases, &arms, &around(&cases, ty), ty);
    }

    /// Runs and single values in the same statement, which is the partition the module is named
    /// after and the thing a design that picked one shape could not say.
    #[test]
    fn every_value_reaches_its_arm_when_runs_and_singles_are_mixed() {
        let cases: Vec<i128> =
            vec![-9, -8, -7, -6, 0, 5, 6, 7, 8, 9, 10, 40, 41, 90, 91, 92, 93, 94, 95, 200];
        let arms: Vec<usize> = vec![0, 0, 0, 0, 1, 2, 2, 2, 2, 2, 2, 3, 4, 5, 5, 5, 5, 5, 5, 6];
        let ty = Type::int(32);
        let mut built = built_sharing(&cases, &arms, ty);
        routes(&mut built, &cases, &arms, &around(&cases, ty), ty);
    }

    /// A run that covers a whole type, where the width of it is every bit set and the comparison
    /// against it is a test that is true of everything. Section 24.6 calls this out because the
    /// same arithmetic in the switch's own type overflows here rather than wrapping usefully.
    #[test]
    fn a_run_covering_the_whole_type_matches_everything() {
        let cases: Vec<i128> = (-128..128).collect();
        let arms = vec![0; cases.len()];
        let ty = Type::int(8);
        let mut built = built_sharing(&cases, &arms, ty);
        switches(&mut built.func);
        verified(&mut built);

        let text = printed(&built.func, &mut built.names);
        assert_eq!(text.matches("icmp").count(), 1, "one run, one test: {text}");

        let entry = built.func.entry().expect("an entry block");
        let operand = built.func[entry].params[0];
        for x in [-128, -1, 0, 1, 127] {
            assert_eq!(
                arrives(&built.func, operand, x, ty),
                built.arms[0],
                "every value of the type is in the run"
            );
        }
    }

    /// C forbids one and the front end rejects one, and everything the clusters promise each other
    /// rests on that, so a duplicate that got this far stops the compiler rather than being guessed
    /// at. Section 24.6 asks for the assumption to be recorded, and this is the record.
    #[test]
    #[should_panic(expected = "two cases of the same value")]
    fn a_case_value_written_twice_stops_the_compiler() {
        let cases = [4, 9, 4];
        let arms = [0, 1, 2];
        let mut built = built_sharing(&cases, &arms, Type::int(32));
        switches(&mut built.func);
    }

    /// Two consecutive cases whose arms are the same block but which pass it different arguments
    /// are two destinations, so they are two clusters and not one run. Nothing the front end writes
    /// produces this today, which is why the `switch` has to be built by hand, and the check is
    /// there because a run that merged them would hand the block one of the two values whichever
    /// case arrived.
    #[test]
    fn cases_that_share_a_block_but_not_its_arguments_are_not_a_run() {
        let mut names = Interner::new();
        let int = Type::int(32);
        let mut func = Func::new(
            names.intern("sw"),
            Signature::new().with_params(&[int]).with_returns(&[int]),
        );
        let entry = func.create_block();
        let x = func.append_param(entry, int);
        let default = func.create_block();
        let join = func.create_block();
        let param = func.append_param(join, int);

        let mut build = Builder::new(&mut func, entry);
        let ten = build.iconst(int, 10);
        let twenty = build.iconst(int, 20);
        let none = func.push_values(&[]);
        let first = func.push_values(&[ten]);
        let second = func.push_values(&[twenty]);
        let targets = func.push_block_calls(&[
            BlockCall { block: default, args: none },
            BlockCall { block: join, args: first },
            BlockCall { block: join, args: second },
        ]);
        let cases = func.push_imms(&[Imm::int(1, int), Imm::int(2, int)]);
        let info = func.add_switch(SwitchInfo { targets, cases });
        let args = func.push_values(&[x]);
        let data = InstData { args, extra: Extra::Switch(info), ..InstData::new(Opcode::Switch) };
        Builder::new(&mut func, entry).inst(data, &[]);

        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);
        let mut build = Builder::new(&mut func, default);
        let zero = build.iconst(int, 0);
        build.ret(&[zero]);

        switches(&mut func);
        let text = printed(&func, &mut names);
        assert_eq!(text.matches("icmp eq").count(), 2, "two cases, two equality tests: {text}");
        assert!(!text.contains("icmp ule"), "and no range test over them: {text}");
    }

    /// The leaf size is a number and not an accident, so it is worth one test that says what it is
    /// for: at the size itself nothing is built, and one past it the search starts.
    #[test]
    fn the_leaf_size_is_where_the_search_starts() {
        let flat: Vec<i128> = (0..LINEAR as i128).map(|at| at * 5).collect();
        let mut walked = built(&flat);
        switches(&mut walked.func);
        assert!(
            !printed(&walked.func, &mut walked.names).contains("icmp slt"),
            "a leaf's worth of clusters is still a chain"
        );

        let one_more: Vec<i128> = (0..LINEAR as i128 + 1).map(|at| at * 5).collect();
        let mut split = built(&one_more);
        switches(&mut split.func);
        assert!(
            printed(&split.func, &mut split.names).contains("icmp slt"),
            "one more than a leaf splits"
        );
    }
}
