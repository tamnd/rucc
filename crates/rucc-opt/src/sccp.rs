//! Conditional constant propagation, over the bits of each value that are known.
//!
//! Design: `spec/optimizer/14-constant-propagation.md`, and tamnd/rucc#2339.
//!
//! ```c
//! int x = 1, done = 0;
//! for (int i = 0; i < n; i++) {
//!     if (x != 1) done = 1;
//!     x = 1;
//! }
//! return done;
//! ```
//!
//! After scalar replacement `x` and `done` are parameters of the loop header, each fed by the entry
//! and by the latch. Every other pass looks at one of them and sees a parameter fed partly by
//! itself, and a parameter fed by itself could be anything. This pass starts from the other end: it
//! assumes every block is unreachable and every value undefined, and only gives that up where the
//! function forces it to. `x` is one on the way in and one on the way round, so `x != 1` is false,
//! so the edge that sets `done` is never taken, so `done` is zero on both of the edges that are.
//! Iterating the fold and the branch removal to a fixpoint does not get there, because both start
//! from the assumption that every edge is taken and never have a reason to drop it.
//!
//! # Bits, not constants
//!
//! What is known about a value is [`Bits`], the same known bits the range analysis keeps beside its
//! intervals, and a constant is the case where all of them are known. Section 14.2 says this is
//! gcc's design and why it is the better one: that a value is even, or fits in eight bits, is a fact
//! a constant cannot hold and a later comparison or mask can use. It is also what keeps the fixpoint
//! short. Each fact only ever loses known bits, so a value changes at most once for each bit it has,
//! where an interval can grow by one on every trip round a loop.
//!
//! The arithmetic is the table in [`crate::range::ops`], asked with the operands turned into ranges
//! that know nothing but their bits. Section 14.4 says the pass should not contain arithmetic of its
//! own, and the ranges already have every operation this reads, with the flags taken into account.
//!
//! # The fixpoint and what is done with it
//!
//! Two worklists, as Wegman and Zadeck describe it: blocks that have just become reachable, every
//! instruction in which is looked at, and values whose fact just changed, every reader of which is
//! looked at again. A branch whose condition is known sends only the edge it takes, and a block
//! parameter is what the reachable edges into its block pass.
//!
//! A branch on a value still undefined when the lists run dry is a branch the analysis never
//! decided, and assuming neither of its edges is taken would be concluding something from nothing.
//! Every such branch has all its edges taken and the fixpoint resumes, which is what LLVM does about
//! the same thing. Since there is no instruction in the IR that makes an undefined value, the only
//! way to get one is a cycle nothing enters, and a branch in one of those is unreachable anyway.
//!
//! Section 14.6 says fuel gates the substitution and not the fixpoint, because a fixpoint stopped
//! early holds assumptions nothing has checked yet. So the fixpoint always runs to the end, and then
//! each value whose bits are all known becomes a constant for as long as the fuel lasts. No block is
//! deleted and no branch is rewritten here: a branch whose condition became a constant is a fold for
//! `prune` and `simplify-cfg`, which run after this, as section 06.5 has it.

use rucc_base::hash::{Map, Set};
use rucc_ir::{
    Block, BlockCall, Extra, Flags, Func, Imm, Inst, InstData, Opcode, Type, Value, ValueList,
};

use crate::range::ops::{self, Truth};
use crate::range::{Bits, Range};
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats, uses};

/// What the pass calls itself, which is what `-fdump-ir=after-sccp` spells.
pub const NAME: &str = "sccp";

/// Recorded once for each value that became a constant.
const REPLACED: &str = "value proved constant and replaced";

/// Recorded for a value proved constant after the fuel ran out.
const NO_FUEL: &str = "value proved constant and not replaced, the pass ran out of fuel";

/// Recorded for each branch edge the fixpoint never took.
const DEAD_EDGE: &str = "branch edge proved never taken";

/// Recorded for each branch on a value the fixpoint never settled, which then takes every edge.
const UNDECIDED: &str = "branch on an undefined value given all its edges";

/// The pass. It holds nothing, because everything it knows it works out from the function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sccp;

