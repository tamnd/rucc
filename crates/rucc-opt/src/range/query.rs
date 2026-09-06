//! Asking what a value is at a point, and answering it by walking backwards from there.
//!
//! Design: `spec/optimizer/10-value-ranges.md` sections 10.1, 10.3 and 10.6. The representation
//! is [`super::Range`] and the arithmetic over it is [`super::ops`]. This is the part that reads
//! a function.
//!
//! # On demand, and why that is the whole design
//!
//! The textbook version of this analysis is a forward propagation: start every value at empty,
//! iterate over the control flow graph to a fixed point, keep a range per value. Section 10.1
//! says what is wrong with it, and it is not the running time. It is that the range such a pass
//! stores is the range at the definition, and the question anyone actually has is the range at a
//! use, which is narrower by every branch in between. A pass that answers the first question
//! precisely and the second one not at all has computed the wrong thing carefully.
//!
//! So [`Ranges::at`] takes a value and a block and walks backwards. The definition of the value
//! gives a first answer, the branches that dominate the block narrow it, and nothing is computed
//! for a value nobody asked about. Section 10.1 measured the ratio the other way round and rucc
//! has fewer consumers than GCC does, so the ratio here is worse.
//!
//! # Inverting the condition, which is where the precision is
//!
//! `if (x < 10)` tells you about `x` and that is easy. `if (x + 3 < 10)` tells you about `x + 3`,
//! and the fact worth having is that `x` is at most six. GCC calls the machinery that gets from
//! one to the other GORI, and it is the inverse half of the table in [`super::ops`] applied along
//! the chain from the condition back to the value being asked about.
//!
//! [`Ranges::at`] does that walk. It is bounded, because the chain can be as long as the function
//! and because a walk that is not bounded is a compile time bug waiting for the right input.
//! [`Options::logical_depth`] is how deep it goes, and it is GCC's `ranger-logical-depth`, whose
//! default is the same six.
//!
//! # The oracle, which knows things intervals cannot say
//!
//! `a < b` is not a fact about the range of either. If both are `[0, 100]` the intervals say
//! nothing, and yet a branch may have proved it. Section 10.3 says to keep this and to keep it
//! small, so [`Ranges::relation`] answers from what was recorded on the dominating edges plus one
//! step of composition, and it is keyed by block because `a < b` holds on one edge and not on the
//! other one out of the same branch. Section 10.7 lists a relation recorded without its block as
//! a way to be wrong, and it is the one that would show up as a miscompilation rather than as a
//! missed optimization.
//!
//! # The cache is bounded on purpose
//!
//! A cache holding a range per value per block is quadratic in function size, and section 10.6
//! points out that the input which makes that hurt is not hypothetical: generated parsers have
//! tens of thousands of blocks and it is why GCC has `vrp-sparse-threshold` at all. So the cache
//! here holds one range per value at its definition and at most [`Options::refinements`]
//! block-specific answers beside it. Past that, a query for a new block gets the definition
//! range, which is correct and less precise, and [`Counts::fallbacks`] says how often that
//! happened. The bound is a parameter rather than a constant because the right number is an
//! empirical question and section 10.6 says GCC's numbers are a record of bug reports.
//!
//! # How this is wrong
//!
//! A value carried around a loop is not pinned down. The walk assumes the range of the type for
//! a value it is already in the middle of computing, which is what makes it terminate, so what
//! comes back for a loop counter is one step of the recurrence applied to everything rather than
//! the interval a fixed point would reach. That is sound, because every operation here
//! over-approximates and the assumption it started from does too, and it is loose. There is no
//! widening in M4 to tighten it, and the honest place to close the gap is document 07's scalar
//! evolution, which already knows the shape of a loop-carried value and is a better answer than
//! a widening operator guessing at one.
//!
//! Ranges derived from an overflow flag are ranges derived from undefined behaviour, and section
//! 10.7 says those have to be visible. [`Counts::assumed`] counts them, which is less than that
//! section asks for: it wants `-fdump-ranges` to mark them and name the line, and the dump is not
//! here yet.
//!
//! Precision loss is the failure mode with no symptom. [`Counts::losses`] breaks the queries that
//! came back knowing nothing down by the opcode that lost it, which is how the table in
//! [`super::ops`] grows by evidence rather than by guesswork.

use std::collections::{BTreeMap, HashMap, HashSet};

use rucc_ir::{Block, Def, Extra, Func, Inst, IntPred, Opcode, Value};

use super::ops::{self, Truth, Undo};
use super::{PAIRS, Range};
use crate::cfg::Cfg;
use crate::dom::Dominators;

/// How many relations one block's chain of dominating edges keeps.
///
/// The oracle is a list rather than a matrix, so the cost of a query is the length of this and
/// the cost of holding one is a small vector per block. Sixteen is more relations than any block
/// in real C is dominated by, and a block that is dominated by more than sixteen keeps the ones
/// nearest to it, which are the ones a query is most likely to be about.
const RELATIONS: usize = 16;

/// How many cases a switch default edge will exclude before it stops trying.
///
/// Excluding one value from a range costs an interval and there are [`PAIRS`] of them, so the
/// fourth exclusion cannot be represented and the fifth is wasted work. This is not a limit on
/// how many cases a switch may have.
const EXCLUSIONS: usize = PAIRS + 1;

/// The limits, all three of which exist because the thing they bound is otherwise unbounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// How deep into a condition the edge calculation looks, and how far back along the chain
    /// from a condition to a value the inversion walks.
    ///
    /// GCC's `ranger-logical-depth`, whose default at `gcc/params.opt:998` is also six.
    pub logical_depth: u32,
    /// How many dominating edges one query walks before it stops narrowing.
    ///
    /// GCC's `ranger-recompute-depth` at `gcc/params.opt:1003` bounds a related walk with the
    /// same default of five. The two are not the same walk, so the number is borrowed and the
    /// meaning is not.
    pub recompute_depth: u32,
    /// How many block-specific answers the cache keeps for one value.
    ///
    /// Section 10.6's one threshold. A query past it gets the range at the definition.
    pub refinements: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { logical_depth: 6, recompute_depth: 5, refinements: 8 }
    }
}

