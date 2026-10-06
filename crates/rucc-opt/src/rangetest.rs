//! Range tests: comparisons of one value against constants, joined by `and` and `or`, become the
//! fewest tests of the set of values that passes.
//!
//! Section 19.4 of `spec/optimizer/19-reassociation-and-arithmetic.md`, in one block and across a
//! chain of them.
//! `x == 1 || x == 2 || x == 3 || x == 7` is four comparisons and three `or`s, and read as a set it
//! is `x` in `[1, 3]` or `x` is `7`. The first of those is `(unsigned)(x - 1) <= 2`, one subtract
//! and one comparison, and `c >= '0' && c <= '9'` is the same shape with an `and`.
//!
//! At `-O2` and `-O3` `short-circuit` has already turned the `||` into an `or` of bits in one block
//! by the time this pass runs, so what it sees is a tree like the ones `reassoc` sees: a root `and`
//! or `or` of type `i1`, and under it every operand of the same operation defined in the same block
//! and used by nothing else.
//!
//! # Sets
//!
//! A comparison of `x` against a constant is the set of values of `x` that make it true, kept as
//! sorted intervals of the unsigned reading of `x`. An unsigned comparison is one interval, and a
//! signed one is one or two, since the negative numbers are the top half of the unsigned ones. An
//! `or` of two comparisons of the same `x` is the union of their sets and an `and` is the
//! intersection, and a leaf of the tree that is itself an `and` or an `or` of comparisons of one
//! value, used by nothing else, is read the same way. That is what makes
//! `(c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')` two intervals of one value.
//!
//! # What comes out
//!
//! Each value's set is written back in whichever form is cheapest. An interval at either end of
//! the range is one comparison, and one in the middle is a subtract and an unsigned comparison.
//! When the set's complement has fewer intervals, the complement is tested and the answers are
//! joined with `and`, so `x != 3 && x != 4 && x != 5` is one test. A set with at least
//! `RANGE_TEST_BIT_INTERVALS` intervals spread over no more than 64 values is a bit test: one
//! comparison that the value is in the window, and one bit of a constant word picked by the value's
//! place in it. The shift amount is masked to six bits, so the shift means something whatever the
//! value is and the comparison in front decides alone when the value is outside the window. An
//! empty set is `false` and a full one is `true`.
//!
//! A value whose rewritten form is not cheaper than the comparisons it had is left as it was, and
//! comparisons of other values stay where they are in the tree.
//!
//! # Chains of branches
//!
//! At `-O1`, `-Os` and `-Oz` there is no `short-circuit`, so the comparisons are still a chain of
//! blocks, each asking about `x` and branching. The first block may hold anything. Every block
//! after it holds nothing but its comparisons and their constants, is reached only from the block
//! before it, and sends one of its two edges where all the others send one. That place is reached
//! when `x` is in the union of what each block sends there, so the first block can ask that once
//! and go straight to where the last block went otherwise, and nothing reaches the rest of the
//! chain. This is the cross block half of gcc's `optimize_range_tests`, which
//! `maybe_optimize_range_tests` at `gcc/tree-ssa-reassoc.cc` does. Asking the later comparisons on
//! a path that did not ask them is safe because a comparison of an integer with a constant cannot
//! trap and does nothing else.
//!
//! The chain is weighed as a tree is, by its comparisons, and written back in the same forms. A
//! chain the pass takes is one branch where it was several, which is what tamnd/rucc#3017 measured
//! missing at those levels.

use rucc_base::hash::{Map, Set};
use rucc_cost::heuristics::RANGE_TEST_BIT_INTERVALS;
use rucc_ir::{
    Block, BlockCall, Def, Extra, Func, Imm, Inst, InstData, IntPred, Opcode, Type, Value,
};

use crate::uses::{count, operands, substitute};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// What this pass is called, for the lists in [`crate::pipeline`] that name it.
pub const NAME: &str = "rangetest";

/// Recorded for comparisons of one value rewritten as interval tests.
const INTERVALS: &str = "comparisons of one value merged into interval tests";

/// Recorded for comparisons of one value rewritten as a bit test.
const BITS: &str = "comparisons of one value merged into a bit test";

/// Recorded for comparisons that decide nothing because their set is empty or everything.
const SETTLED: &str = "comparisons of one value that are always true or always false";

/// Recorded for a chain of branches on one value made one branch in the block it starts in.
const CHAINED: &str = "branches on comparisons of one value merged into one test";

/// Recorded for a tree that would have been rewritten if there had been fuel for it.
const NO_FUEL: &str = "comparisons left as they were, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeTest;

impl Pass for RangeTest {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "comparisons of one value against constants become the fewest tests of the set that passes"
    }

    fn preserves(&self) -> Preserved {
        // A chain merged into its first block takes the rest of the chain and its edges away, so
        // the graph is not the one anything was built on.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        chains(func, an, fuel, &mut stats);
        let uses = count(func);
        let mut inside: Set<Inst> = Set::default();
        let mut forward: Map<Value, Value> = Map::default();
        let mut dead: Vec<Inst> = Vec::new();
        for block in func.blocks().collect::<Vec<Block>>() {
            // Bottom up, so that the first instruction of a tree met is its root.
            for inst in func.insts_backwards(block).collect::<Vec<Inst>>() {
                if inside.contains(&inst) {
                    continue;
                }
                let Some(tree) = Tree::of(func, &uses, block, inst) else { continue };
                inside.extend(tree.interior.iter().copied());
                let Some(plan) = tree.plan(func, &uses, block) else { continue };
                // A tree of comparisons read into a set goes with this root, so it is not a root
                // of its own further up the block.
                inside.extend(plan.consumed.iter().copied());
                if !fuel.take() {
                    stats.missed(NO_FUEL);
                    continue;
                }
                let root = func[inst].first_result.expect("an and or an or has a result");
                let value = plan.build(func, inst, tree.op, &mut stats);
                forward.insert(root, value);
                dead.push(inst);
                dead.extend(tree.interior);
                dead.extend(plan.consumed);
            }
        }
        if forward.is_empty() {
            return stats;
        }
        substitute(func, &forward);
        sweep(func, dead);
        stats
    }
}

