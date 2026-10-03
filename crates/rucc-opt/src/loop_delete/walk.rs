//! A loop that goes round once for each bit of a value, and how many times that is.
//!
//! Section 20.4 of `spec/optimizer/20-idioms-and-libcalls.md` puts these with the trip count, for
//! the reason gcc does in `number_of_iterations_popcount`. A loop like
//!
//! ```c
//! while (x) { x &= x - 1; n++; }
//! ```
//!
//! has no count [`crate::scev`] can write as a distance between two values, and it still has a
//! count, which is `ctpop` of what `x` came in as. Four loops of this kind are taken here, and what
//! each one takes off the value and what it waits for decide the count:
//!
//! | step | waits for | goes round |
//! |---|---|---|
//! | `x & (x - 1)` | `x` to be zero | `ctpop(x)` |
//! | `x >> 1` | `x` to be zero | `w - ctlz(x)` |
//! | `x >> 1` | the bottom bit to be set | `cttz(x)` |
//! | `x << 1` | the top bit to be set | `ctlz(x)` |
//!
//! The first two come back whatever `x` is, since every step clears a bit and there are only so
//! many. The last two never come back for a zero, so they are taken only where a branch in front
//! of the loop has already said `x` is not one. That is the rule [`super`] has for every loop: what
//! it takes out has to come back, and on a zero these do not.
//!
//! # Which test
//!
//! The test may read the value before the step, which is the `while` loop as written, or after it,
//! which is what [`crate::header_copy`] leaves once it has copied the first test in front of the
//! loop. Before the step the count is the table above. After it, the first value tested is what one
//! step left, so the count is the table applied to that. Where the test in front of the loop passed
//! that is one fewer, which is an offset [`super`] folds into each value it works out, and where
//! nothing says it passed the step is done once in front of the loop and counted instead.
//!
//! # Only where it is one instruction
//!
//! Section 20.6 is the warning. A count with no instruction behind it is written out by the code
//! generator as a dozen shifts and masks and a multiply, and a loop over a value with two bits set
//! goes round twice. gcc 16 turns the first loop into `popcnt` only where it has the instruction
//! and leaves the loop alone otherwise, and this asks the same question through
//! [`crate::machine::Machine::counts_in_one`], which reads the table the code generator reads.

use rucc_ir::{Block, Def, Extra, Func, Inst, InstData, IntPred, Opcode, Value};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::loops::{Exit, LoopId, Loops};

/// What the loop takes off the value each time round, and so what it is counting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// `x & (x - 1)` until nothing is left, which is how many bits are set.
    Lowest,
    /// `x >> 1` until nothing is left, which is how many bits it takes to write `x`.
    Down,
    /// `x >> 1` until the bottom bit is set, which is the zeros below the lowest set bit.
    Trailing,
    /// `x << 1` until the top bit is set, which is the zeros above the highest set bit.
    Leading,
}

impl Step {
    /// The count the answer is built on.
    pub(super) fn opcode(self) -> Opcode {
        match self {
            Step::Lowest => Opcode::Ctpop,
            Step::Down | Step::Leading => Opcode::Ctlz,
            Step::Trailing => Opcode::Cttz,
        }
    }
}

/// What one step of the value is, before it is known what the loop waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// `x & (x - 1)`.
    Lowest,
    /// `x >> 1`, a logical shift. An arithmetic one never gets a negative value to zero.
    Right,
    /// `x << 1`.
    Left,
}

/// What a test says about a value when it comes out the way that keeps the loop going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holds {
    /// It is not zero.
    Nonzero,
    /// Its bottom bit is clear.
    BottomClear,
    /// Its top bit is clear.
    TopClear,
}

/// A loop that walks the bits of one value, and what is needed to say how many times it goes round.
#[derive(Clone, Copy, Debug)]
pub(super) struct Walk {
    /// What it takes off the value.
    pub(super) step: Step,
    /// What the value is on the way in.
    pub(super) from: Value,
    /// Whether the test reads the value after the step rather than before it.
    after: bool,
    /// Whether a branch in front of the loop already made the test pass for the value on the way
    /// in.
    passed: bool,
}

impl Walk {
    /// The width of the value walked, which is the width of the count.
    pub(super) fn bits(self, func: &Func) -> u32 {
        func[self.from].ty.bits()
    }

    /// What has to be added to the count [`count`] writes to get how many times the back edge is
    /// taken.
    ///
    /// Minus one where the test reads after the step and the first one is known to have passed,
    /// because then the first value tested is one step on and the count of it is one fewer. It is
    /// kept apart from the count so that [`super`] can fold it into the base of every value it
    /// works out, and a total that went up by one each time round comes out as the count itself
    /// rather than the count with one taken off and put back.
    pub(super) fn offset(self) -> i128 {
        if self.after && self.passed { -1 } else { 0 }
    }
}