/// What the queries did, which is the only way to find out that this is not working.
///
/// A range that came back knowing nothing produces correct code that is slower, with no test
/// failing and no warning printed. Section 10.7 says the defence is a counter and section 10.8
/// says `-ftime-report` prints it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    queries: u64,
    hits: u64,
    fallbacks: u64,
    full: u64,
    assumed: u64,
    lost: BTreeMap<Opcode, u64>,
}

impl Counts {
    /// How many times a range was asked for.
    #[must_use]
    pub const fn queries(&self) -> u64 {
        self.queries
    }

    /// How many of those the cache answered.
    #[must_use]
    pub const fn hits(&self) -> u64 {
        self.hits
    }

    /// How many were answered with the range at the definition because the cache was full.
    #[must_use]
    pub const fn fallbacks(&self) -> u64 {
        self.fallbacks
    }

    /// How many came back knowing nothing at all.
    #[must_use]
    pub const fn full(&self) -> u64 {
        self.full
    }

    /// How many ranges were narrower because an instruction promised not to overflow.
    ///
    /// These are the ranges section 10.7 calls correct and surprising: they are true only
    /// because the program would be undefined otherwise.
    #[must_use]
    pub const fn assumed(&self) -> u64 {
        self.assumed
    }

    /// Which opcodes lost the information, most often first.
    #[must_use]
    pub fn losses(&self) -> Vec<(Opcode, u64)> {
        let mut losses: Vec<(Opcode, u64)> = self.lost.iter().map(|(&op, &n)| (op, n)).collect();
        losses.sort_by_key(|&(opcode, count)| (std::cmp::Reverse(count), opcode));
        losses
    }
}

/// One relation between two values, as it was recorded on an edge.
///
/// The pair is ordered as it was written, so `a < b` and `b > a` are the same fact stored one
/// way, and reading it the other way round is [`IntPred::swapped`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Relation {
    left: Value,
    pred: IntPred,
    right: Value,
}

/// What the cache holds for one value.
#[derive(Clone, Debug, Default)]
struct Entry {
    at_def: Option<Range>,
    refined: HashMap<Block, Range>,
}

/// The range analysis of one function.
///
/// Queries take `&mut self` because a query fills the cache and moves the counters, which is the
/// design and not an accident: an analysis that answered without recording what it was asked
/// could not report the losses in section 10.7.
#[derive(Debug)]
pub struct Ranges<'a> {
    func: &'a Func,
    cfg: &'a Cfg,
    dom: &'a Dominators,
    options: Options,
    cache: HashMap<Value, Entry>,
    relations: HashMap<Block, Vec<Relation>>,
    counts: Counts,
    /// The values whose definition range is being computed right now.
    ///
    /// Re-entering one is a cycle, which in SSA means a loop-carried value, and the answer there
    /// is the range of the type.
    active: HashSet<Value>,
    /// How many times that has happened, so that an answer which leaned on a cycle is not cached
    /// and the next query gets the same answer rather than a worse one.
    cycles: u64,
}

impl<'a> Ranges<'a> {
    /// The analysis of this function, with the limits at their defaults.
    #[must_use]
    pub fn new(func: &'a Func, cfg: &'a Cfg, dom: &'a Dominators) -> Self {
        Self::with(func, cfg, dom, Options::default())
    }

    /// The same, with the limits the command line asked for.
    #[must_use]
    pub fn with(func: &'a Func, cfg: &'a Cfg, dom: &'a Dominators, options: Options) -> Self {
        Self {
            func,
            cfg,
            dom,
            options,
            cache: HashMap::new(),
            relations: HashMap::new(),
            counts: Counts::default(),
            active: HashSet::new(),
            cycles: 0,
        }
    }

    /// What the queries have done so far.
    #[must_use]
    pub const fn counts(&self) -> &Counts {
        &self.counts
    }

    /// What this value can be where it is defined.
    pub fn of(&mut self, value: Value) -> Range {
        self.counts.queries += 1;
        self.at_def(value)
    }

    /// What this value can be on entry to this block.
    ///
    /// The block has to be one the definition reaches, which for a use is the block the use is
    /// in. Asking about a block the definition does not dominate is not wrong, it just gets an
    /// answer that ignored the branches it could not see.
    pub fn at(&mut self, value: Value, block: Block) -> Range {
        self.counts.queries += 1;
        self.refined(value, block)
    }

    /// What this value can be at this instruction.
    ///
    /// The same as [`Ranges::at`] on the block holding it. Ranges within a block do not change
    /// in rucc's IR, because there is nothing between two instructions that could narrow one:
    /// the branches are all at the ends of blocks.
    pub fn at_inst(&mut self, value: Value, inst: Inst) -> Range {
        match self.func.block_of(inst) {
            Some(block) => self.at(value, block),
            None => self.of(value),
        }
    }

    /// Whether this comparison is settled where it stands.
    ///
    /// The ranges answer first, because they answer more often. The oracle answers the cases
    /// they cannot, which are the ones where the two values are related without either being
    /// pinned down, and section 10.3 says that is most of what removes a repeated bounds check.
    pub fn compare(&mut self, pred: IntPred, a: Value, b: Value, block: Block) -> Truth {
        let (left, right) = (self.at(a, block), self.at(b, block));
        if left.width() != right.width() {
            return Truth::Either;
        }
        match ops::compare(pred, left, right) {
            Truth::Either => (),
            settled => return settled,
        }
        match self.relation(a, b, block) {
            Some(known) if implies(known, pred) => Truth::Always,
            Some(known) if excludes(known, pred) => Truth::Never,
            _ => Truth::Either,
        }
    }

    /// What is known to hold between these two values in this block, if anything.
    ///
    /// What was recorded on a dominating edge, read in the order asked, plus one step through an
    /// intermediate value. Not the transitive closure: section 10.3 says computing that is where
    /// the cost of a relational oracle goes and that one step pays for most of it.
    pub fn relation(&mut self, a: Value, b: Value, block: Block) -> Option<IntPred> {
        let facts = self.facts(block).clone();
        if let Some(direct) = read(&facts, a, b) {
            return Some(direct);
        }
        for step in &facts {
            for middle in [step.left, step.right] {
                if middle == a || middle == b {
                    continue;
                }
                let composed = read(&facts, a, middle)
                    .zip(read(&facts, middle, b))
                    .and_then(|(first, second)| compose(first, second));
                if composed.is_some() {
                    return composed;
                }
            }
        }
        None
    }