/// Removes every instruction in the list that nothing uses any more, over and over until none is
/// left, since an instruction can be the last user of another in the list.
pub(crate) fn sweep(func: &mut Func, mut dead: Vec<Inst>) {
    let mut uses = count(func);
    loop {
        let before = dead.len();
        dead.retain(|&inst| {
            let used = func[inst].results().any(|result| uses[result.index()] > 0);
            if used {
                return true;
            }
            operands(func, inst, |arg| uses[arg.index()] -= 1);
            func.remove_inst(inst);
            false
        });
        if dead.len() == before {
            return;
        }
    }
}

/// A root `and` or `or` of bits and what hangs under it.
struct Tree {
    op: Opcode,
    /// Every leaf in the order the tree has them, left to right.
    leaves: Vec<Value>,
    /// The instructions under the root that go when it does.
    interior: Vec<Inst>,
}

impl Tree {
    /// The tree whose root this is, when it is an `and` or an `or` of bits.
    fn of(func: &Func, uses: &[u32], block: Block, root: Inst) -> Option<Self> {
        let op = func[root].opcode;
        if op != Opcode::And && op != Opcode::Or {
            return None;
        }
        let result = func[root].first_result?;
        if func[result].ty != Type::I1 {
            return None;
        }
        let mut leaves = Vec::new();
        let mut interior = Vec::new();
        let mut stack = vec![result];
        while let Some(value) = stack.pop() {
            let under = match func[value].def {
                Def::Result { inst, .. } if value == result => Some(inst),
                Def::Result { inst, .. }
                    if uses[value.index()] == 1
                        && func.block_of(inst) == Some(block)
                        && func[inst].opcode == op =>
                {
                    Some(inst)
                }
                _ => None,
            };
            let Some(inst) = under else {
                leaves.push(value);
                continue;
            };
            if inst != root {
                interior.push(inst);
            }
            let args = &func[func[inst].args];
            stack.push(args[1]);
            stack.push(args[0]);
        }
        Some(Self { op, leaves, interior })
    }

    /// What the tree becomes, or nothing when no value's comparisons get cheaper.
    fn plan(&self, func: &Func, uses: &[u32], block: Block) -> Option<Plan> {
        let mut groups: Vec<Group> = Vec::new();
        let mut others: Vec<Value> = Vec::new();
        for &leaf in &self.leaves {
            let mut read = Read { func, uses, block, consumed: Vec::new(), compares: 0 };
            let Some((value, set)) = read.set(leaf) else {
                others.push(leaf);
                continue;
            };
            let group = match groups.iter_mut().position(|group| group.value == value) {
                Some(at) => &mut groups[at],
                None => {
                    let ty = func[value].ty;
                    let everything =
                        if self.op == Opcode::And { vec![(0, max(ty))] } else { Vec::new() };
                    groups.push(Group::new(value, ty, everything));
                    groups.last_mut().expect("just pushed")
                }
            };
            group.set = if self.op == Opcode::And {
                intersect(&group.set, &set, max(group.ty))
            } else {
                union(&group.set, &set)
            };
            group.leaves.push(leaf);
            group.consumed.extend(read.consumed);
            group.compares += read.compares;
        }
        let mut kept = others;
        let mut forms = Vec::new();
        let mut consumed = Vec::new();
        for group in groups {
            match group.form() {
                Some(form) => {
                    consumed.extend(group.consumed);
                    forms.push((group.value, group.ty, form));
                }
                None => kept.extend(group.leaves),
            }
        }
        (!forms.is_empty()).then_some(Plan { kept, forms, consumed })
    }
}

/// Reads a bit as a set of values of one integer, looking through the comparisons and trees of
/// them that nothing else uses.
pub(crate) struct Read<'a> {
    pub(crate) func: &'a Func,
    pub(crate) uses: &'a [u32],
    pub(crate) block: Block,
    /// The instructions the set was read through, which go when the tree is rewritten.
    pub(crate) consumed: Vec<Inst>,
    /// How many comparisons the set was read from.
    pub(crate) compares: u32,
}