/// The walk this loop is, when it is one this can count and one that comes back.
///
/// `exit` is the loop's one way out and `preheader` is where the value comes in from. Nothing is
/// written here, the same as everything else [`super`] asks before it writes.
pub(super) fn walk(
    func: &Func,
    cfg: &Cfg,
    doms: &Dominators,
    loops: &Loops,
    id: LoopId,
    preheader: Block,
    exit: Exit,
) -> Option<Walk> {
    // One way round. A loop inside this one would run the test more than once each time round,
    // and a second latch would be a second value coming back for the one walked.
    if !loops.children(id).is_empty() {
        return None;
    }
    let &[latch] = loops.latches(id) else { return None };
    let header = loops.header(id);
    // The test runs every time round, which is what lets the times it passed be the count.
    if !doms.dominates(exit.from, latch) {
        return None;
    }
    let term = func.terminator(exit.from)?;
    if func[term].opcode != Opcode::BrIf {
        return None;
    }
    let [cond] = func[func[term].args][..] else { return None };
    let mut targets = func.successors(term).map(|call| call.block);
    let (yes, no) = (targets.next()?, targets.next()?);
    let stays = if no == exit.to && yes != exit.to {
        true
    } else if yes == exit.to && no != exit.to {
        false
    } else {
        return None;
    };
    let (tested, holds) = test(func, cond, stays)?;

    // The value walked is a parameter of the header, and the back edge hands it one step of
    // itself. The test reads either the parameter or that step.
    let back = func.terminator(latch)?;
    let back = func.successors(back).find(|call| call.block == header)?;
    let back = func[back.args].to_vec();
    let params = func[header].params.to_vec();
    let (at, after) = match params.iter().position(|&param| param == tested) {
        Some(at) => (at, false),
        None => {
            let (_, of) = shape(func, tested)?;
            (params.iter().position(|&param| param == of)?, true)
        }
    };
    let next = *back.get(at)?;
    let (shape, of) = shape(func, next)?;
    if of != params[at] || (after && next != tested) {
        return None;
    }
    let step = match (shape, holds) {
        (Shape::Lowest, Holds::Nonzero) => Step::Lowest,
        (Shape::Right, Holds::Nonzero) => Step::Down,
        (Shape::Right, Holds::BottomClear) => Step::Trailing,
        (Shape::Left, Holds::TopClear) => Step::Leading,
        _ => return None,
    };

    let entry = func.terminator(preheader)?;
    let entry = func.successors(entry).find(|call| call.block == header)?;
    let from = *func[entry.args].get(at)?;
    let passed = known(func, cfg, doms, preheader, from, holds);
    let nonzero = (holds == Holds::Nonzero && passed)
        || number(func, from).is_some_and(|it| it != 0)
        || known(func, cfg, doms, preheader, from, Holds::Nonzero);
    if matches!(step, Step::Trailing | Step::Leading) {
        // A zero never sets the bit waited for, so the loop has to be known to start on something
        // else. Testing after the step needs the first test to have passed as well, since that is
        // what says the value one step on is not zero either.
        if !nonzero || (after && !passed) {
            return None;
        }
    }
    Some(Walk { step, from, after, passed })
}

/// Which step `next` is and what it is a step of.
fn shape(func: &Func, next: Value) -> Option<(Shape, Value)> {
    let Def::Result { inst, .. } = func[next].def else { return None };
    let [left, right] = func[func[inst].args][..] else { return None };
    let ty = func[next].ty;
    if !ty.is_int() || ty.is_vector() {
        return None;
    }
    match func[inst].opcode {
        Opcode::LShr if number(func, right) == Some(1) => Some((Shape::Right, left)),
        Opcode::Shl if number(func, right) == Some(1) => Some((Shape::Left, left)),
        Opcode::And if less_one(func, right) == Some(left) => Some((Shape::Lowest, left)),
        Opcode::And if less_one(func, left) == Some(right) => Some((Shape::Lowest, right)),
        _ => None,
    }
}

/// What `value` is one less than, when it is `x - 1` or `x + -1`.
fn less_one(func: &Func, value: Value) -> Option<Value> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let [left, right] = func[func[inst].args][..] else { return None };
    match (func[inst].opcode, number(func, right)) {
        (Opcode::Sub, Some(1)) | (Opcode::Add, Some(-1)) => Some(left),
        _ => None,
    }
}

/// The number a value is, read as signed in its own width.
fn number(func: &Func, value: Value) -> Option<i128> {
    crate::fold::constant(func, value).map(|(imm, ty)| imm.signed(ty))
}