    /// The range at the definition, cached, with the cycle guard around it.
    fn at_def(&mut self, value: Value) -> Range {
        let ty = self.func[value].ty;
        if !ty.is_int() || !ty.is_scalar() {
            return Range::of(ty);
        }
        if let Some(cached) = self.cache.get(&value).and_then(|entry| entry.at_def) {
            self.counts.hits += 1;
            return cached;
        }
        if !self.active.insert(value) {
            self.cycles += 1;
            return Range::of(ty);
        }
        let before = self.cycles;
        let range = self.compute(value);
        self.active.remove(&value);
        if self.cycles == before {
            self.cache.entry(value).or_default().at_def = Some(range);
        }
        range
    }

    /// The range at the definition, worked out.
    fn compute(&mut self, value: Value) -> Range {
        let ty = self.func[value].ty;
        match self.func[value].def {
            Def::Param { block, index } => self.of_param(value, block, index),
            Def::Result { inst, .. } => {
                let range = self.of_inst(value, inst);
                if range.is_full() {
                    self.counts.full += 1;
                    *self.counts.lost.entry(self.func[inst].opcode).or_default() += 1;
                }
                debug_assert_eq!(range.width(), ty.bits(), "a range of the wrong width");
                range
            }
        }
    }

    /// The range of a block parameter, which is what every predecessor can pass to it.
    fn of_param(&mut self, value: Value, block: Block, index: u32) -> Range {
        let ty = self.func[value].ty;
        if self.cfg.entry() == Some(block) {
            return Range::of(ty);
        }
        let preds: Vec<Block> = self.cfg.predecessors(block).to_vec();
        if preds.is_empty() {
            return Range::of(ty);
        }
        let mut range = Range::empty(ty.bits());
        for pred in preds {
            let Some(arg) = argument(self.func, pred, block, index as usize) else {
                return Range::of(ty);
            };
            let incoming = self.refined(arg, pred);
            let edge = self.edge_fact(pred, block, arg).unwrap_or_else(|| Range::of(ty));
            range = range.union(incoming.intersect(edge));
            if range.is_full() {
                return range;
            }
        }
        range
    }

    /// The range of an instruction's result, which is the table in [`super::ops`] applied to the
    /// ranges of its operands where they stand.
    fn of_inst(&mut self, value: Value, inst: Inst) -> Range {
        let ty = self.func[value].ty;
        let width = ty.bits();
        let data = self.func[inst];
        let block = self.func.block_of(inst);
        let args: Vec<Value> = self.func[data.args].to_vec();
        let flags = data.flags;
        let operand = |this: &mut Self, index: usize| match (args.get(index), block) {
            (Some(&arg), Some(block)) => this.refined(arg, block),
            (Some(&arg), None) => this.at_def(arg),
            (None, _) => Range::of(ty),
        };
        match data.opcode {
            Opcode::IConst => {
                let Extra::Imm(at) = data.extra else { return Range::of(ty) };
                Range::exactly(self.func[at].unsigned(), width)
            }
            Opcode::Add | Opcode::Sub | Opcode::Mul => {
                let (a, b) = (operand(self, 0), operand(self, 1));
                if a.width() != b.width() {
                    return Range::of(ty);
                }
                let apply = |flags| match data.opcode {
                    Opcode::Add => ops::add(a, b, flags),
                    Opcode::Sub => ops::sub(a, b, flags),
                    _ => ops::mul(a, b, flags),
                };
                self.assuming(apply, flags)
            }
            Opcode::And | Opcode::Or | Opcode::Xor => {
                let (a, b) = (operand(self, 0), operand(self, 1));
                if a.width() != b.width() {
                    return Range::of(ty);
                }
                match data.opcode {
                    Opcode::And => ops::and(a, b),
                    Opcode::Or => ops::or(a, b),
                    _ => ops::xor(a, b),
                }
            }
            Opcode::Shl | Opcode::LShr | Opcode::AShr => {
                let (a, count) = (operand(self, 0), operand(self, 1));
                if a.width() != count.width() {
                    return Range::of(ty);
                }
                let apply = |flags| match data.opcode {
                    Opcode::Shl => ops::shl(a, count, flags),
                    Opcode::LShr => ops::lshr(a, count, flags),
                    _ => ops::ashr(a, count, flags),
                };
                self.assuming(apply, flags)
            }
            Opcode::Trunc => ops::trunc(operand(self, 0), width),
            Opcode::ZExt => ops::zext(operand(self, 0), width),
            Opcode::SExt => ops::sext(operand(self, 0), width),
            Opcode::ICmp => {
                let Extra::IntPred(pred) = data.extra else { return Range::of(ty) };
                let (a, b) = (operand(self, 0), operand(self, 1));
                if a.width() != b.width() {
                    return Range::of(ty);
                }
                match ops::compare(pred, a, b) {
                    Truth::Always => Range::exactly(1, width),
                    Truth::Never => Range::exactly(0, width),
                    Truth::Either => Range::of(ty),
                }
            }
            // A bit count cannot exceed the width of what it counts, which is worth saying
            // because the value it produces is almost always used to index or to shift.
            Opcode::Ctlz | Opcode::Cttz | Opcode::Ctpop => {
                let counted = args.first().map_or(width, |&arg| self.func[arg].ty.bits());
                Range::between(0, u128::from(counted), width)
            }
            _ => Range::of(ty),
        }
    }

    /// The operation under the flags it carries, and the count of how much they bought.
    ///
    /// Section 10.7 says the flag has to be an input to the operation rather than a check
    /// somewhere upstream. It also says a range that is only true because the program would
    /// otherwise be undefined has to be visible, and the difference between the two answers here
    /// is exactly that range.
    fn assuming(
        &mut self,
        apply: impl Fn(rucc_ir::Flags) -> Range,
        flags: rucc_ir::Flags,
    ) -> Range {
        let range = apply(flags);
        if !flags.is_empty() && range != apply(rucc_ir::Flags::NONE) {
            self.counts.assumed += 1;
        }
        range
    }