impl Read<'_> {
    /// The value compared and the set of its values that make the bit true.
    ///
    /// A comparison something else also reads is read all the same and stays for that reader,
    /// while an `and` or an `or` under it is read only when this is its one use.
    pub(crate) fn set(&mut self, bit: Value) -> Option<(Value, Vec<(u64, u64)>)> {
        let func = self.func;
        let Def::Result { inst, .. } = func[bit].def else { return None };
        if func.block_of(inst) != Some(self.block) || func[bit].ty != Type::I1 {
            return None;
        }
        let only = self.uses[bit.index()] == 1;
        let data = &func[inst];
        let args = &func[data.args];
        match data.opcode {
            Opcode::ICmp => {
                let Extra::IntPred(pred) = data.extra else { return None };
                let (value, number, pred) = match (integer(func, args[0]), integer(func, args[1])) {
                    (None, Some(number)) => (args[0], number, pred),
                    (Some(number), None) => (args[1], number, pred.swapped()),
                    _ => return None,
                };
                let ty = func[value].ty;
                if !ty.is_int() || ty.lanes() != 1 || ty.bits() == 0 || ty.bits() > 64 {
                    return None;
                }
                if only {
                    self.consumed.push(inst);
                }
                self.compares += 1;
                Some((value, compare(pred, number, ty)))
            }
            Opcode::And | Opcode::Or if only => {
                let (left, one) = self.set(args[0])?;
                let (right, two) = self.set(args[1])?;
                if left != right {
                    return None;
                }
                self.consumed.push(inst);
                let ty = func[left].ty;
                let set = if data.opcode == Opcode::And {
                    intersect(&one, &two, max(ty))
                } else {
                    union(&one, &two)
                };
                Some((left, set))
            }
            _ => None,
        }
    }
}

/// The comparisons of one value in a tree.
struct Group {
    value: Value,
    ty: Type,
    set: Vec<(u64, u64)>,
    leaves: Vec<Value>,
    consumed: Vec<Inst>,
    compares: u32,
}

impl Group {
    fn new(value: Value, ty: Type, set: Vec<(u64, u64)>) -> Self {
        Self { value, ty, set, leaves: Vec::new(), consumed: Vec::new(), compares: 0 }
    }

    /// How the set is best tested, when that is cheaper than what the tree has now.
    fn form(&self) -> Option<Form> {
        let top = max(self.ty);
        if self.set.is_empty() {
            return Some(Form::Constant(false));
        }
        if self.set == [(0, top)] {
            return Some(Form::Constant(true));
        }
        // Each comparison of the tree and the `and` or `or` that joins it to the next.
        let had = 2 * self.compares - 1;
        let outside = complement(&self.set, top);
        let (lo, hi) = (self.set[0].0, self.set[self.set.len() - 1].1);
        let fewest = self.set.len().min(outside.len());
        let one_word = hi - lo < 64; // not a threshold: one bit for each number in a `u64`.
        if fewest >= RANGE_TEST_BIT_INTERVALS && one_word && self.compares as usize > fewest {
            let mut word = 0u64;
            for &(from, to) in &self.set {
                for number in from..=to {
                    word |= 1 << (number - lo);
                }
            }
            return Some(Form::Bits { base: lo, span: hi - lo, word });
        }
        let (negated, intervals) = if outside.len() < self.set.len() {
            (true, outside)
        } else {
            (false, self.set.clone())
        };
        let cost = intervals
            .iter()
            .map(|&(from, to)| 1 + u32::from(from != 0 && to != top && from != to))
            .sum::<u32>()
            + intervals.len() as u32
            - 1;
        (cost < had).then_some(Form::Intervals { negated, intervals })
    }
}

/// How the set of one value is tested.
enum Form {
    /// Always this.
    Constant(bool),
    /// In one of these intervals, or with `negated`, in none of them.
    Intervals { negated: bool, intervals: Vec<(u64, u64)> },
    /// Within `span` of `base`, and the bit of `word` at the distance from `base` is set.
    Bits { base: u64, span: u64, word: u64 },
}

/// A tree as it will be rebuilt.
struct Plan {
    /// The leaves that stay as they were.
    kept: Vec<Value>,
    /// The values whose comparisons are rewritten, and how.
    forms: Vec<(Value, Type, Form)>,
    /// The comparisons and trees of them read into the sets, which go with the root.
    consumed: Vec<Inst>,
}

impl Plan {
    /// Writes the tree in front of its old root and answers the bit it comes to.
    fn build(&self, func: &mut Func, before: Inst, op: Opcode, stats: &mut Stats) -> Value {
        let mut bits = self.kept.clone();
        for (value, ty, form) in &self.forms {
            stats.optimized(form.said());
            bits.push(test(func, before, *value, *ty, form));
        }
        let mut acc = bits[0];
        for &bit in &bits[1..] {
            acc = binary(func, before, op, acc, bit, Type::I1);
        }
        acc
    }
}

impl Form {
    /// What is recorded for a tree written in this form.
    const fn said(&self) -> &'static str {
        match self {
            Self::Constant(_) => SETTLED,
            Self::Intervals { .. } => INTERVALS,
            Self::Bits { .. } => BITS,
        }
    }
}

/// The bit that is true when `value` is in the set the form tests, written in front of `before`.
fn test(func: &mut Func, before: Inst, value: Value, ty: Type, form: &Form) -> Value {
    match form {
        Form::Constant(yes) => constant(func, before, Type::I1, i128::from(*yes)),
        Form::Intervals { negated, intervals } => {
            let join = if *negated { Opcode::And } else { Opcode::Or };
            let mut acc: Option<Value> = None;
            for &(from, to) in intervals {
                let test = interval(func, before, value, ty, from, to, *negated);
                acc = Some(match acc {
                    None => test,
                    Some(had) => binary(func, before, join, had, test, Type::I1),
                });
            }
            acc.expect("a set that is not empty has an interval")
        }
        Form::Bits { base, span, word } => bit_test(func, before, value, ty, *base, *span, *word),
    }
}