impl Pass for Sccp {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a value that is the same on every path the function can take becomes a constant"
    }

    fn preserves(&self) -> Preserved {
        // Constants where instructions were and at the top of blocks, and no edge touched. The
        // operands the replaced instructions read are read by fewer things now, which is the
        // liveness and nothing else.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let Some(solved) = Solver::solve(func, &mut stats) else { return stats };
        replace(func, &solved, fuel, &mut stats);
        stats
    }
}

/// What is known about one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fact {
    /// Nothing has reached it yet, which is the optimistic start.
    Undefined,
    /// An integer, and these of its bits.
    Known(Bits),
    /// Anything else, about which this pass says nothing.
    Varying,
}

impl Fact {
    /// What a value is known to be when it could have come from either.
    fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Undefined, fact) | (fact, Self::Undefined) => fact,
            (Self::Known(a), Self::Known(b)) => Self::Known(a.join(b)),
            _ => Self::Varying,
        }
    }
}

/// Whether this pass keeps bits for a value of this type.
fn tracked(ty: Type) -> bool {
    ty.is_int() && ty.is_scalar()
}

/// The fact for a value this pass knows nothing about, which is every bit unknown for an integer.
fn nothing(ty: Type) -> Fact {
    if tracked(ty) { Fact::Known(Bits::unknown(ty.bits())) } else { Fact::Varying }
}

/// The value, as a constant, when every one of its bits is known.
fn constant(fact: Fact, ty: Type) -> Option<u128> {
    let Fact::Known(bits) = fact else { return None };
    (tracked(ty) && bits.unknown_bits() == 0).then_some(bits.value())
}

/// What the fixpoint came to, apart from the function so the function can be rewritten with it.
struct Solved {
    /// What is known about each value, by [`Value::index`].
    facts: Vec<Fact>,
    /// Whether each block is reachable, by [`Block::index`].
    reached: Vec<bool>,
}

/// Which arms of a terminator are taken.
enum Arms {
    /// Every one, because the condition could be anything or the terminator has no condition.
    Every,
    /// None yet, because nothing has reached the condition.
    NoneYet,
    /// This one, by position, because the condition settles it.
    One(usize),
}

/// The fixpoint while it runs.
struct Solver<'a> {
    func: &'a Func,
    /// What is known about each value, by [`Value::index`].
    facts: Vec<Fact>,
    /// Whether each block is reachable, by [`Block::index`].
    reached: Vec<bool>,
    /// The instructions that read each value, as an argument or as an argument to a block.
    readers: Vec<Vec<Inst>>,
    /// Which arms of each terminator have been taken, by position.
    taken: Set<(Inst, usize)>,
    /// The branches on an undefined value that were given every edge.
    forced: Set<Inst>,
    /// Blocks to look at every instruction of.
    blocks: Vec<Block>,
    /// Values whose readers to look at again.
    values: Vec<Value>,
}

impl<'a> Solver<'a> {
    /// Runs the fixpoint over the function, or nothing for a declaration.
    fn solve(func: &'a Func, stats: &mut Stats) -> Option<Solved> {
        let entry = func.entry()?;
        let counts = func.counts();
        let mut readers = vec![Vec::new(); counts.values];
        for block in func.blocks() {
            for inst in func.insts(block) {
                uses::operands(func, inst, |value| readers[value.index()].push(inst));
            }
        }
        let mut solver = Self {
            func,
            facts: vec![Fact::Undefined; counts.values],
            reached: vec![false; counts.blocks],
            readers,
            taken: Set::default(),
            forced: Set::default(),
            blocks: Vec::new(),
            values: Vec::new(),
        };
        // The entry's parameters are the function's, which come from a caller nothing here sees.
        for &param in &func[entry].params {
            solver.facts[param.index()] = nothing(func[param].ty);
        }
        solver.reach(entry);
        loop {
            solver.settle();
            if !solver.force(stats) {
                break;
            }
        }
        for block in func.blocks().filter(|block| solver.reached[block.index()]) {
            let Some(term) = func.terminator(block) else { continue };
            let arms = func.successors(term).count();
            let dead = (0..arms).filter(|&arm| !solver.taken.contains(&(term, arm))).count();
            for _ in 0..dead {
                stats.note(DEAD_EDGE);
            }
        }
        Some(Solved { facts: solver.facts, reached: solver.reached })
    }