/// What `cond` coming out `taken` says about one value, when it is one of the things a walk waits
/// on.
fn test(func: &Func, cond: Value, taken: bool) -> Option<(Value, Holds)> {
    let Def::Result { inst, .. } = func[cond].def else { return None };
    if func[inst].opcode != Opcode::ICmp {
        return None;
    }
    let Extra::IntPred(pred) = func[inst].extra else { return None };
    let [left, right] = func[func[inst].args][..] else { return None };
    let right = number(func, right)?;
    match (pred, right, taken) {
        (IntPred::Ne, 0, true) | (IntPred::Eq, 0, false) => Some((left, Holds::Nonzero)),
        (IntPred::Eq, 0, true) | (IntPred::Ne, 0, false) => masked(func, left),
        (IntPred::Sge, 0, true)
        | (IntPred::Sgt, -1, true)
        | (IntPred::Slt, 0, false)
        | (IntPred::Sle, -1, false) => Some((left, Holds::TopClear)),
        _ => None,
    }
}

/// Which bit `x & mask` being zero says is clear, when it is the bottom one or the top one.
fn masked(func: &Func, value: Value) -> Option<(Value, Holds)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::And {
        return None;
    }
    let [left, right] = func[func[inst].args][..] else { return None };
    let top = -(1i128 << (func[value].ty.bits() - 1));
    match number(func, right)? {
        1 => Some((left, Holds::BottomClear)),
        mask if mask == top => Some((left, Holds::TopClear)),
        _ => None,
    }
}

/// Whether a branch every way into `at` goes through says this of `value`.
///
/// Up the dominator tree from `at`, and a branch counts when one of its edges is the only way into
/// a block that dominates `at`, since then every road to `at` took that edge. That is the shape
/// [`crate::header_copy`] leaves, with the copied test in front of the loop and the preheader on
/// its passing edge, and it is the shape of an `if (!x) return` above the loop as well.
fn known(func: &Func, cfg: &Cfg, doms: &Dominators, at: Block, value: Value, holds: Holds) -> bool {
    let mut below = at;
    while let Some(above) = doms.immediate_dominator(below) {
        if let Some((cond, taken)) = edge(func, cfg, doms, above, at) {
            if test(func, cond, taken) == Some((value, holds)) {
                return true;
            }
        }
        below = above;
    }
    false
}

/// The condition `above` branches on and which way it went, when one of its edges is the only way
/// to `at`.
fn edge(
    func: &Func,
    cfg: &Cfg,
    doms: &Dominators,
    above: Block,
    at: Block,
) -> Option<(Value, bool)> {
    let term = func.terminator(above)?;
    if func[term].opcode != Opcode::BrIf {
        return None;
    }
    let [cond] = func[func[term].args][..] else { return None };
    let mut targets = func.successors(term).map(|call| call.block);
    let (yes, no) = (targets.next()?, targets.next()?);
    if yes == no {
        return None;
    }
    let only = |to: Block| cfg.predecessors(to) == [above] && doms.dominates(to, at);
    if only(yes) {
        Some((cond, true))
    } else if only(no) {
        Some((cond, false))
    } else {
        None
    }
}

/// The count of the walk, worked out in front of `before`, without its [`Walk::offset`].
pub(super) fn count(func: &mut Func, before: Inst, walk: Walk) -> Value {
    let ty = func[walk.from].ty;
    // One step of the value, where the test reads after the step and nothing said the first test
    // passed. The count of what one step left is then the count, whatever the value came in as,
    // and a zero stays a zero and is counted as no times round.
    let on = if walk.after && !walk.passed { one_step(func, before, walk) } else { walk.from };
    let args = func.push_values(&[on]);
    let counted =
        super::made(func, before, InstData { args, ..InstData::new(walk.step.opcode()) }, ty);
    if walk.step != Step::Down {
        return counted;
    }
    let width = crate::ivopts::number(func, before, ty, i128::from(ty.bits()));
    super::arith(func, before, Opcode::Sub, width, counted, ty)
}

/// One step of the value on the way in, which only the two walks that come back on a zero need.
fn one_step(func: &mut Func, before: Inst, walk: Walk) -> Value {
    let ty = func[walk.from].ty;
    let one = crate::ivopts::number(func, before, ty, 1);
    match walk.step {
        Step::Lowest => {
            let less = super::arith(func, before, Opcode::Sub, walk.from, one, ty);
            super::arith(func, before, Opcode::And, walk.from, less, ty)
        }
        Step::Down => super::arith(func, before, Opcode::LShr, walk.from, one, ty),
        Step::Trailing | Step::Leading => {
            unreachable!("a walk that never comes back on a zero is taken only where it passed")
        }
    }
}