/// Merges every chain of branches on one value into the block it starts in.
fn chains(func: &mut Func, an: &mut Analyses, fuel: &mut Fuel, stats: &mut Stats) {
    if func.entry().is_none() {
        return;
    }
    // From the top down, so that a chain is met at its first block and not partway along it.
    let order: Vec<Block> = an.cfg(func).reverse_postorder().collect();
    // A block whose address is taken can be reached by a jump nobody can see, so it is never one
    // to take away.
    let addressed = crate::copy::addressed(func);
    let mut uses = count(func);
    let mut into = incoming(func);
    let mut gone: Set<Block> = Set::default();
    for head in order {
        if gone.contains(&head) {
            continue;
        }
        let known = Known { uses: &uses, into: &into, addressed: &addressed };
        let Some(chain) = Chain::of(func, &known, head) else { continue };
        if !fuel.take() {
            stats.missed(NO_FUEL);
            return;
        }
        gone.extend(chain.rest.iter().copied());
        chain.write(func);
        stats.optimized(CHAINED);
        uses = count(func);
        into = incoming(func);
    }
}

/// How many edges come into each block.
fn incoming(func: &Func) -> Map<Block, u32> {
    let mut into: Map<Block, u32> = Map::default();
    for block in func.blocks() {
        let Some(term) = func.terminator(block) else { continue };
        for call in func.successors(term) {
            *into.entry(call.block).or_insert(0) += 1;
        }
    }
    into
}

/// What is known about the function while a chain is looked for.
struct Known<'a> {
    uses: &'a [u32],
    into: &'a Map<Block, u32>,
    addressed: &'a Set<Block>,
}

/// A chain of branches on one value, from the block it starts in.
struct Chain {
    head: Block,
    /// The blocks after the head, which nothing reaches once the head asks the whole question.
    rest: Vec<Block>,
    /// Which edge of the head's branch every block of the chain has one of, the first or the
    /// second.
    common: usize,
    /// Where the last block went when the value was not in the set, which is where the head's
    /// other edge goes now.
    out: BlockCall,
    value: Value,
    ty: Type,
    form: Form,
    /// The comparisons the head's own condition was read from, which go when nothing reads them.
    consumed: Vec<Inst>,
}

impl Chain {
    /// The chain that starts in this block, when there is one and one test is cheaper than its
    /// comparisons.
    fn of(func: &Func, known: &Known<'_>, head: Block) -> Option<Self> {
        let term = func.terminator(head)?;
        if func[term].opcode != Opcode::BrIf {
            return None;
        }
        let edges: Vec<BlockCall> = func.successors(term).collect();
        let mut read =
            Read { func, uses: known.uses, block: head, consumed: Vec::new(), compares: 0 };
        let (value, first) = read.set(func[func[term].args][0])?;
        let ty = func[value].ty;
        let top = max(ty);
        for common in 0..2 {
            let target = edges[common];
            // The values that take the edge every block of the chain has.
            let mut set = if common == 0 { first.clone() } else { complement(&first, top) };
            let mut compares = read.compares;
            let mut rest: Vec<Block> = Vec::new();
            let mut next = edges[1 - common];
            loop {
                if next.block == head || next.block == target.block || rest.contains(&next.block) {
                    break;
                }
                let Some(Link { set: more, compares: asked, other: out }) =
                    link(func, known, next, value, target)
                else {
                    break;
                };
                set = union(&set, &more);
                compares += asked;
                rest.push(next.block);
                next = out;
            }
            if rest.is_empty() {
                continue;
            }
            // The branch takes its first edge when its bit is true, so the bit is the set that
            // goes that way.
            let taken = if common == 0 { set } else { complement(&set, top) };
            let mut group = Group::new(value, ty, taken);
            group.compares = compares;
            let Some(form) = group.form() else { continue };
            let consumed = read.consumed.clone();
            return Some(Self { head, rest, common, out: next, value, ty, form, consumed });
        }
        None
    }

    /// Asks the whole question in the head, sends its other edge where the chain ended, and takes
    /// the rest of the chain away.
    fn write(self, func: &mut Func) {
        let term = func.terminator(self.head).expect("the head of a chain ends in its branch");
        let bit = test(func, term, self.value, self.ty, &self.form);
        let args = func[term].args;
        func.rewrite(args, |_| bit);
        let at =
            func.target_list(term).iter().nth(1 - self.common).expect("a branch has two edges");
        func.set_block_call(at, self.out);
        for block in self.rest {
            func.remove_block(block);
        }
        sweep(func, self.consumed);
    }
}

/// What one block after the head adds to a chain.
#[derive(Debug)]
struct Link {
    /// The values of the chain's value that take the edge the chain leaves by.
    set: Vec<(u64, u64)>,
    /// How many comparisons that was read from.
    compares: u32,
    /// Where the block goes otherwise, which is the next link if there is one.
    other: BlockCall,
}

/// The [`Link`] that the block `call` reaches is, if it is one.
///
/// The block has to be reached only by `call`, take nothing, end in a branch with one edge the
/// same as `common`, and hold nothing but comparisons of `value` and their constants, none of them
/// read anywhere else. Then nothing is lost when it goes.
fn link(
    func: &Func,
    known: &Known<'_>,
    call: BlockCall,
    value: Value,
    common: BlockCall,
) -> Option<Link> {
    let block = call.block;
    if known.into.get(&block).copied() != Some(1)
        || known.addressed.contains(&block)
        || !func[block].params.is_empty()
    {
        return None;
    }
    let term = func.terminator(block)?;
    if func[term].opcode != Opcode::BrIf {
        return None;
    }
    let edges: Vec<BlockCall> = func.successors(term).collect();
    let same =
        |edge: &BlockCall| edge.block == common.block && func[edge.args] == func[common.args];
    let at = match (same(&edges[0]), same(&edges[1])) {
        (true, false) => 0,
        (false, true) => 1,
        _ => return None,
    };
    let mut read = Read { func, uses: known.uses, block, consumed: Vec::new(), compares: 0 };
    let (asked, set) = read.set(func[func[term].args][0])?;
    if asked != value {
        return None;
    }
    let mut inside: Map<Value, u32> = Map::default();
    for inst in func.insts(block) {
        for &arg in &func[func[inst].args] {
            *inside.entry(arg).or_insert(0) += 1;
        }
    }
    for inst in func.insts(block) {
        if func.is_terminator(inst) {
            continue;
        }
        if func[inst].opcode != Opcode::IConst && !read.consumed.contains(&inst) {
            return None;
        }
        // Every use inside the block, and none on an edge or anywhere else.
        if func[inst]
            .results()
            .any(|result| inside.get(&result).copied().unwrap_or(0) != known.uses[result.index()])
        {
            return None;
        }
    }
    let set = if at == 0 { set } else { complement(&set, max(func[value].ty)) };
    Some(Link { set, compares: read.compares, other: edges[1 - at] })
}