    /// The range at the definition, narrowed by the branches that dominate this block.
    fn refined(&mut self, value: Value, block: Block) -> Range {
        let ty = self.func[value].ty;
        if !ty.is_int() || !ty.is_scalar() {
            return Range::of(ty);
        }
        if let Some(&cached) = self.cache.get(&value).and_then(|e| e.refined.get(&block)) {
            self.counts.hits += 1;
            return cached;
        }
        let full = self
            .cache
            .get(&value)
            .is_some_and(|entry| entry.refined.len() >= self.options.refinements);
        if full {
            self.counts.fallbacks += 1;
            return self.at_def(value);
        }
        let before = self.cycles;
        let range = self.walk(value, block);
        if self.cycles == before {
            let entry = self.cache.entry(value).or_default();
            if entry.refined.len() < self.options.refinements {
                entry.refined.insert(block, range);
            }
        }
        range
    }

    /// The walk itself, up the dominator tree from the block to the definition.
    ///
    /// It stops at the definition because an edge above that cannot say anything about a value
    /// that does not exist yet, and because whatever it says about the operands is already in
    /// the answer: they were asked for where the instruction stands.
    fn walk(&mut self, value: Value, block: Block) -> Range {
        let mut range = self.at_def(value);
        let stop = defining_block(self.func, value);
        let mut cursor = block;
        let mut steps = 0;
        while steps < self.options.recompute_depth && Some(cursor) != stop {
            let Some(parent) = self.dom.immediate_dominator(cursor) else { break };
            if self.cfg.predecessors(cursor) == [parent] {
                if let Some(fact) = self.edge_fact(parent, cursor, value) {
                    range = range.intersect(fact);
                }
            }
            cursor = parent;
            steps += 1;
        }
        range
    }

    /// What taking the edge from one block to another says about a value, if anything.
    fn edge_fact(&mut self, from: Block, to: Block, value: Value) -> Option<Range> {
        let term = self.func.terminator(from)?;
        let depth = self.options.logical_depth;
        match self.func[term].opcode {
            Opcode::BrIf => {
                let calls: Vec<_> = self.func.successors(term).collect();
                let (then, other) = (calls.first()?, calls.get(1)?);
                if then.block == other.block {
                    return None;
                }
                let taken = then.block == to;
                let cond = *self.func[self.func[term].args].first()?;
                self.condition_fact(cond, taken, value, from, depth)
            }
            Opcode::Switch => self.switch_fact(term, to, value, from, depth),
            _ => None,
        }
    }

    /// What a switch edge says about the value it switched on, carried back to the value asked
    /// about.
    fn switch_fact(
        &mut self,
        term: Inst,
        to: Block,
        value: Value,
        block: Block,
        depth: u32,
    ) -> Option<Range> {
        if depth == 0 {
            return None;
        }
        let Extra::Switch(info) = self.func[term].extra else { return None };
        let info = self.func[info];
        let calls: Vec<_> = self.func[info.targets].to_vec();
        let cases: Vec<_> = self.func[info.cases].to_vec();
        let subject = *self.func[self.func[term].args].first()?;
        let width = self.func[subject].ty.bits();
        let default = calls.first()?.block;
        let hits: Vec<usize> = (1..calls.len()).filter(|&index| calls[index].block == to).collect();
        let known = if default == to {
            // The default edge means none of the cases matched, which is a fact only while the
            // exclusions still fit. It is also not a fact at all if a case goes to the same
            // block, since then the edge does not say which of the two ways it came.
            if !hits.is_empty() {
                return None;
            }
            let mut range = Range::full(width);
            for &case in cases.iter().take(EXCLUSIONS) {
                range = range.intersect(Range::other_than(case.unsigned(), width));
            }
            range
        } else {
            let pairs: Vec<(u128, u128)> = hits
                .iter()
                .filter_map(|&index| cases.get(index - 1))
                .map(|case| (case.unsigned(), case.unsigned()))
                .collect();
            if pairs.is_empty() {
                return None;
            }
            Range::from_pairs(&pairs, width)
        };
        self.carry_back(subject, known, value, block, depth - 1)
    }

    /// What a condition being true, or being false, says about a value.
    fn condition_fact(
        &mut self,
        cond: Value,
        taken: bool,
        value: Value,
        block: Block,
        depth: u32,
    ) -> Option<Range> {
        if depth == 0 {
            return None;
        }
        if cond == value {
            let width = self.func[value].ty.bits();
            return Some(Range::exactly(u128::from(taken), width));
        }
        let Def::Result { inst, .. } = self.func[cond].def else { return None };
        let data = self.func[inst];
        let args: Vec<Value> = self.func[data.args].to_vec();
        match data.opcode {
            Opcode::ICmp => {
                let Extra::IntPred(pred) = data.extra else { return None };
                let pred = if taken { pred } else { pred.inverse() };
                let (&left, &right) = (args.first()?, args.get(1)?);
                let (a, b) = (self.refined(left, block), self.refined(right, block));
                if a.width() != b.width() {
                    return None;
                }
                let want = ops::narrow_for(pred, a, b);
                if let Some(found) = self.carry_back(left, want, value, block, depth - 1) {
                    return Some(found);
                }
                let want = ops::narrow_for(pred.swapped(), b, a);
                self.carry_back(right, want, value, block, depth - 1)
            }
            // Both arms of an `and` hold on the edge where it is true, and both fail on the edge
            // where an `or` is false. The other two edges say nothing, because either arm could
            // be the one that decided it. This is the whole of what section 10.1's logical depth
            // is counting.
            Opcode::And | Opcode::Or => {
                let holds = data.opcode == Opcode::And;
                if taken != holds {
                    return None;
                }
                let (&left, &right) = (args.first()?, args.get(1)?);
                let a = self.condition_fact(left, taken, value, block, depth - 1);
                let b = self.condition_fact(right, taken, value, block, depth - 1);
                match (a, b) {
                    (Some(a), Some(b)) => Some(a.intersect(b)),
                    (found, None) | (None, found) => found,
                }
            }
            // `xor c, 1` on a one bit value is `not c`, which is how the front end writes a
            // negated condition.
            Opcode::Xor => {
                let (&left, &right) = (args.first()?, args.get(1)?);
                let (cond, other) = match self.constant(right) {
                    Some(_) => (left, right),
                    None => (right, left),
                };
                let one = self.constant(other)? == 1 && self.func[other].ty.bits() == 1;
                if !one {
                    return None;
                }
                self.condition_fact(cond, !taken, value, block, depth - 1)
            }
            _ => None,
        }
    }