    /// Works both lists until neither has anything left on it.
    fn settle(&mut self) {
        loop {
            if let Some(block) = self.blocks.pop() {
                for inst in self.func.insts(block) {
                    self.visit(inst);
                }
                continue;
            }
            let Some(value) = self.values.pop() else { return };
            for index in 0..self.readers[value.index()].len() {
                let inst = self.readers[value.index()][index];
                let reached =
                    self.func.block_of(inst).is_some_and(|block| self.reached[block.index()]);
                if reached {
                    self.visit(inst);
                }
            }
        }
    }

    /// Gives every edge to each reachable branch still on an undefined value, and says whether
    /// there was one.
    fn force(&mut self, stats: &mut Stats) -> bool {
        let mut any = false;
        for block in self.func.blocks() {
            if !self.reached[block.index()] {
                continue;
            }
            let Some(term) = self.func.terminator(block) else { continue };
            let data = &self.func[term];
            if !matches!(data.opcode, Opcode::BrIf | Opcode::Switch) || self.forced.contains(&term)
            {
                continue;
            }
            let Some(&cond) = self.func[data.args].first() else { continue };
            if self.facts[cond.index()] == Fact::Undefined {
                self.forced.insert(term);
                stats.note(UNDECIDED);
                self.visit(term);
                any = true;
            }
        }
        any
    }

    /// Marks a block reachable and queues its instructions, the first time.
    fn reach(&mut self, block: Block) {
        if !self.reached[block.index()] {
            self.reached[block.index()] = true;
            self.blocks.push(block);
        }
    }

    /// Joins what a value is known to be with something new, and queues its readers if that
    /// changed anything.
    fn learn(&mut self, value: Value, fact: Fact) {
        let old = self.facts[value.index()];
        let new = old.join(fact);
        if new != old {
            self.facts[value.index()] = new;
            self.values.push(value);
        }
    }

    /// Looks at one instruction in a reachable block.
    fn visit(&mut self, inst: Inst) {
        if self.func.is_terminator(inst) {
            self.branch(inst);
            return;
        }
        let results: Vec<Value> = self.func[inst].results().collect();
        let fact = match results.as_slice() {
            [one] => self.transfer(inst, self.func[*one].ty),
            _ => Fact::Varying,
        };
        for value in results {
            let fact = if fact == Fact::Varying { nothing(self.func[value].ty) } else { fact };
            self.learn(value, fact);
        }
    }

    /// Takes the arms of a terminator its condition allows, passing on what each passes.
    fn branch(&mut self, term: Inst) {
        let calls: Vec<BlockCall> = self.func.successors(term).collect();
        let arms: Vec<usize> = match self.arm(term) {
            Arms::One(arm) => vec![arm],
            Arms::NoneYet => Vec::new(),
            Arms::Every => (0..calls.len()).collect(),
        };
        for arm in arms {
            let Some(call) = calls.get(arm) else { continue };
            self.taken.insert((term, arm));
            let params = self.func[call.block].params.clone();
            for (param, &arg) in params.into_iter().zip(&self.func[call.args]) {
                let fact = if tracked(self.func[param].ty) {
                    self.facts[arg.index()]
                } else {
                    Fact::Varying
                };
                self.learn(param, fact);
            }
            self.reach(call.block);
        }
    }

    /// Which arms of a terminator its condition lets through.
    fn arm(&self, term: Inst) -> Arms {
        let data = &self.func[term];
        if !matches!(data.opcode, Opcode::BrIf | Opcode::Switch) || self.forced.contains(&term) {
            return Arms::Every;
        }
        let Some(&cond) = self.func[data.args].first() else { return Arms::Every };
        let ty = self.func[cond].ty;
        let bits = match self.facts[cond.index()] {
            Fact::Undefined => return Arms::NoneYet,
            Fact::Known(bits) => bits,
            Fact::Varying => return Arms::Every,
        };
        if data.opcode == Opcode::BrIf {
            // The first arm is the one taken when the condition is not zero, which a single known
            // one bit settles as well as a known value does.
            if bits.value() != 0 {
                return Arms::One(0);
            }
            return if bits.unknown_bits() == 0 { Arms::One(1) } else { Arms::Every };
        }
        let Some(value) = constant(Fact::Known(bits), ty) else { return Arms::Every };
        let Extra::Switch(at) = data.extra else { return Arms::Every };
        let cases = &self.func[self.func[at].cases];
        let mask = Bits::exactly(u128::MAX, ty.bits()).value();
        // The default is the first target and each case is one after its value.
        let case = cases.iter().position(|imm| imm.unsigned() & mask == value);
        Arms::One(case.map_or(0, |case| case + 1))
    }