/// Whether `value` is in `[from, to]`, or with `negated`, whether it is not.
fn interval(
    func: &mut Func,
    before: Inst,
    value: Value,
    ty: Type,
    from: u64,
    to: u64,
    negated: bool,
) -> Value {
    let top = max(ty);
    let (pred, lhs, number) = if from == to {
        (IntPred::Eq, value, from)
    } else if from == 0 {
        (IntPred::Ule, value, to)
    } else if to == top {
        (IntPred::Uge, value, from)
    } else {
        let base = constant(func, before, ty, i128::from(from));
        let moved = binary(func, before, Opcode::Sub, value, base, ty);
        (IntPred::Ule, moved, to - from)
    };
    let pred = if negated { pred.inverse() } else { pred };
    let number = constant(func, before, ty, i128::from(number));
    icmp(func, before, pred, lhs, number)
}

/// Whether `value` is within `span` of `base` and the bit of `word` at its distance from `base`
/// is set.
fn bit_test(
    func: &mut Func,
    before: Inst,
    value: Value,
    ty: Type,
    base: u64,
    span: u64,
    word: u64,
) -> Value {
    let moved = if base == 0 {
        value
    } else {
        let base = constant(func, before, ty, i128::from(base));
        binary(func, before, Opcode::Sub, value, base, ty)
    };
    let limit = constant(func, before, ty, i128::from(span));
    let within = icmp(func, before, IntPred::Ule, moved, limit);
    let wide = Type::int(64);
    let distance = if ty.bits() < 64 {
        let args = func.push_values(&[moved]);
        emit(func, before, InstData { args, ..InstData::new(Opcode::ZExt) }, wide)
    } else {
        moved
    };
    // Six bits of the distance, so the shift is in range whatever the value is. When the value is
    // outside the window `within` is false and this bit does not matter.
    let six = constant(func, before, wide, 63);
    let distance = binary(func, before, Opcode::And, distance, six, wide);
    let word = constant(func, before, wide, i128::from(word));
    let shifted = binary(func, before, Opcode::LShr, word, distance, wide);
    let one = constant(func, before, wide, 1);
    let low = binary(func, before, Opcode::And, shifted, one, wide);
    let zero = constant(func, before, wide, 0);
    let set = icmp(func, before, IntPred::Ne, low, zero);
    binary(func, before, Opcode::And, within, set, Type::I1)
}

/// The largest unsigned value of the type.
pub(crate) fn max(ty: Type) -> u64 {
    u64::MAX >> (64 - ty.bits())
}

/// The values of `x` of type `ty` for which `x pred number` holds.
fn compare(pred: IntPred, number: i128, ty: Type) -> Vec<(u64, u64)> {
    let top = max(ty);
    let unsigned = (number as u64) & top;
    let bits = ty.bits();
    let smin = -(1i128 << (bits - 1));
    let smax = (1i128 << (bits - 1)) - 1;
    let signed = Imm::int(number, ty).signed(ty);
    match pred {
        IntPred::Eq => vec![(unsigned, unsigned)],
        IntPred::Ne => complement(&[(unsigned, unsigned)], top),
        IntPred::Ult if unsigned == 0 => Vec::new(),
        IntPred::Ult => vec![(0, unsigned - 1)],
        IntPred::Ule => vec![(0, unsigned)],
        IntPred::Ugt if unsigned == top => Vec::new(),
        IntPred::Ugt => vec![(unsigned + 1, top)],
        IntPred::Uge => vec![(unsigned, top)],
        IntPred::Slt if signed == smin => Vec::new(),
        IntPred::Slt => signed_interval(smin, signed - 1, ty),
        IntPred::Sle => signed_interval(smin, signed, ty),
        IntPred::Sgt if signed == smax => Vec::new(),
        IntPred::Sgt => signed_interval(signed + 1, smax, ty),
        IntPred::Sge => signed_interval(signed, smax, ty),
    }
}

/// The signed interval `[from, to]` of `ty` read as unsigned, which is two intervals when it
/// crosses zero.
fn signed_interval(from: i128, to: i128, ty: Type) -> Vec<(u64, u64)> {
    let top = max(ty);
    let wrap = |number: i128| (number as u64) & top;
    if from >= 0 || to < 0 {
        vec![(wrap(from), wrap(to))]
    } else {
        union(&[(0, wrap(to))], &[(wrap(from), top)])
    }
}