    /// Given that `subject` is in `known`, what that says about `value`.
    ///
    /// The inverse half of the table, walked back along the chain from the subject of a
    /// condition to the value being asked about. Every step is sound on its own because
    /// [`ops::backward`] answers with every operand that could have produced a result in range,
    /// so a chain of them over-approximates and never loses a value that the program can reach.
    fn carry_back(
        &mut self,
        subject: Value,
        known: Range,
        value: Value,
        block: Block,
        depth: u32,
    ) -> Option<Range> {
        if subject == value {
            return Some(known);
        }
        if depth == 0 || known.is_full() {
            return None;
        }
        let Def::Result { inst, .. } = self.func[subject].def else { return None };
        let data = self.func[inst];
        let args: Vec<Value> = self.func[data.args].to_vec();
        let (&left, right) = (args.first()?, args.get(1).copied());
        let steps: Vec<(Value, Undo, Option<Value>)> = match data.opcode {
            // Addition is the same undo both ways round, since either operand is the result less
            // the other one. Subtraction is not, and section 10.4's inverse for its right operand
            // is the one that looks like the others and is not.
            Opcode::Add => vec![(left, Undo::AddLeft, right), (right?, Undo::AddLeft, Some(left))],
            Opcode::Sub => vec![(left, Undo::SubLeft, right), (right?, Undo::SubRight, Some(left))],
            Opcode::Xor => vec![(left, Undo::Xor, right), (right?, Undo::Xor, Some(left))],
            Opcode::ZExt => vec![(left, Undo::Zext(self.func[left].ty.bits()), None)],
            Opcode::SExt => vec![(left, Undo::Sext(self.func[left].ty.bits()), None)],
            _ => return None,
        };
        for (operand, undo, other) in steps {
            let other = match other {
                Some(other) => self.refined(other, block),
                None => Range::full(known.width()),
            };
            if other.width() != known.width() {
                continue;
            }
            let back = ops::backward(undo, known, other);
            if let Some(found) = self.carry_back(operand, back, value, block, depth - 1) {
                return Some(found);
            }
        }
        None
    }

    /// The relations that hold in a block, which are its own edge's and its dominator's.
    fn facts(&mut self, block: Block) -> &Vec<Relation> {
        if !self.relations.contains_key(&block) {
            let mut facts = match self.dom.immediate_dominator(block) {
                Some(parent) => self.facts(parent).clone(),
                None => Vec::new(),
            };
            if let Some(own) = self.own_relation(block) {
                facts.push(own);
                if facts.len() > RELATIONS {
                    facts.remove(0);
                }
            }
            self.relations.insert(block, facts);
        }
        &self.relations[&block]
    }

    /// The relation the one edge into this block recorded, if it recorded one.
    fn own_relation(&mut self, block: Block) -> Option<Relation> {
        let [from] = *self.cfg.predecessors(block) else { return None };
        let term = self.func.terminator(from)?;
        if self.func[term].opcode != Opcode::BrIf {
            return None;
        }
        let calls: Vec<_> = self.func.successors(term).collect();
        let (then, other) = (calls.first()?, calls.get(1)?);
        if then.block == other.block {
            return None;
        }
        let taken = then.block == block;
        let cond = *self.func[self.func[term].args].first()?;
        let Def::Result { inst, .. } = self.func[cond].def else { return None };
        if self.func[inst].opcode != Opcode::ICmp {
            return None;
        }
        let Extra::IntPred(pred) = self.func[inst].extra else { return None };
        let args = &self.func[self.func[inst].args];
        let (&left, &right) = (args.first()?, args.get(1)?);
        let pred = if taken { pred } else { pred.inverse() };
        Some(Relation { left, pred, right })
    }

    /// The constant a value is, if it is one.
    fn constant(&self, value: Value) -> Option<u128> {
        let Def::Result { inst, .. } = self.func[value].def else { return None };
        if self.func[inst].opcode != Opcode::IConst {
            return None;
        }
        let Extra::Imm(at) = self.func[inst].extra else { return None };
        Some(self.func[at].unsigned())
    }
}

/// The block a value is defined in.
fn defining_block(func: &Func, value: Value) -> Option<Block> {
    match func[value].def {
        Def::Param { block, .. } => Some(block),
        Def::Result { inst, .. } => func.block_of(inst),
    }
}

/// What this predecessor passes to the block's parameter at this position.
///
/// `None` when the predecessor branches to the block more than once with different arguments,
/// which a `br_if` with both arms on the same block can do and which means the parameter takes a
/// value that depends on the test rather than on the edge.
fn argument(func: &Func, pred: Block, block: Block, index: usize) -> Option<Value> {
    let term = func.terminator(pred)?;
    let mut found = None;
    for call in func.successors(term) {
        if call.block != block {
            continue;
        }
        let arg = *func[call.args].get(index)?;
        if found.replace(arg).is_some_and(|old| old != arg) {
            return None;
        }
    }
    found
}

/// The recorded relation between these two values, read in the order asked.
fn read(facts: &[Relation], a: Value, b: Value) -> Option<IntPred> {
    facts.iter().rev().find_map(|fact| {
        if fact.left == a && fact.right == b {
            Some(fact.pred)
        } else if fact.left == b && fact.right == a {
            Some(fact.pred.swapped())
        } else {
            None
        }
    })
}

/// Which of less, equal and greater a predicate allows.
const fn outcomes(pred: IntPred) -> u8 {
    match pred {
        IntPred::Eq => 0b010,
        IntPred::Ne => 0b101,
        IntPred::Slt | IntPred::Ult => 0b001,
        IntPred::Sle | IntPred::Ule => 0b011,
        IntPred::Sgt | IntPred::Ugt => 0b100,
        IntPred::Sge | IntPred::Uge => 0b110,
    }
}

/// Whether two predicates are reading their operands the same way.
///
/// Equality reads them as neither signed nor unsigned, so it composes with both. Nothing else
/// crosses: `a <s b` says nothing about `a <u b`, and a compiler that assumed otherwise would be
/// wrong on exactly the inputs where it matters.
const fn comparable(a: IntPred, b: IntPred) -> bool {
    ordering_free(a) || ordering_free(b) || a.is_signed() == b.is_signed()
}