    /// What an instruction's one result is known to be, from what its operands are.
    fn transfer(&self, inst: Inst, ty: Type) -> Fact {
        if !tracked(ty) {
            return Fact::Varying;
        }
        let data = &self.func[inst];
        let args = &self.func[data.args];
        let width = ty.bits();
        if data.opcode == Opcode::IConst {
            let Extra::Imm(at) = data.extra else { return nothing(ty) };
            return Fact::Known(Bits::exactly(self.func[at].unsigned(), width));
        }
        if data.opcode == Opcode::Select {
            let [cond, then, other] = *args else { return nothing(ty) };
            return match self.facts[cond.index()] {
                Fact::Undefined => Fact::Undefined,
                Fact::Known(bits) if bits.value() != 0 => self.facts[then.index()],
                Fact::Known(bits) if bits.unknown_bits() == 0 => self.facts[other.index()],
                _ => self.facts[then.index()].join(self.facts[other.index()]),
            };
        }
        if !reads(data.opcode) {
            return nothing(ty);
        }
        let mut ranges = Vec::with_capacity(args.len());
        for &arg in args {
            let arg_ty = self.func[arg].ty;
            match self.facts[arg.index()] {
                Fact::Undefined => return Fact::Undefined,
                Fact::Known(bits) if tracked(arg_ty) => {
                    ranges.push(Range::full(arg_ty.bits()).narrow(bits));
                }
                _ => return nothing(ty),
            }
        }
        let range = arithmetic(data, &ranges, width);
        match range {
            Some(range) if range.is_empty() => Fact::Undefined,
            Some(range) if range.width() == width => Fact::Known(range.bits()),
            _ => nothing(ty),
        }
    }
}

/// Whether the transfer for this opcode is the range table's, which is the list of operations
/// whose result is a function of their operands and nothing else.
const fn reads(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Shl
            | Opcode::LShr
            | Opcode::AShr
            | Opcode::Trunc
            | Opcode::ZExt
            | Opcode::SExt
            | Opcode::ICmp
            | Opcode::Ctlz
            | Opcode::Cttz
            | Opcode::Ctpop
    )
}

/// The range of the result, from the ranges of the operands, or nothing where the operands are not
/// the shape the operation takes.
fn arithmetic(data: &InstData, ranges: &[Range], width: u32) -> Option<Range> {
    let flags = data.flags;
    let pair = || match *ranges {
        [a, b] if a.width() == b.width() => Some((a, b)),
        _ => None,
    };
    let range = match data.opcode {
        Opcode::Add => pair().map(|(a, b)| ops::add(a, b, flags))?,
        Opcode::Sub => pair().map(|(a, b)| ops::sub(a, b, flags))?,
        Opcode::Mul => pair().map(|(a, b)| ops::mul(a, b, flags))?,
        Opcode::And => pair().map(|(a, b)| ops::and(a, b))?,
        Opcode::Or => pair().map(|(a, b)| ops::or(a, b))?,
        Opcode::Xor => pair().map(|(a, b)| ops::xor(a, b))?,
        Opcode::Shl => pair().map(|(a, b)| ops::shl(a, b, flags))?,
        Opcode::LShr => pair().map(|(a, b)| ops::lshr(a, b, flags))?,
        Opcode::AShr => pair().map(|(a, b)| ops::ashr(a, b, flags))?,
        Opcode::Trunc => ops::trunc(*ranges.first()?, width),
        Opcode::ZExt => ops::zext(*ranges.first()?, width),
        Opcode::SExt => ops::sext(*ranges.first()?, width),
        Opcode::ICmp => {
            let Extra::IntPred(pred) = data.extra else { return None };
            let (a, b) = pair()?;
            match ops::compare(pred, a, b) {
                Truth::Always => Range::exactly(1, width),
                Truth::Never => Range::exactly(0, width),
                Truth::Either => Range::between(0, 1, width),
            }
        }
        // A count is never more than the width of what it counts.
        Opcode::Ctlz | Opcode::Cttz | Opcode::Ctpop => {
            Range::between(0, u128::from(ranges.first()?.width()), width)
        }
        _ => return None,
    };
    Some(range)
}