/// The intervals sorted, with the ones that overlap or touch made one.
fn union(one: &[(u64, u64)], two: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut all: Vec<(u64, u64)> = one.iter().chain(two).copied().collect();
    all.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (from, to) in all {
        match out.last_mut() {
            Some(last) if last.1 == u64::MAX || from <= last.1 + 1 => last.1 = last.1.max(to),
            _ => out.push((from, to)),
        }
    }
    out
}

/// The values up to `top` in none of the intervals, which must be sorted and apart.
pub(crate) fn complement(set: &[(u64, u64)], top: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut next = Some(0u64);
    for &(from, to) in set {
        let Some(start) = next else { break };
        if from > start {
            out.push((start, from - 1));
        }
        next = if to == top { None } else { Some(to + 1) };
    }
    if let Some(start) = next {
        out.push((start, top));
    }
    out
}

fn intersect(one: &[(u64, u64)], two: &[(u64, u64)], top: u64) -> Vec<(u64, u64)> {
    complement(&union(&complement(one, top), &complement(two, top)), top)
}

/// The value of an integer constant, read with its own sign.
fn integer(func: &Func, value: Value) -> Option<i128> {
    crate::discharge::constant(func, value)
}

fn constant(func: &mut Func, before: Inst, ty: Type, value: i128) -> Value {
    let at = func.add_imm(Imm::int(value, ty));
    emit(func, before, InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) }, ty)
}

fn icmp(func: &mut Func, before: Inst, pred: IntPred, a: Value, b: Value) -> Value {
    let args = func.push_values(&[a, b]);
    emit(
        func,
        before,
        InstData { args, extra: Extra::IntPred(pred), ..InstData::new(Opcode::ICmp) },
        Type::I1,
    )
}

fn binary(func: &mut Func, before: Inst, opcode: Opcode, a: Value, b: Value, ty: Type) -> Value {
    let args = func.push_values(&[a, b]);
    emit(func, before, InstData { args, ..InstData::new(opcode) }, ty)
}