/// Whether a predicate reads its operands as neither signed nor unsigned.
const fn ordering_free(pred: IntPred) -> bool {
    matches!(pred, IntPred::Eq | IntPred::Ne)
}

/// Whether what is known forces this predicate to hold.
fn implies(known: IntPred, pred: IntPred) -> bool {
    comparable(known, pred) && outcomes(known) & !outcomes(pred) == 0
}

/// Whether what is known forces this predicate to fail.
fn excludes(known: IntPred, pred: IntPred) -> bool {
    comparable(known, pred) && outcomes(known) & outcomes(pred) == 0
}

/// The relation that follows from two, when one does.
///
/// One step, not a closure. `a < m` and `m <= b` gives `a < b`, and anything mixing a less with a
/// greater gives nothing, which is right: it is the case where the two facts say the values are
/// on opposite sides of the middle one and nothing follows about them.
fn compose(first: IntPred, second: IntPred) -> Option<IntPred> {
    if !comparable(first, second) {
        return None;
    }
    let strict = |pred| matches!(pred, IntPred::Slt | IntPred::Ult | IntPred::Sgt | IntPred::Ugt);
    let direction = |pred| outcomes(pred) & 0b101;
    match (first, second) {
        (IntPred::Eq, other) | (other, IntPred::Eq) => Some(other),
        // Not equal is not a direction, so nothing follows through it: `a != m` and `m != b`
        // leaves `a` and `b` free to be the same value.
        (IntPred::Ne, _) | (_, IntPred::Ne) => None,
        // Two orderings compose when they point the same way, and the result is strict when
        // either step is.
        _ if direction(first) != direction(second) => None,
        _ if strict(first) => Some(first),
        _ => Some(second),
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Flags, Func, IntPred, Opcode, Signature, Type, Value};

    use super::{Options, Ranges};
    use crate::cfg::Cfg;
    use crate::dom::Dominators;
    use crate::range::Range;
    use crate::range::ops::{self, Truth};

    const I32: Type = Type::int(32);

    /// A function taking this many integer parameters, with this many blocks, the entry first.
    ///
    /// The parameters are the point. A test about what a branch proves needs a value that
    /// nothing is known about, and a constant passed into a block would be narrowed to itself
    /// before the branch got a chance to say anything.
    fn shape(params: usize, blocks: usize) -> (Func, Vec<Value>, Vec<Block>) {
        let mut names = Interner::new();
        let types = vec![I32; params];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&types));
        let blocks: Vec<Block> = (0..blocks).map(|_| func.create_block()).collect();
        let args = types.iter().map(|&ty| func.append_param(blocks[0], ty)).collect();
        (func, args, blocks)
    }

    /// The analysis of a finished function, kept together because the parts borrow each other.
    struct Asked {
        cfg: Cfg,
        dom: Dominators,
        func: Func,
    }

    impl Asked {
        fn new(func: Func) -> Self {
            let cfg = Cfg::new(&func);
            let dom = Dominators::new(&cfg);
            Asked { cfg, dom, func }
        }

        fn ranges(&self) -> Ranges<'_> {
            Ranges::new(&self.func, &self.cfg, &self.dom)
        }

        fn with(&self, options: Options) -> Ranges<'_> {
            Ranges::with(&self.func, &self.cfg, &self.dom, options)
        }
    }

    /// The signed bounds of a range, which is what most of these tests are asking about.
    fn bounds(range: Range) -> Option<(i128, i128)> {
        range.signed_bounds()
    }

    #[test]
    fn a_constant_is_itself() {
        let (mut func, _, blocks) = shape(0, 1);
        let mut build = Builder::new(&mut func, blocks[0]);
        let seven = build.iconst(I32, 7);
        build.ret(&[]);
        let asked = Asked::new(func);
        assert_eq!(asked.ranges().of(seven).singleton(), Some(7));
    }

    #[test]
    fn arithmetic_on_constants_is_the_arithmetic() {
        let (mut func, _, blocks) = shape(0, 1);
        let mut build = Builder::new(&mut func, blocks[0]);
        let a = build.iconst(I32, 7);
        let b = build.iconst(I32, 5);
        let sum = build.binary(Opcode::Add, a, b, Flags::NONE);
        build.ret(&[]);
        let asked = Asked::new(func);
        assert_eq!(asked.ranges().of(sum).singleton(), Some(12));
    }

    #[test]
    fn a_value_nothing_is_known_about_is_the_whole_of_its_type_and_says_which_opcode_lost_it() {
        let (mut func, args, blocks) = shape(1, 1);
        let mut build = Builder::new(&mut func, blocks[0]);
        let counted = build.unary(Opcode::Ctlz, args[0], I32);
        let squared = build.binary(Opcode::Mul, args[0], args[0], Flags::NONE);
        build.ret(&[]);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        assert!(ranges.of(args[0]).is_full(), "a parameter is anything");
        // The count of leading zeroes is bounded by the width even though its operand is not.
        assert_eq!(bounds(ranges.of(counted)), Some((0, 32)));
        assert!(ranges.of(squared).is_full());
        assert_eq!(ranges.counts().losses(), vec![(Opcode::Mul, 1)]);
    }

    /// `if (x < bound)` on a parameter, with the two arms in blocks one and two.
    fn guarded(pred: IntPred, bound: i128) -> (Func, Value, Block, Block) {
        let (mut func, args, blocks) = shape(1, 3);
        let mut build = Builder::new(&mut func, blocks[0]);
        let limit = build.iconst(I32, bound);
        let test = build.icmp(pred, args[0], limit);
        build.br_if(test, blocks[1], &[], blocks[2], &[]);
        Builder::new(&mut func, blocks[1]).ret(&[]);
        Builder::new(&mut func, blocks[2]).ret(&[]);
        (func, args[0], blocks[1], blocks[2])
    }

    #[test]
    fn a_branch_narrows_the_value_it_tested_on_both_of_its_edges() {
        let (func, x, then, otherwise) = guarded(IntPred::Slt, 10);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        assert_eq!(bounds(ranges.at(x, then)), Some((i128::from(i32::MIN), 9)));
        assert_eq!(bounds(ranges.at(x, otherwise)), Some((10, i128::from(i32::MAX))));
    }

    #[test]
    fn the_range_at_the_definition_is_not_the_range_at_the_use() {
        let (func, x, then, _) = guarded(IntPred::Ult, 64);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        assert!(ranges.of(x).is_full(), "nothing is known where it is defined");
        assert_eq!(ranges.at(x, then).unsigned_bounds(), Some((0, 63)));
    }

    #[test]
    fn a_null_check_is_the_fact_a_single_interval_cannot_hold() {
        let (func, x, _, otherwise) = guarded(IntPred::Eq, 0);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        let range = ranges.at(x, otherwise);
        assert!(range.nonzero(), "the else edge of an equality with zero proves it");
        // One interval, because this reasons about bit patterns rather than signed numbers.
        // The same fact in GCC's signed domain is two, which is why section 10.2 insists on
        // there being more than one and why the count here is worth writing down.
        assert_eq!(range.pairs().len(), 1);
    }

    /// `if (x + offset < bound)`, which is section 10.1's example of what the inversion is for.
    fn through_arithmetic(offset: i128, bound: i128) -> (Func, Value, Block) {
        let (mut func, args, blocks) = shape(1, 3);
        let mut build = Builder::new(&mut func, blocks[0]);
        let by = build.iconst(I32, offset);
        let shifted = build.binary(Opcode::Add, args[0], by, Flags::NSW);
        let limit = build.iconst(I32, bound);
        let test = build.icmp(IntPred::Slt, shifted, limit);
        build.br_if(test, blocks[1], &[], blocks[2], &[]);
        Builder::new(&mut func, blocks[1]).ret(&[]);
        Builder::new(&mut func, blocks[2]).ret(&[]);
        (func, args[0], blocks[1])
    }

    #[test]
    fn the_condition_is_inverted_back_to_the_value_it_was_computed_from() {
        let (func, x, then) = through_arithmetic(3, 10);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        let (_, high) = bounds(ranges.at(x, then)).expect("not empty");
        assert!(high <= 6, "x + 3 < 10 makes x at most six, and this said {high}");
    }

    #[test]
    fn the_inversion_stops_where_it_is_told_to() {
        let (func, x, then) = through_arithmetic(3, 10);
        let asked = Asked::new(func);
        let options = Options { logical_depth: 1, ..Options::default() };
        let mut ranges = asked.with(options);
        assert!(ranges.at(x, then).is_full(), "one step cannot reach past the comparison");
    }

    #[test]
    fn a_value_carried_round_a_loop_is_not_pinned_down_and_the_branch_still_says_something() {
        let (mut func, _, blocks) = shape(0, 4);
        let counter = func.append_param(blocks[1], I32);
        let mut build = Builder::new(&mut func, blocks[0]);
        let start = build.iconst(I32, 0);
        build.jump(blocks[1], &[start]);
        let mut build = Builder::new(&mut func, blocks[1]);
        let limit = build.iconst(I32, 100);
        let test = build.icmp(IntPred::Slt, counter, limit);
        build.br_if(test, blocks[2], &[], blocks[3], &[]);
        let mut build = Builder::new(&mut func, blocks[2]);
        let one = build.iconst(I32, 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        build.jump(blocks[1], &[next]);
        Builder::new(&mut func, blocks[3]).ret(&[]);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        // There is no widening in M4, so the definition range is one step of the recurrence
        // applied to everything rather than the `[0, 100]` a fixed point would reach. It holds
        // every value the counter really takes, which is what makes it sound, and it holds a
        // great many it does not, which is what makes it worth saying out loud.
        let at_def = ranges.of(counter);
        assert!(at_def.contains(0) && at_def.contains(50) && at_def.contains(100));
        assert_eq!(bounds(at_def), Some((i128::from(i32::MIN) + 1, 100)));
        // The branch still says what a consumer inside the loop wanted.
        let (_, inside) = bounds(ranges.at(counter, blocks[2])).expect("not empty");
        assert_eq!(inside, 99);
        let (after, _) = bounds(ranges.at(counter, blocks[3])).expect("not empty");
        assert_eq!(after, 100);
    }

    #[test]
    fn a_block_parameter_is_everything_its_predecessors_pass_to_it() {
        let (mut func, args, blocks) = shape(1, 4);
        let merged = func.append_param(blocks[3], I32);
        let mut build = Builder::new(&mut func, blocks[0]);
        let zero = build.iconst(I32, 0);
        let cond = build.icmp(IntPred::Slt, args[0], zero);
        build.br_if(cond, blocks[1], &[], blocks[2], &[]);
        let mut build = Builder::new(&mut func, blocks[1]);
        let five = build.iconst(I32, 5);
        build.jump(blocks[3], &[five]);
        let mut build = Builder::new(&mut func, blocks[2]);
        let nine = build.iconst(I32, 9);
        build.jump(blocks[3], &[nine]);
        Builder::new(&mut func, blocks[3]).ret(&[]);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        let range = ranges.of(merged);
        assert!(range.contains(5) && range.contains(9), "both arms are in it");
        assert!(!range.contains(7), "and nothing between them is");
    }

    #[test]
    fn a_switch_edge_pins_its_cases_and_the_default_excludes_them() {
        let (mut func, args, blocks) = shape(1, 3);
        let mut build = Builder::new(&mut func, blocks[0]);
        build.switch(args[0], blocks[2], &[(4, blocks[1]), (7, blocks[1])]);
        Builder::new(&mut func, blocks[1]).ret(&[]);
        Builder::new(&mut func, blocks[2]).ret(&[]);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        assert_eq!(ranges.at(args[0], blocks[1]).list(4), Some(vec![4, 7]), "the two cases");
        let fell_through = ranges.at(args[0], blocks[2]);
        assert!(!fell_through.contains(4) && !fell_through.contains(7));
        assert!(fell_through.contains(5), "and everything else is still possible");
    }

    #[test]
    fn both_arms_of_an_and_hold_where_it_is_true() {
        let (mut func, args, blocks) = shape(1, 3);
        let mut build = Builder::new(&mut func, blocks[0]);
        let low = build.iconst(I32, 10);
        let high = build.iconst(I32, 20);
        let above = build.icmp(IntPred::Sgt, args[0], low);
        let below = build.icmp(IntPred::Slt, args[0], high);
        let both = build.binary(Opcode::And, above, below, Flags::NONE);
        build.br_if(both, blocks[1], &[], blocks[2], &[]);
        Builder::new(&mut func, blocks[1]).ret(&[]);
        Builder::new(&mut func, blocks[2]).ret(&[]);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        assert_eq!(bounds(ranges.at(args[0], blocks[1])), Some((11, 19)));
        assert!(ranges.at(args[0], blocks[2]).is_full(), "the false edge says nothing");
    }

    #[test]
    fn a_comparison_the_ranges_settle_is_settled() {
        let (func, x, then, _) = guarded(IntPred::Slt, 10);
        let mut asked = Asked::new(func);
        let ten = {
            let mut build = Builder::new(&mut asked.func, then);
            build.iconst(I32, 10)
        };
        let asked = Asked::new(asked.func);
        let mut ranges = asked.ranges();
        assert_eq!(ranges.compare(IntPred::Slt, x, ten, then), Truth::Always);
        assert_eq!(ranges.compare(IntPred::Sgt, x, ten, then), Truth::Never);
    }

    /// `if (a < b)`, with nothing known about either, which is what the oracle is for.
    ///
    /// Blocks one and two are the arms and block three is where they meet again.
    fn related() -> (Func, Value, Value, Vec<Block>) {
        let (mut func, args, blocks) = shape(2, 4);
        let mut build = Builder::new(&mut func, blocks[0]);
        let test = build.icmp(IntPred::Slt, args[0], args[1]);
        build.br_if(test, blocks[1], &[], blocks[2], &[]);
        Builder::new(&mut func, blocks[1]).jump(blocks[3], &[]);
        Builder::new(&mut func, blocks[2]).jump(blocks[3], &[]);
        Builder::new(&mut func, blocks[3]).ret(&[]);
        (func, args[0], args[1], blocks)
    }

    #[test]
    fn a_relation_the_intervals_cannot_see_is_still_known() {
        let (func, a, b, blocks) = related();
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        // The intervals do learn something from `a < b`, which is that neither is at the end of
        // the type it could not be at. What they cannot do is settle the comparison, and that is
        // what the oracle is here for.
        let (left, right) = (ranges.at(a, blocks[1]), ranges.at(b, blocks[1]));
        assert_eq!(ops::compare(IntPred::Slt, left, right), Truth::Either);
        assert_eq!(ranges.relation(a, b, blocks[1]), Some(IntPred::Slt));
        assert_eq!(ranges.compare(IntPred::Slt, a, b, blocks[1]), Truth::Always);
        assert_eq!(ranges.compare(IntPred::Sge, a, b, blocks[1]), Truth::Never);
        assert_eq!(ranges.compare(IntPred::Ne, a, b, blocks[1]), Truth::Always);
        assert_eq!(ranges.compare(IntPred::Ult, a, b, blocks[1]), Truth::Either);
    }

    #[test]
    fn a_relation_belongs_to_the_block_the_edge_led_to() {
        let (func, a, b, blocks) = related();
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        assert_eq!(ranges.relation(a, b, blocks[1]), Some(IntPred::Slt));
        assert_eq!(ranges.relation(a, b, blocks[2]), Some(IntPred::Sge), "the other edge");
        assert_eq!(ranges.relation(a, b, blocks[3]), None, "where they meet, neither holds");
        assert_eq!(ranges.compare(IntPred::Slt, a, b, blocks[3]), Truth::Either);
    }

    #[test]
    fn one_step_of_composition_is_taken() {
        let (mut func, args, blocks) = shape(3, 4);
        let [a, b, c] = [args[0], args[1], args[2]];
        let mut build = Builder::new(&mut func, blocks[0]);
        let first = build.icmp(IntPred::Slt, a, b);
        build.br_if(first, blocks[1], &[], blocks[3], &[]);
        let mut build = Builder::new(&mut func, blocks[1]);
        let second = build.icmp(IntPred::Sle, b, c);
        build.br_if(second, blocks[2], &[], blocks[3], &[]);
        Builder::new(&mut func, blocks[2]).ret(&[]);
        Builder::new(&mut func, blocks[3]).ret(&[]);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        assert_eq!(ranges.relation(a, c, blocks[2]), Some(IntPred::Slt), "a < b and b <= c");
        assert_eq!(ranges.compare(IntPred::Slt, a, c, blocks[2]), Truth::Always);
    }

    #[test]
    fn the_cache_gives_up_rather_than_growing_without_a_bound() {
        let (func, x, then, otherwise) = guarded(IntPred::Slt, 10);
        let asked = Asked::new(func);
        let options = Options { refinements: 1, ..Options::default() };
        let mut ranges = asked.with(options);
        assert_eq!(bounds(ranges.at(x, then)), Some((i128::from(i32::MIN), 9)));
        assert!(ranges.at(x, otherwise).is_full(), "past the bound it is the definition range");
        assert_eq!(ranges.counts().fallbacks(), 1);
    }

    #[test]
    fn asking_twice_asks_the_cache_the_second_time() {
        let (func, x, then, _) = guarded(IntPred::Slt, 10);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        let first = ranges.at(x, then);
        let hits = ranges.counts().hits();
        let second = ranges.at(x, then);
        assert_eq!(first, second);
        assert!(ranges.counts().hits() > hits, "the second query hit the cache");
        assert_eq!(ranges.counts().queries(), 2);
    }

    #[test]
    fn a_range_that_is_only_true_because_overflow_is_undefined_is_counted() {
        let (mut func, args, blocks) = shape(1, 1);
        let mut build = Builder::new(&mut func, blocks[0]);
        let big = build.iconst(I32, i128::from(i32::MAX) - 4);
        let counted = build.unary(Opcode::Ctlz, args[0], I32);
        let sum = build.binary(Opcode::Add, counted, big, Flags::NSW);
        build.ret(&[]);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        assert!(!ranges.of(sum).is_full(), "the promise not to overflow bounds the sum");
        assert_eq!(ranges.counts().assumed(), 1);
    }

    #[test]
    fn a_query_about_something_that_is_not_an_integer_answers_without_pretending() {
        let (mut func, _, blocks) = shape(0, 1);
        let mut build = Builder::new(&mut func, blocks[0]);
        let mem = build.mem_entry();
        build.ret(&[]);
        let asked = Asked::new(func);
        let mut ranges = asked.ranges();
        assert!(ranges.of(mem).is_full());
        assert_eq!(ranges.counts().full(), 0, "a memory value is not a lost integer");
    }
}