/// Turns every value the fixpoint proved constant into one, while the fuel lasts.
///
/// An instruction whose result is a constant becomes the constant where it stands, the way
/// [`crate::fold`] does it. A block parameter gets a constant at the top of its block and its
/// readers pointed at that, and the parameter itself is left for the passes that remove unread ones.
fn replace(func: &mut Func, solved: &Solved, fuel: &mut Fuel, stats: &mut Stats) {
    let blocks: Vec<Block> = func.blocks().filter(|block| solved.reached[block.index()]).collect();
    let mut forward = Map::default();
    for block in blocks {
        let params = func[block].params.clone();
        for param in params {
            let ty = func[param].ty;
            let Some(value) = constant(solved.facts[param.index()], ty) else { continue };
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            let Some(first) = func.insts(block).next() else { continue };
            let at = func.add_imm(Imm::from_bits(value));
            let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) };
            let span = func.span(first);
            let made = func.create_inst(data, &[ty], span);
            func.insert_before(made, first);
            let result = func[made].results().next().expect("a constant is one value");
            forward.insert(param, result);
            stats.optimized(REPLACED);
        }
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            let data = func[inst];
            if !reads(data.opcode) && data.opcode != Opcode::Select {
                continue;
            }
            let Some(result) = data.results().next() else { continue };
            let ty = func[result].ty;
            let Some(value) = constant(solved.facts[result.index()], ty) else { continue };
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            let at = func.add_imm(Imm::from_bits(value));
            let data = &mut func[inst];
            data.opcode = Opcode::IConst;
            data.flags = Flags::NONE;
            data.args = ValueList::EMPTY;
            data.extra = Extra::Imm(at);
            stats.optimized(REPLACED);
        }
    }
    if !forward.is_empty() {
        uses::substitute(func, &forward);
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::Sccp;
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it under this much fuel,
    /// printed, and checked by the verifier on the way out.
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
            Sccp.run(&mut module[id], &mut an, fuel);
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    fn solved(body: &str) -> String {
        run(body, &mut Fuel::unlimited())
    }

    /// The loop from the top of the module. `x` goes round as one, so the edge that would set
    /// `done` is never taken and `done` goes round as zero.
    const LOOP: &str = r#"
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = iconst.i32 0
    jump block1(%1, %2, %2)
block1(%3: i32, %4: i32, %5: i32):
    %6 = icmp slt %5, %0
    br_if %6, block2, block5
block2:
    %7 = icmp ne %3, %1
    br_if %7, block3, block4(%4)
block3:
    %8 = iconst.i32 1
    jump block4(%8)
block4(%9: i32):
    %10 = iconst.i32 1
    %11 = add.i32 %5, %10
    jump block1(%10, %9, %11)
block5:
    return %4
}
"#;

    #[test]
    fn a_value_that_goes_round_a_loop_unchanged_is_a_constant() {
        let out = solved(LOOP);
        assert!(out.contains("return %"), "{out}");
        assert!(!out.contains("icmp ne"), "the comparison is a constant, {out}");
        assert!(out.contains("icmp slt"), "and the loop test is not, {out}");
        let ret = out.lines().find(|line| line.contains("return")).expect("a return");
        let returned = ret.trim().trim_start_matches("return ").to_string();
        let def = out
            .lines()
            .find(|line| line.trim().starts_with(&format!("{returned} =")))
            .unwrap_or_else(|| panic!("{returned} is defined, {out}"));
        assert!(def.contains("iconst.i32 0"), "done is zero, {out}");
    }

    /// A branch whose condition is known takes one arm, so what the other arm passes does not
    /// reach the block they meet in.
    #[test]
    fn a_known_branch_sends_only_the_arm_it_takes() {
        let out = solved(
            r#"
func @f() -> i32, linkage(external) {
block0:
    %0 = iconst.i32 3
    %1 = iconst.i32 3
    %2 = icmp eq %0, %1
    br_if %2, block1, block2
block1:
    %3 = iconst.i32 7
    jump block3(%3)
block2:
    %4 = iconst.i32 9
    jump block3(%4)
block3(%5: i32):
    %6 = add.i32 %5, %0
    return %6
}
"#,
        );
        assert!(out.contains("iconst.i32 10"), "seven and three, {out}");
        assert!(!out.contains("add"), "{out}");
    }

    /// A switch on a known value takes its case, and a parameter the default would have set is
    /// what the case sets.
    #[test]
    fn a_known_switch_sends_only_its_case() {
        let out = solved(
            r#"
func @f() -> i32, linkage(external) {
block0:
    %0 = iconst.i32 2
    switch %0, block1(%0), [1 => block1(%0), 2 => block2]
block1(%1: i32):
    %2 = mul.i32 %1, %1
    return %2
block2:
    %3 = iconst.i32 5
    jump block1(%3)
}
"#,
        );
        assert!(out.contains("iconst.i32 25"), "{out}");
        assert!(!out.contains("mul"), "{out}");
    }

    /// Known bits are facts short of a constant: whatever `x` is, `x & 12` has its low two bits
    /// clear, so testing them is testing a zero.
    #[test]
    fn the_known_bits_of_an_and_settle_a_test_of_the_bits_it_cleared() {
        let out = solved(
            r#"
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 12
    %2 = and.i32 %0, %1
    %3 = iconst.i32 3
    %4 = and.i32 %2, %3
    return %4
}
"#,
        );
        assert!(out.contains("and %0, %1"), "the first and is not a constant, {out}");
        assert!(out.contains("%4 = iconst.i32 0"), "the second is, {out}");
    }

    /// A parameter that is a constant on every edge into its block is that constant, and one that
    /// differs between two taken edges is left alone.
    #[test]
    fn a_parameter_is_what_every_taken_edge_agrees_on() {
        let out = solved(
            r#"
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = iconst.i32 4
    %2 = iconst.i32 6
    br_if %0, block1(%1, %1), block1(%1, %2)
block1(%3: i32, %4: i32):
    %5 = add.i32 %3, %4
    return %5
}
"#,
        );
        assert!(out.contains("add %"), "the second parameter is four or six, {out}");
        let add = out.lines().find(|line| line.contains("add %")).expect("the add is there");
        assert!(!add.contains("%3"), "the first parameter is a constant now, {out}");
    }

    /// Fuel stops the replacing and not the fixpoint, so what is replaced with a little fuel is a
    /// prefix of what is replaced with all of it and never a value the fixpoint had not finished.
    #[test]
    fn fuel_stops_the_replacing_and_not_the_fixpoint() {
        let none = run(LOOP, &mut Fuel::of(0));
        assert!(none.contains("icmp ne %3, %1"), "{none}");
        let one = run(LOOP, &mut Fuel::of(1));
        let all = solved(LOOP);
        assert_ne!(one, none, "one replacement was made");
        assert_ne!(one, all, "and only one");
    }

    /// A block only a dead edge reaches is left as it was, branch and all, since nothing the
    /// fixpoint says about it was ever checked against a path the program can take, and removing it
    /// is `prune`'s work.
    #[test]
    fn a_block_nothing_reaches_is_left_alone() {
        let out = solved(
            r#"
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = iconst.i32 0
    %2 = icmp ne %1, %1
    br_if %2, block2(%0), block1
block1:
    %3 = iconst.i32 2
    return %3
block2(%4: i1):
    br_if %4, block3, block1
block3:
    %5 = iconst.i32 1
    return %5
}
"#,
        );
        assert!(out.contains("br_if %4"), "an unreachable branch is left as it was, {out}");
        assert!(out.contains("block2(%4: i1)"), "and so is its parameter, {out}");
    }
}