fn emit(func: &mut Func, before: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::Type;

    use super::{RangeTest, compare, complement, intersect, union};
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it under this much fuel.
    fn run(body: &str, fuel: &mut Fuel) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses();
            RangeTest.run(&mut module[id], &mut an, fuel);
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    fn cleaned(body: &str) -> String {
        run(body, &mut Fuel::unlimited())
    }

    fn count(out: &str, opcode: &str) -> usize {
        out.lines().filter(|line| line.contains(&format!("= {opcode}"))).count()
    }

    /// The value of `x pred k` for every `x` of eight bits, by the sets and by arithmetic.
    #[test]
    fn every_eight_bit_comparison_is_its_set() {
        use rucc_ir::IntPred::*;
        let ty = Type::int(8);
        for pred in [Eq, Ne, Slt, Sle, Sgt, Sge, Ult, Ule, Ugt, Uge] {
            for k in 0..256i128 {
                let set = compare(pred, k, ty);
                for x in 0..256u64 {
                    let (sx, sk) = (x as u8 as i8, k as u8 as i8);
                    let (ux, uk) = (x as u8, k as u8);
                    let want = match pred {
                        Eq => ux == uk,
                        Ne => ux != uk,
                        Slt => sx < sk,
                        Sle => sx <= sk,
                        Sgt => sx > sk,
                        Sge => sx >= sk,
                        Ult => ux < uk,
                        Ule => ux <= uk,
                        Ugt => ux > uk,
                        Uge => ux >= uk,
                    };
                    let got = set.iter().any(|&(from, to)| from <= x && x <= to);
                    assert_eq!(got, want, "{pred:?} {k} at {x}, {set:?}");
                }
            }
        }
    }

    /// Union, intersection and complement agree with the membership they stand for, over every
    /// pair of sets of up to two intervals of four bits.
    #[test]
    fn the_set_operations_are_the_membership_they_stand_for() {
        let top = 15u64;
        let mut sets: Vec<Vec<(u64, u64)>> = vec![Vec::new()];
        for a in 0..=top {
            for b in a..=top {
                sets.push(vec![(a, b)]);
                for c in b + 2..=top {
                    sets.push(vec![(a, b), (c, top)]);
                }
            }
        }
        let has = |set: &[(u64, u64)], x: u64| set.iter().any(|&(from, to)| from <= x && x <= to);
        for one in &sets {
            let outside = complement(one, top);
            for two in sets.iter().step_by(7) {
                let either = union(one, two);
                let both = intersect(one, two, top);
                for x in 0..=top {
                    assert_eq!(has(&either, x), has(one, x) || has(two, x));
                    assert_eq!(has(&both, x), has(one, x) && has(two, x));
                    assert_eq!(has(&outside, x), !has(one, x));
                }
                assert!(either.windows(2).all(|pair| pair[0].1 + 1 < pair[1].0), "{either:?}");
            }
        }
    }

    const FOUR: &str = r#"
func @f(i32) -> i1, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = icmp eq %0, %1
    %3 = iconst.i32 2
    %4 = icmp eq %0, %3
    %5 = or %2, %4
    %6 = iconst.i32 3
    %7 = icmp eq %0, %6
    %8 = or %5, %7
    %9 = iconst.i32 7
    %10 = icmp eq %0, %9
    %11 = or %8, %10
    return %11
}
"#;

    /// `x == 1 || x == 2 || x == 3 || x == 7` is `x - 1 <= 2` unsigned, or `x == 7`.
    #[test]
    fn equalities_that_touch_are_one_interval() {
        let out = cleaned(FOUR);
        assert_eq!(count(&out, "icmp"), 2, "{out}");
        assert!(out.contains("icmp ule"), "{out}");
        assert!(out.contains("icmp eq"), "{out}");
        assert_eq!(count(&out, "or"), 1, "{out}");
    }

    /// `c >= 48 && c <= 57` is one unsigned comparison of `c - 48`, and
    /// `x != 3 && x != 4 && x != 5` is the complement of one interval.
    #[test]
    fn an_and_is_an_intersection_and_a_complement_can_be_cheaper() {
        let out = cleaned(
            r#"
func @f(i32) -> i1, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 48
    %2 = icmp sge %0, %1
    %3 = iconst.i32 57
    %4 = icmp sle %0, %3
    %5 = and %2, %4
    return %5
}

func @g(i32) -> i1, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 3
    %2 = icmp ne %0, %1
    %3 = iconst.i32 4
    %4 = icmp ne %0, %3
    %5 = and %2, %4
    %6 = iconst.i32 5
    %7 = icmp ne %0, %6
    %8 = and %5, %7
    return %8
}
"#,
        );
        assert_eq!(count(&out, "icmp"), 2, "{out}");
        assert!(out.contains("icmp ule") && out.contains("icmp ugt"), "{out}");
        assert!(out.contains("iconst.i32 9") && out.contains("iconst.i32 2"), "{out}");
        assert_eq!(count(&out, "and"), 0, "{out}");
    }

    /// Two ranges of one value, each an `and` under the `or`, are read as one set.
    #[test]
    fn a_nested_tree_of_one_value_is_read_as_its_set() {
        let out = cleaned(
            r#"
func @f(i32) -> i1, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 97
    %2 = icmp sge %0, %1
    %3 = iconst.i32 122
    %4 = icmp sle %0, %3
    %5 = and %2, %4
    %6 = iconst.i32 65
    %7 = icmp sge %0, %6
    %8 = iconst.i32 90
    %9 = icmp sle %0, %8
    %10 = and %7, %9
    %11 = or %5, %10
    return %11
}
"#,
        );
        assert_eq!(count(&out, "icmp"), 2, "{out}");
        assert_eq!(count(&out, "sub"), 2, "{out}");
    }

    /// Four values spread over less than a word are one window test and one bit.
    #[test]
    fn scattered_values_in_one_word_are_a_bit_test() {
        let out = cleaned(
            r#"
func @f(i32) -> i1, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 32
    %2 = icmp eq %0, %1
    %3 = iconst.i32 9
    %4 = icmp eq %0, %3
    %5 = or %2, %4
    %6 = iconst.i32 10
    %7 = icmp eq %0, %6
    %8 = or %5, %7
    %9 = iconst.i32 13
    %10 = icmp eq %0, %9
    %11 = or %8, %10
    return %11
}
"#,
        );
        assert_eq!(count(&out, "lshr"), 1, "{out}");
        assert!(out.contains("iconst.i32 23"), "the window is 9 to 32, {out}");
        // Bits 0, 1, 4 and 23 of the window starting at 9.
        assert!(out.contains(&format!("iconst.i64 {}", (1u64 << 23) | (1 << 4) | 3)), "{out}");
    }

    /// Comparisons of another value, and a comparison shared with something else, stay.
    #[test]
    fn other_values_and_shared_comparisons_stay() {
        let out = cleaned(
            r#"
func @use(i1), linkage(external);

func @f(i32, i32) -> i1, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 1
    %3 = icmp eq %0, %2
    call @use(%3) : (i1)
    %4 = iconst.i32 2
    %5 = icmp eq %0, %4
    %6 = or %3, %5
    %7 = icmp eq %1, %2
    %8 = or %6, %7
    %9 = iconst.i32 3
    %10 = icmp eq %0, %9
    %11 = or %8, %10
    return %11
}
"#,
        );
        assert!(out.contains("%3 = icmp eq %0, %2"), "{out}");
        assert!(out.contains("icmp eq %1, %2"), "{out}");
        assert!(out.contains("icmp ule"), "{out}");
    }

    /// `x < 3 && x > 5` is nothing, and `x < 5 || x >= 3` is everything.
    #[test]
    fn an_empty_set_is_false_and_a_full_one_is_true() {
        let out = cleaned(
            r#"
func @f(i32) -> i1, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 3
    %2 = icmp slt %0, %1
    %3 = iconst.i32 5
    %4 = icmp sgt %0, %3
    %5 = and %2, %4
    return %5
}

func @g(i32) -> i1, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 5
    %2 = icmp slt %0, %1
    %3 = iconst.i32 3
    %4 = icmp sge %0, %3
    %5 = or %2, %4
    return %5
}
"#,
        );
        assert_eq!(count(&out, "icmp"), 0, "{out}");
        assert!(out.contains("iconst.i1 0") && out.contains("iconst.i1 -1"), "{out}");
    }

    /// Two equalities far apart are already as cheap as they get.
    #[test]
    fn two_points_apart_are_left_alone() {
        let body = r#"
func @f(i32) -> i1, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = icmp eq %0, %1
    %3 = iconst.i32 9
    %4 = icmp eq %0, %3
    %5 = or %2, %4
    return %5
}
"#;
        let out = cleaned(body);
        assert!(out.contains("%5 = or %2, %4"), "{out}");
    }

    /// `x == 1 || x == 2 || x == 3` at `-O1`, once `simplify-cfg` has merged what `thread` left: three
    /// blocks, each comparing and branching to the call when it holds.
    const CHAIN: &str = r#"
func @hit(), linkage(external);

func @f(i32), linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = icmp eq %0, %1
    %3 = iconst.i1 -1
    br_if %2, block3, block1

block1:
    %4 = iconst.i32 2
    %5 = icmp eq %0, %4
    %6 = iconst.i1 -1
    br_if %5, block3, block2

block2:
    %7 = iconst.i32 3
    %8 = icmp eq %0, %7
    br_if %8, block3, block4

block3:
    call @hit() : ()
    jump block4

block4:
    return
}
"#;

    fn branches(out: &str) -> usize {
        out.lines().filter(|line| line.trim_start().starts_with("br_if")).count()
    }

    /// How many blocks there are, which the printer numbers again from zero once some have gone.
    fn blocks(out: &str) -> usize {
        out.lines().filter(|line| line.starts_with("block")).count()
    }

    #[test]
    fn a_chain_of_branches_is_one_test_in_its_first_block() {
        let out = cleaned(CHAIN);
        assert_eq!(branches(&out), 1, "{out}");
        assert_eq!(count(&out, "icmp"), 1, "{out}");
        assert!(out.contains("icmp ule") && out.contains("= sub %0"), "{out}");
        assert_eq!(blocks(&out), 3, "{out}");
        assert!(out.contains(", block1, block2"), "{out}");
    }

    /// `c >= '0' && c <= '9'` sends the other way round: every block leaves for the end when its
    /// comparison fails, so the set is the one that reaches the call.
    #[test]
    fn a_chain_that_leaves_when_a_comparison_fails_is_its_intersection() {
        let out = cleaned(
            r#"
func @hit(), linkage(external);

func @f(i32), linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 48
    %2 = icmp sge %0, %1
    br_if %2, block1, block3

block1:
    %3 = iconst.i32 57
    %4 = icmp sle %0, %3
    br_if %4, block2, block3

block2:
    call @hit() : ()
    jump block3

block3:
    return
}
"#,
        );
        assert_eq!(branches(&out), 1, "{out}");
        assert!(out.contains("icmp ule") && out.contains("iconst.i32 9"), "{out}");
        assert_eq!(blocks(&out), 3, "{out}");
        assert!(out.contains(", block1, block2"), "{out}");
    }

    /// `x != 3 && x != 4 && x != 5` leaves for the end on each equality, and the values that reach
    /// the call are everything outside `[3, 5]`, which is one test of the complement.
    #[test]
    fn a_chain_of_inequalities_is_one_test_of_what_they_leave_out() {
        let out = cleaned(
            r#"
func @hit(), linkage(external);

func @f(i32), linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 3
    %2 = icmp ne %0, %1
    br_if %2, block1, block4

block1:
    %3 = iconst.i32 4
    %4 = icmp ne %0, %3
    br_if %4, block2, block4

block2:
    %5 = iconst.i32 5
    %6 = icmp ne %0, %5
    br_if %6, block3, block4

block3:
    call @hit() : ()
    jump block4

block4:
    return
}
"#,
        );
        assert_eq!(branches(&out), 1, "{out}");
        assert_eq!(count(&out, "icmp"), 1, "{out}");
        assert!(out.contains("icmp ugt") && out.contains("iconst.i32 2"), "{out}");
        assert_eq!(blocks(&out), 3, "{out}");
        assert!(out.contains(", block1, block2"), "{out}");
    }

    /// A block that does something else, one that something else also reaches, one that hands the
    /// common block another argument, and one asking about another value all stop the chain.
    #[test]
    fn a_block_that_is_more_than_a_comparison_ends_the_chain() {
        let body = r#"
func @hit(), linkage(external);

func @busy(i32), linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = icmp eq %0, %1
    br_if %2, block2, block1

block1:
    call @hit() : ()
    %3 = iconst.i32 2
    %4 = icmp eq %0, %3
    br_if %4, block2, block3

block2:
    call @hit() : ()
    jump block3

block3:
    return
}

func @shared(i32, i1), linkage(external) {
block0(%0: i32, %1: i1):
    br_if %1, block2, block1

block1:
    %2 = iconst.i32 1
    %3 = icmp eq %0, %2
    br_if %3, block4, block2

block2:
    %4 = iconst.i32 2
    %5 = icmp eq %0, %4
    br_if %5, block4, block3

block3:
    return

block4:
    call @hit() : ()
    return
}

func @args(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = icmp eq %0, %1
    %3 = iconst.i32 7
    br_if %2, block2(%3), block1

block1:
    %4 = iconst.i32 2
    %5 = icmp eq %0, %4
    %6 = iconst.i32 8
    br_if %5, block2(%6), block3

block2(%7: i32):
    return %7

block3:
    %8 = iconst.i32 0
    return %8
}

func @other(i32, i32), linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 1
    %3 = icmp eq %0, %2
    br_if %3, block2, block1

block1:
    %4 = iconst.i32 2
    %5 = icmp eq %1, %4
    br_if %5, block2, block3

block2:
    call @hit() : ()
    jump block3

block3:
    return
}
"#;
        let out = cleaned(body);
        assert_eq!(branches(&out), 9, "{out}");
        assert_eq!(count(&out, "icmp"), 8, "{out}");
    }

    #[test]
    fn fuel_stops_a_chain() {
        let none = run(CHAIN, &mut Fuel::of(0));
        assert_eq!(branches(&none), 3, "{none}");
    }

    #[test]
    fn fuel_stops_the_rewriting() {
        let none = run(FOUR, &mut Fuel::of(0));
        assert_eq!(count(&none, "icmp"), 4, "{none}");
        let one = run(FOUR, &mut Fuel::of(1));
        assert_eq!(count(&one, "icmp"), 2, "{one}");
    }
}
