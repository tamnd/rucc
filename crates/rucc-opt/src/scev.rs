//! Scalar evolution: how a value changes across the iterations of a loop, and how many
//! iterations there are.
//!
//! Design: `spec/optimizer/07-loops-and-scev.md` sections 7.4 through 7.7. This is the second
//! half of document 07 and it answers the last two of the four questions section 7.6 says loop
//! analysis exists for. The first two are in [`crate::loops`].
//!
//! # Chains of recurrences, and how much of one
//!
//! GCC writes how a value changes as a chain of recurrences, `{base, +, step}`, meaning a value
//! that is `base` on the first iteration and `step` more on each one after. The representation is
//! good because it is closed under the operations anyone wants: adding two chrecs of the same
//! loop adds componentwise, multiplying by something invariant scales both parts, and evaluating
//! one at a given iteration is arithmetic rather than a special case. That closure is why
//! `j = 2 * i + 3` is as easy as `i = i + 1`, and pattern matching the second would run out of
//! road on the first.
//!
//! Section 7.4 says what rucc builds and it is a subset: affine chrecs only. A value is
//! invariant, or `{base, +, step}` with both parts invariant, or unknown. Addition, subtraction,
//! multiplication by an invariant, shifting by a constant, and extension where the extension
//! provably does not wrap. Nothing polynomial and nothing mutually recursive. That covers every
//! induction variable a C programmer writes and every array subscript document 31 could use, and
//! what it leaves out of GCC's four thousand lines is the part serving Fortran and the polyhedral
//! framework.
//!
//! The one extension past affine is pointer chrecs, because C loops walk pointers and `p = p + 1`
//! is `i = i + 1` with a scale. A `ptr_add` is addition with the byte offset as the step, which
//! is the difference between analysing half of real C loops and analysing nearly all of them.
//!
//! # Trip counts, and the part that is uncomfortable
//!
//! Given an exit that compares an affine chrec against something invariant, solving for the
//! iteration at which the comparison first fails is arithmetic. What makes it hard is that the
//! answer is almost always conditional: on the loop being entered at all, and on the induction
//! variable not wrapping before it gets there. Section 7.5 says a trip count returned without its
//! assumptions is a miscompilation generator, and that the temptation to return one is strong
//! because the assumptions are usually true.
//!
//! So [`Bound`] carries them and there is no way to read the count without seeing them.
//! [`Bound::parts`] hands back both, and [`Bound::proven`] hands back the count only when there
//! is nothing left to prove. A caller that means to emit a runtime check reads the assumptions
//! and emits it, and a caller that forgets cannot get at the number.
//!
//! [`Bound`] and [`Estimate`] are different types on purpose. A bound is used for correctness, an
//! estimate is used to decide whether a transformation is worth doing, and section 7.5 calls
//! conflating them a category error that costs correctness. GCC keeps them apart as
//! `max_loop_iterations` and `estimate_numbers_of_iterations` and the names do not stop anyone.
//! Different structs do.

use std::collections::HashMap;

use rucc_ir::{Block, Def, Extra, Flags, Func, Imm, Inst, IntPred, Opcode, Type, Value};

use crate::cfg::Cfg;
use crate::loops::{LoopId, Loops};

/// How deep the search for a step walks back through arithmetic.
///
/// The chain from a header parameter to the value fed back to it is two or three instructions in
/// anything a person writes, and the walk terminates on its own because SSA has no cycles except
/// through block parameters. The limit is here so a generated function with a thousand additions
/// in the increment costs a bounded amount rather than a stack.
const STEP_LIMIT: u32 = 16;

/// How many times a loop is assumed to run when nothing better is known.
///
/// GCC's `--param avg-loop-niter`, whose default is the same number. It is a guess and it is only
/// ever used through [`Estimate`], which is only ever used to decide whether something is worth
/// doing.
const ASSUMED_ITERATIONS: u64 = 10;

/// A value that does not change inside the loop, read as `scale * value + offset`.
///
/// The `value` is a value defined outside the loop, or `None` when the expression is a plain
/// number. Keeping the shape rather than a bare [`Value`] is what lets `j = 2 * i + 3` come out
/// as `{3, +, 2}` instead of unknown: the base and the step of that chrec are expressions nothing
/// in the function computes, so a representation that could only name existing values would have
/// to give up.
///
/// Arithmetic on two of these is refused when both are symbolic and the symbols differ, because
/// `x + y` is not of this shape. That is the boundary of the subset and it is where the answer
/// becomes unknown rather than wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invariant {
    /// What it is built on, or `None` for a plain number.
    pub value: Option<Value>,
    /// How many of it.
    pub scale: i128,
    /// What is added to it.
    pub offset: i128,
}

impl Invariant {
    /// A plain number.
    #[must_use]
    pub fn number(offset: i128) -> Self {
        Self { value: None, scale: 0, offset }
    }

    /// One of a value.
    #[must_use]
    pub fn of(value: Value) -> Self {
        Self { value: Some(value), scale: 1, offset: 0 }
    }

    /// The number this is, when it is one.
    #[must_use]
    pub fn as_number(self) -> Option<i128> {
        (self.value.is_none() || self.scale == 0).then_some(self.offset)
    }

    /// Whether this is the number zero.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.as_number() == Some(0)
    }

    /// The symbol both expressions are built on, when they agree on one or one has none.
    fn shared(self, other: Self) -> Option<Option<Value>> {
        match (self.as_number().is_some(), other.as_number().is_some()) {
            (true, _) => Some(other.value),
            (_, true) => Some(self.value),
            _ => (self.value == other.value).then_some(self.value),
        }
    }

    /// The two added, when the sum is of this shape.
    #[must_use]
    pub fn plus(self, other: Self) -> Option<Self> {
        let value = self.shared(other)?;
        Some(Self {
            value,
            scale: self.scale.checked_add(other.scale)?,
            offset: self.offset.checked_add(other.offset)?,
        })
    }

    /// The second subtracted from the first, when the difference is of this shape.
    #[must_use]
    pub fn minus(self, other: Self) -> Option<Self> {
        self.plus(other.negated()?)
    }

    /// This with its sign flipped.
    #[must_use]
    pub fn negated(self) -> Option<Self> {
        Some(Self {
            value: self.value,
            scale: self.scale.checked_neg()?,
            offset: self.offset.checked_neg()?,
        })
    }

    /// The two multiplied, which needs one of them to be a plain number.
    #[must_use]
    pub fn times(self, other: Self) -> Option<Self> {
        let (symbol, by) = match (self.as_number(), other.as_number()) {
            (Some(by), _) => (other, by),
            (_, Some(by)) => (self, by),
            _ => return None,
        };
        Some(Self {
            value: symbol.value,
            scale: symbol.scale.checked_mul(by)?,
            offset: symbol.offset.checked_mul(by)?,
        })
    }
}

/// How a value changes from one iteration of a loop to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evolution {
    /// The same on every iteration.
    Invariant(Invariant),
    /// `{base, +, step}`: `base` the first time round and `step` more each time after.
    Affine(Chrec),
    /// Not something this analysis describes. Never a claim that the value does not evolve.
    Unknown,
}

impl Evolution {
    /// The chrec, when this is one.
    #[must_use]
    pub fn chrec(self) -> Option<Chrec> {
        match self {
            Self::Affine(chrec) => Some(chrec),
            _ => None,
        }
    }

    /// The invariant expression, when this is one.
    #[must_use]
    pub fn invariant(self) -> Option<Invariant> {
        match self {
            Self::Invariant(inv) => Some(inv),
            _ => None,
        }
    }
}

/// An affine chain of recurrences, `{base, +, step}`, evolving in a named type.
///
/// The type is not decoration. `{0, +, 1}` in `unsigned char` is not the sequence `0, 1, 2, ...`,
/// it is that sequence modulo two hundred and fifty six, and section 7.7 says this is where a
/// naive implementation is wrong constantly and in ways that pass every test written by someone
/// thinking in `int`. Every operation here checks the type and every one that cannot stay right
/// in it answers unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrec {
    /// What the value is on the first iteration.
    pub base: Invariant,
    /// What is added each time round.
    pub step: Invariant,
    /// The type it evolves in, which is what says when it wraps.
    pub ty: Type,
    /// What the instruction that increments it promised. `nsw` means the sequence does not wrap
    /// when read as signed and `nuw` means it does not when read as unsigned, and both come from
    /// the increment rather than from anything this analysis proved.
    pub flags: Flags,
}

impl Chrec {
    /// Whether the sequence is known not to wrap under the reading this predicate takes.
    #[must_use]
    pub fn does_not_wrap(self, signed: bool) -> bool {
        self.flags.contains(if signed { Flags::NSW } else { Flags::NUW })
    }
}

/// Something that has to be true for a trip count to be the right answer.
///
/// Section 7.5 asks for exactly this: not a trip count but a trip count plus a predicate under
/// which it holds, so the consumer either proves the predicate, emits a runtime check for it, or
/// gives up. These are the predicates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assumption {
    /// The counter starts on the near side of its limit, so the distance between them is a
    /// number that is not negative.
    ///
    /// For a loop ending on an ordering this is the loop being entered at all.
    /// `for (i = 0; i < n; i++)` with `n` of zero runs no times and the distance is zero, but `n`
    /// of minus one also runs no times and the distance is minus one, so a count taken from the
    /// distance has to be told which case it is in. For a loop ending on `!=` it is the limit
    /// being somewhere the counter is heading, because one stepping away from its limit never
    /// arrives.
    ///
    /// Only ever present on a symbolic count. When the distance is a number the sign of it is
    /// there to be read, so this is settled rather than assumed.
    Approaching,
    /// The induction variable does not wrap in its own type before the exit is taken.
    ///
    /// Present whenever the increment did not carry the matching `nsw` or `nuw` flag. With the
    /// flag there is nothing to assume, because the flag is the promise.
    NoWrap(Chrec),
    /// Signed overflow is undefined here, which is what makes `for (int i = 0; i <= n; i++)`
    /// finite.
    ///
    /// GCC infers loop bounds from this in `infer_loop_bounds_from_signedness`, and it is the
    /// single most common source of a report that the compiler broke a working program. It is
    /// recorded rather than assumed silently so that `-fwrapv` can withdraw the count and so that
    /// a dump can name it.
    StrictOverflow,
}

impl Assumption {
    /// What it says, in a line, for a dump to print.
    ///
    /// Section 7.5 asks that every inference of this kind be dumpable and say what it rests on,
    /// because a user who has been bitten by one deserves a command that tells them which line
    /// the compiler used against them. This is the sentence that command prints.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Approaching => "the counter starts on the near side of its limit".to_string(),
            Self::NoWrap(chrec) => {
                format!("the induction variable does not wrap in i{}", chrec.ty.bits())
            }
            Self::StrictOverflow => {
                "signed overflow is undefined, so -fwrapv withdraws this count".to_string()
            }
        }
    }
}

/// How many iterations, as a number or as an expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// Exactly this many.
    Exact(u128),
    /// This many, worked out from something the loop does not change.
    Symbolic(Invariant),
}

/// How many times a loop runs at most, and what that rests on.
///
/// For correctness. A pass that deletes an iteration, peels one off, or decides a memory access
/// is in bounds needs one of these. The count cannot be read without the assumptions, which is
/// section 7.7's defence against a caller proving two of three and forgetting the third.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    count: Count,
    assumptions: Vec<Assumption>,
}

impl Bound {
    /// The count and everything it rests on, together, because they cannot be asked for apart.
    #[must_use]
    pub fn parts(&self) -> (Count, &[Assumption]) {
        (self.count, &self.assumptions)
    }

    /// What has to be proved before the count means anything.
    #[must_use]
    pub fn assumptions(&self) -> &[Assumption] {
        &self.assumptions
    }

    /// The count, for a caller with nothing left to prove.
    ///
    /// `None` does not mean the count is unknown. It means there are assumptions and this is not
    /// the accessor for reading a count that has them.
    #[must_use]
    pub fn proven(&self) -> Option<Count> {
        self.assumptions.is_empty().then_some(self.count)
    }
}

/// How many times a loop probably runs.
///
/// For cost decisions and never for correctness. A pass asking whether unrolling pays for itself
/// wants one of these, and it is fine for the answer to be a guess, because being wrong makes the
/// code slower rather than wrong. Nothing here can be turned into a [`Bound`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Estimate {
    iterations: u64,
    guessed: bool,
}

impl Estimate {
    /// The number to do arithmetic with.
    #[must_use]
    pub fn iterations(self) -> u64 {
        self.iterations
    }

    /// Whether nothing was known and this is the default.
    #[must_use]
    pub fn is_guess(self) -> bool {
        self.guessed
    }
}

/// The analysis, which works out an answer when asked and remembers it.
///
/// Demand driven and memoized, per section 7.8, because the cost of scalar evolution is a
/// function of how many distinct values get asked about rather than of the size of the function.
/// The cache holds one loop's worth of answers per loop and the whole thing is thrown away when
/// anything about the loops changes, which per document 04.4 is any pass that touches one.
#[derive(Debug)]
pub struct Scev<'a> {
    func: &'a Func,
    cfg: &'a Cfg,
    loops: &'a Loops,
    known: HashMap<(LoopId, Value), Evolution>,
}

impl<'a> Scev<'a> {
    /// A fresh analysis over these loops, knowing nothing yet.
    #[must_use]
    pub fn new(func: &'a Func, cfg: &'a Cfg, loops: &'a Loops) -> Self {
        Self { func, cfg, loops, known: HashMap::new() }
    }

    /// How this value changes across the iterations of this loop.
    pub fn evolution(&mut self, id: LoopId, value: Value) -> Evolution {
        if let Some(&known) = self.known.get(&(id, value)) {
            return known;
        }
        // Unknown while the answer is being worked out, so the cycle from a header parameter back
        // to itself terminates instead of asking the same question forever. Anything that reaches
        // the parameter again gets unknown and the shape it was matching fails, which is the
        // right answer for a value defined in terms of itself through arithmetic this does not
        // describe.
        self.known.insert((id, value), Evolution::Unknown);
        let found = self.compute(id, value);
        self.known.insert((id, value), found);
        found
    }

    /// How many times this loop runs at most, and what that rests on.
    ///
    /// Any one exit gives a valid upper bound, because a loop cannot run more times than the
    /// first exit that fires, so this takes the first exit it can solve rather than the smallest.
    /// That is `max_loop_iterations` and not `estimate_numbers_of_iterations`, which is why the
    /// answer is a [`Bound`].
    pub fn bound(&mut self, id: LoopId) -> Option<Bound> {
        let exits: Vec<Block> = self.loops.exits(id).iter().map(|exit| exit.from).collect();
        exits.into_iter().find_map(|from| self.bound_at(id, from))
    }

    /// How many times this loop probably runs.
    pub fn estimate(&mut self, id: LoopId) -> Estimate {
        match self.bound(id).map(|bound| bound.count) {
            Some(Count::Exact(exact)) => {
                Estimate { iterations: u64::try_from(exact).unwrap_or(u64::MAX), guessed: false }
            }
            _ => Estimate { iterations: ASSUMED_ITERATIONS, guessed: true },
        }
    }

    /// The evolution of a value nothing is known about yet.
    fn compute(&mut self, id: LoopId, value: Value) -> Evolution {
        if let Some(invariant) = self.invariant(id, value) {
            return Evolution::Invariant(invariant);
        }
        match self.func[value].def {
            Def::Param { block, index } if block == self.loops.header(id) => {
                self.at_header(id, value, index as usize)
            }
            // A parameter of a block inside the loop that is not the header takes a different
            // value depending on which way control came, and describing that is a job for the
            // value range work of document 10 rather than for a chrec.
            Def::Param { .. } => Evolution::Unknown,
            Def::Result { inst, .. } => self.at_inst(id, inst, value),
        }
    }

    /// The value as an expression that does not change inside the loop, if it is one.
    fn invariant(&self, id: LoopId, value: Value) -> Option<Invariant> {
        if let Some((imm, ty)) = constant(self.func, value) {
            return Some(Invariant::number(imm.signed(ty)));
        }
        // A constant is invariant wherever it sits, which is why it is asked about first. Anything
        // else has to be defined outside the loop.
        self.loops.is_invariant(self.func, id, value).then(|| Invariant::of(value))
    }

    /// The evolution of a parameter of the loop header, which is where an induction variable is.
    ///
    /// The parameter takes one value on the way in and another on the way round, which is what
    /// other IRs spell as a phi node. If the way round is the parameter plus something invariant,
    /// the parameter is an affine chrec and that something is its step.
    fn at_header(&mut self, id: LoopId, value: Value, index: usize) -> Evolution {
        let (func, cfg, loops) = (self.func, self.cfg, self.loops);
        let header = loops.header(id);
        // Section 7.3 wants exactly one latch and the canonicalizer makes one. Two of them means
        // two ways round with two different increments, and picking one would be a guess.
        let [latch] = loops.latches(id) else { return Evolution::Unknown };
        let mut entering = None;
        let mut around = None;
        for &pred in cfg.predecessors(header) {
            let Some(arg) = argument(func, pred, header, index) else { return Evolution::Unknown };
            let slot = if pred == *latch { &mut around } else { &mut entering };
            if slot.replace(arg).is_some_and(|old| old != arg) {
                return Evolution::Unknown;
            }
        }
        let (Some(entering), Some(around)) = (entering, around) else { return Evolution::Unknown };
        let Some(base) = self.invariant(id, entering) else { return Evolution::Unknown };
        let Some((step, flags)) = self.step(id, around, value, 0) else {
            return Evolution::Unknown;
        };
        affine(base, step, func[value].ty, flags)
    }

    /// What is added to `of` to get `value`, and what the additions promised.
    ///
    /// Written as its own walk rather than as the general combination below, because at the point
    /// this runs the parameter's own evolution is not known yet and the general walk would ask
    /// for it and get unknown.
    fn step(&self, id: LoopId, value: Value, of: Value, depth: u32) -> Option<(Invariant, Flags)> {
        if value == of {
            // Nothing added yet, and nothing has had a chance to overflow either.
            return Some((Invariant::number(0), Flags::NSW.union(Flags::NUW)));
        }
        if depth >= STEP_LIMIT {
            return None;
        }
        let Def::Result { inst, .. } = self.func[value].def else { return None };
        let data = &self.func[inst];
        let args = &self.func[data.args];
        let (&lhs, &rhs) = (args.first()?, args.get(1)?);
        let combine = |carried: (Invariant, Flags), other: Invariant, subtract: bool| {
            let (delta, flags) = carried;
            let moved = if subtract { delta.minus(other)? } else { delta.plus(other)? };
            Some((moved, flags.intersection(data.flags)))
        };
        match data.opcode {
            Opcode::Add => {
                if let Some(carried) = self.step(id, lhs, of, depth + 1) {
                    return combine(carried, self.invariant(id, rhs)?, false);
                }
                combine(self.step(id, rhs, of, depth + 1)?, self.invariant(id, lhs)?, false)
            }
            Opcode::Sub => {
                combine(self.step(id, lhs, of, depth + 1)?, self.invariant(id, rhs)?, true)
            }
            // A pointer walks by bytes, and only the pointer side can be the one carrying the
            // induction variable. The offset is the step, which is the element size the front end
            // already multiplied in.
            Opcode::PtrAdd => {
                combine(self.step(id, lhs, of, depth + 1)?, self.invariant(id, rhs)?, false)
            }
            _ => None,
        }
    }

    /// The evolution of an instruction's result, from the evolutions of its operands.
    fn at_inst(&mut self, id: LoopId, inst: Inst, value: Value) -> Evolution {
        let func = self.func;
        let data = &func[inst];
        let (opcode, flags) = (data.opcode, data.flags);
        let args = &func[data.args];
        let ty = func[value].ty;
        let Some(&lhs) = args.first() else { return Evolution::Unknown };
        match opcode {
            Opcode::Add | Opcode::PtrAdd => {
                let Some(&rhs) = args.get(1) else { return Evolution::Unknown };
                let (left, right) = (self.evolution(id, lhs), self.evolution(id, rhs));
                combine(left, right, ty, flags, false)
            }
            Opcode::Sub => {
                let Some(&rhs) = args.get(1) else { return Evolution::Unknown };
                let (left, right) = (self.evolution(id, lhs), self.evolution(id, rhs));
                combine(left, right, ty, flags, true)
            }
            Opcode::Mul => {
                let Some(&rhs) = args.get(1) else { return Evolution::Unknown };
                let (left, right) = (self.evolution(id, lhs), self.evolution(id, rhs));
                scale(left, right, ty, flags)
            }
            // A shift by a constant is a multiplication by a power of two, and only by a constant:
            // a variable count is invariant in the loop and still not a number this can multiply
            // by. A count at or above the width is poison rather than a shift to zero, so the
            // range is checked here rather than assumed.
            Opcode::Shl => {
                let Some(&rhs) = args.get(1) else { return Evolution::Unknown };
                let Some((count, count_ty)) = constant(func, rhs) else {
                    return Evolution::Unknown;
                };
                let count = count.unsigned();
                if count >= u128::from(ty.bits()) || !count_ty.is_int() {
                    return Evolution::Unknown;
                }
                let by = Evolution::Invariant(Invariant::number(1i128 << count));
                scale(self.evolution(id, lhs), by, ty, flags)
            }
            Opcode::SExt | Opcode::ZExt => self.extend(id, opcode, lhs, ty),
            // A truncation is a wrap by construction, so a chrec through one describes a sequence
            // that restarts, and this does not have a representation for that.
            _ => Evolution::Unknown,
        }
    }

    /// A chrec widened, which needs the sequence not to wrap at the narrow width.
    ///
    /// Section 7.4 allows extension only where the extension provably does not wrap, and the
    /// proof here is the flag the increment carries. `nsw` on the increment is the promise that
    /// the signed sequence does not wrap, which is exactly what makes the wide sequence the same
    /// numbers as the narrow one.
    ///
    /// Both parts have to be plain numbers. A symbolic base or step is a value of the narrow type
    /// and the widened chrec would need it widened too, which is an expression nothing computes
    /// and which [`Invariant`] has no room to describe. Saying so is the honest answer, the case
    /// that matters most is a counter from a constant by a constant, and lifting the restriction
    /// is work for whoever needs a symbolic one.
    fn extend(&mut self, id: LoopId, opcode: Opcode, from: Value, to: Type) -> Evolution {
        let narrow = self.func[from].ty;
        let signed = opcode == Opcode::SExt;
        match self.evolution(id, from) {
            Evolution::Invariant(inv) => match inv.as_number() {
                // A number read at the narrow width means the same thing at the wide one under
                // sign extension, and under zero extension once it is not negative.
                Some(number) if signed || number >= 0 => Evolution::Invariant(inv),
                _ => Evolution::Unknown,
            },
            Evolution::Affine(chrec) if chrec.ty == narrow && chrec.does_not_wrap(signed) => {
                let (Some(base), Some(step)) = (chrec.base.as_number(), chrec.step.as_number())
                else {
                    return Evolution::Unknown;
                };
                Evolution::Affine(Chrec {
                    base: Invariant::number(base),
                    step: Invariant::number(step),
                    ty: to,
                    flags: chrec.flags,
                })
            }
            _ => Evolution::Unknown,
        }
    }

    /// The trip count from the exit leaving this block, if this exit can be solved.
    fn bound_at(&mut self, id: LoopId, from: Block) -> Option<Bound> {
        let func = self.func;
        let term = func.terminator(from)?;
        if func[term].opcode != Opcode::BrIf {
            return None;
        }
        let args = &func[func[term].args];
        let &cond = args.first()?;
        let calls = &func[func.target_list(term)];
        let (&taken, &not_taken) = (calls.first()?, calls.get(1)?);
        // Which arm keeps going. If both stay in or both leave, the branch is not the test that
        // ends the loop and there is nothing here to solve.
        let stays = match (
            self.loops.contains(id, taken.block),
            self.loops.contains(id, not_taken.block),
        ) {
            (true, false) => true,
            (false, true) => false,
            _ => return None,
        };

        let Def::Result { inst, .. } = func[cond].def else { return None };
        if func[inst].opcode != Opcode::ICmp {
            return None;
        }
        let Extra::IntPred(pred) = func[inst].extra else { return None };
        // The loop keeps going while the test says so, so an exit taken when the test is true is
        // an exit whose continuing condition is the opposite one.
        let pred = if stays { pred } else { invert(pred) };
        let operands = &func[func[inst].args];
        let (&lhs, &rhs) = (operands.first()?, operands.get(1)?);

        // One side evolves and the other does not. Swapping puts the one that evolves on the left
        // and turns the predicate round with it, so only one direction has to be solved.
        let (chrec, limit, pred) = match (self.evolution(id, lhs), self.evolution(id, rhs)) {
            (Evolution::Affine(chrec), other) => (chrec, other.invariant()?, pred),
            (other, Evolution::Affine(chrec)) => (chrec, other.invariant()?, swap(pred)),
            _ => return None,
        };
        solve(chrec, limit, pred)
    }
}

/// Two evolutions added, or subtracted when asked.
fn combine(left: Evolution, right: Evolution, ty: Type, flags: Flags, subtract: bool) -> Evolution {
    let apply = |a: Invariant, b: Invariant| if subtract { a.minus(b) } else { a.plus(b) };
    match (left, right) {
        (Evolution::Invariant(a), Evolution::Invariant(b)) => {
            apply(a, b).map_or(Evolution::Unknown, Evolution::Invariant)
        }
        (Evolution::Affine(chrec), Evolution::Invariant(b)) => {
            // Adding something that does not move only moves the base.
            let Some(base) = apply(chrec.base, b) else { return Evolution::Unknown };
            affine(base, chrec.step, ty, flags.intersection(chrec.flags))
        }
        (Evolution::Invariant(a), Evolution::Affine(chrec)) => {
            let (Some(base), Some(step)) = (
                apply(a, chrec.base),
                if subtract { chrec.step.negated() } else { Some(chrec.step) },
            ) else {
                return Evolution::Unknown;
            };
            affine(base, step, ty, flags.intersection(chrec.flags))
        }
        (Evolution::Affine(a), Evolution::Affine(b)) => {
            // Two chrecs of the same loop add componentwise, which is the closure property that
            // makes the representation worth having. Of different types they do not, because the
            // two sequences wrap at different widths.
            if a.ty != b.ty {
                return Evolution::Unknown;
            }
            let (Some(base), Some(step)) = (apply(a.base, b.base), apply(a.step, b.step)) else {
                return Evolution::Unknown;
            };
            affine(base, step, ty, flags.intersection(a.flags).intersection(b.flags))
        }
        _ => Evolution::Unknown,
    }
}

/// One evolution multiplied by another, which needs one of them to stand still.
fn scale(left: Evolution, right: Evolution, ty: Type, flags: Flags) -> Evolution {
    let (chrec, by) = match (left, right) {
        (Evolution::Invariant(a), Evolution::Invariant(b)) => {
            return a.times(b).map_or(Evolution::Unknown, Evolution::Invariant);
        }
        (Evolution::Affine(chrec), Evolution::Invariant(by))
        | (Evolution::Invariant(by), Evolution::Affine(chrec)) => (chrec, by),
        // Two chrecs multiplied give a quadratic, which is a chain of recurrences with a second
        // step and is outside the subset section 7.4 chose.
        _ => return Evolution::Unknown,
    };
    let (Some(base), Some(step)) = (chrec.base.times(by), chrec.step.times(by)) else {
        return Evolution::Unknown;
    };
    affine(base, step, ty, flags.intersection(chrec.flags))
}

/// A chrec, or invariant when the step turns out to be nothing.
///
/// A step of zero is a valid affine chrec describing a value that does not move, and section 7.7
/// warns that code dividing by the step to get a trip count divides by zero. Reporting it as
/// invariant here means the shape is right for every reader rather than only for the careful
/// ones, and the trip count solver still checks, because a step can also come out zero from a
/// header parameter incremented by an invariant that happens to be zero.
fn affine(base: Invariant, step: Invariant, ty: Type, flags: Flags) -> Evolution {
    if step.is_zero() {
        return Evolution::Invariant(base);
    }
    Evolution::Affine(Chrec { base, step, ty, flags })
}

/// The iteration at which `chrec pred limit` first fails, with what that rests on.
fn solve(chrec: Chrec, limit: Invariant, pred: IntPred) -> Option<Bound> {
    // Section 7.7's first way of being wrong. A step of zero is a loop that never leaves through
    // this exit, and dividing the distance by it is a crash rather than an answer.
    let step = chrec.step.as_number()?;
    if step == 0 {
        return None;
    }
    let signed = matches!(pred, IntPred::Slt | IntPred::Sle | IntPred::Sgt | IntPred::Sge);

    let mut assumptions = Vec::new();
    if !chrec.does_not_wrap(signed) {
        assumptions.push(Assumption::NoWrap(chrec));
    }
    if signed {
        assumptions.push(Assumption::StrictOverflow);
    }

    // A test that does not read its operands as signed does not read the constants in them that
    // way either, and every constant reaching here was read as signed on the way in.
    let (base, limit) = if signed {
        (chrec.base, limit)
    } else {
        (as_unsigned(chrec.base, chrec.ty)?, as_unsigned(limit, chrec.ty)?)
    };

    // The distance the counter has to travel, always counting up. A loop going down is the same
    // problem with the ends swapped, which is why the step is used by size below and its sign is
    // spent here.
    let apart = step.unsigned_abs();
    match (pred, step > 0) {
        (IntPred::Slt | IntPred::Ult, true) => {
            ordered(limit.minus(base)?, apart, false, assumptions)
        }
        (IntPred::Sle | IntPred::Ule, true) => {
            ordered(limit.minus(base)?, apart, true, assumptions)
        }
        (IntPred::Sgt | IntPred::Ugt, false) => {
            ordered(base.minus(limit)?, apart, false, assumptions)
        }
        (IntPred::Sge | IntPred::Uge, false) => {
            ordered(base.minus(limit)?, apart, true, assumptions)
        }
        (IntPred::Ne, _) => {
            let distance = if step > 0 { limit.minus(base)? } else { base.minus(limit)? };
            landing(distance, apart, assumptions)
        }
        // Either the counter steps away from the limit, in which case the loop is endless rather
        // than long, or the test is one this does not solve. Silence is the answer to both.
        _ => None,
    }
}

/// The same expression, read the way a test without a sign reads it.
///
/// Constants arrive here as the number their bits are when the sign bit is taken seriously,
/// because that is the only reading available before anybody knows what will be done with them.
/// An unsigned test disagrees about half of them. `for (unsigned char i = 0; i < 200; i++)` holds
/// its limit as minus fifty six, and a distance worked out from that is negative, which reads as
/// a loop that runs no times rather than one that runs two hundred.
///
/// The step is not put through this, because a step is a difference rather than a value and its
/// signed reading is the one that says which way the counter goes.
fn as_unsigned(inv: Invariant, ty: Type) -> Option<Invariant> {
    match inv.as_number() {
        Some(number) if number >= 0 => Some(inv),
        Some(number) => {
            // Only an integer constant was read as signed in the first place. A pointer never
            // was, so a negative number sitting in one is an expression this cannot reinterpret.
            let bits = ty.is_int().then(|| ty.bits()).filter(|&bits| bits < 127)?;
            Some(Invariant::number(number & ((1i128 << bits) - 1)))
        }
        // A symbolic operand is whatever it is at run time, and the subtraction below cancels it
        // rather than reading it, so long as nothing signed has been folded in beside it.
        None => (inv.scale == 1 && inv.offset == 0).then_some(inv),
    }
}

/// The count for an exit tested with an ordering, where overshooting the limit still ends it.
fn ordered(
    distance: Invariant,
    step: u128,
    inclusive: bool,
    mut assumptions: Vec<Assumption>,
) -> Option<Bound> {
    match distance.as_number() {
        Some(exact) => {
            if exact < 0 {
                // The counter starts past the limit, so the test fails the first time it runs.
                // That is a count of zero and it rests on nothing at all, not even on the counter
                // behaving, because the counter never moves.
                return Some(Bound { count: Count::Exact(0), assumptions: Vec::new() });
            }
            // Rounding up, because a step that overshoots still took the iteration that overshot.
            let count = (exact.unsigned_abs() + u128::from(inclusive)).div_ceil(step);
            Some(Bound { count: Count::Exact(count), assumptions })
        }
        // Symbolic, and only for a step of one, because dividing an expression by anything else
        // needs a representation for a division and there is not one here.
        None if step == 1 => {
            assumptions.push(Assumption::Approaching);
            let count = distance.plus(Invariant::number(i128::from(inclusive)))?;
            Some(Bound { count: Count::Symbolic(count), assumptions })
        }
        None => None,
    }
}

/// The count for an exit tested with `!=`, where the counter has to land on the limit exactly.
///
/// This is a different problem from the one above and not a special case of it. An ordering test
/// ends the loop the moment the counter is past the limit, so a step that overshoots still stops.
/// `!=` only ends the loop on the one iteration where the counter is the limit, so a counter that
/// steps over the limit, or that starts on the far side of it, keeps going until it wraps. Both
/// of those are endless loops rather than short ones, and answering zero for either was the bug
/// this function exists to not have.
fn landing(distance: Invariant, step: u128, mut assumptions: Vec<Assumption>) -> Option<Bound> {
    match distance.as_number() {
        Some(exact) => {
            let travel = u128::try_from(exact).ok()?;
            // Checked outright rather than assumed, which is why nothing here needs an assumption
            // about the step dividing anything.
            (travel % step == 0).then(|| Bound { count: Count::Exact(travel / step), assumptions })
        }
        // A step of one lands on everything ahead of it, so the only thing left to establish is
        // that the limit is ahead. `while (p != end)` is this case, and a step of anything else
        // would need the division a symbolic distance has no room for.
        None if step == 1 => {
            assumptions.push(Assumption::Approaching);
            Some(Bound { count: Count::Symbolic(distance), assumptions })
        }
        None => None,
    }
}

/// The predicate that is true exactly when this one is not.
fn invert(pred: IntPred) -> IntPred {
    match pred {
        IntPred::Eq => IntPred::Ne,
        IntPred::Ne => IntPred::Eq,
        IntPred::Slt => IntPred::Sge,
        IntPred::Sle => IntPred::Sgt,
        IntPred::Sgt => IntPred::Sle,
        IntPred::Sge => IntPred::Slt,
        IntPred::Ult => IntPred::Uge,
        IntPred::Ule => IntPred::Ugt,
        IntPred::Ugt => IntPred::Ule,
        IntPred::Uge => IntPred::Ult,
    }
}

/// The predicate that says the same thing with the operands the other way round.
fn swap(pred: IntPred) -> IntPred {
    match pred {
        IntPred::Eq => IntPred::Eq,
        IntPred::Ne => IntPred::Ne,
        IntPred::Slt => IntPred::Sgt,
        IntPred::Sle => IntPred::Sge,
        IntPred::Sgt => IntPred::Slt,
        IntPred::Sge => IntPred::Sle,
        IntPred::Ult => IntPred::Ugt,
        IntPred::Ule => IntPred::Uge,
        IntPred::Ugt => IntPred::Ult,
        IntPred::Uge => IntPred::Ule,
    }
}

/// The constant a value is, if it is one.
fn constant(func: &Func, value: Value) -> Option<(Imm, Type)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::IConst {
        return None;
    }
    let Extra::Imm(at) = func[inst].extra else { return None };
    let ty = func[value].ty;
    ty.is_int().then(|| (func[at], ty))
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

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Flags, Func, IntPred, Opcode, Signature, Type, Value};

    use crate::cfg::Cfg;
    use crate::dom::Dominators;
    use crate::loops::{LoopId, Loops};
    use crate::scev::{Assumption, Bound, Count, Evolution, Invariant, Scev};

    /// A loop counting in `ty` from `from` by `step` while the counter is below `to`.
    ///
    /// ```text
    /// entry:  jump header(from)
    /// header(i): test = icmp pred i, to ; br_if test, body, exit
    /// body:   next = add i, step ; jump header(next)
    /// exit:   ret
    /// ```
    ///
    /// The counter is the header's only parameter, which is what the tests ask about.
    struct Counted {
        func: Func,
        counter: Value,
        next: Value,
    }

    fn counted(ty: Type, from: i128, to: i128, step: i128, pred: IntPred, flags: Flags) -> Counted {
        let (it, ()) = counted_with(ty, from, to, step, pred, flags, |_, _| ());
        it
    }

    /// The same loop, with `extra` run in the body on the counter before the counter steps.
    ///
    /// The builder appends, and the body's `jump` back to the header has to stay the last
    /// instruction in it or the block has no terminator and the loop stops being one. So anything
    /// a test wants derived from the counter goes in here rather than being tacked on afterwards.
    fn counted_with<T>(
        ty: Type,
        from: i128,
        to: i128,
        step: i128,
        pred: IntPred,
        flags: Flags,
        extra: impl FnOnce(&mut Builder<'_>, Value) -> T,
    ) -> (Counted, T) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();
        let counter = func.append_param(header, ty);

        let mut build = Builder::new(&mut func, entry);
        let start = build.iconst(ty, from);
        build.jump(header, &[start]);

        let mut build = Builder::new(&mut func, header);
        let limit = build.iconst(ty, to);
        let test = build.icmp(pred, counter, limit);
        build.br_if(test, body, &[], exit, &[]);

        let mut build = Builder::new(&mut func, body);
        let derived = extra(&mut build, counter);
        let by = build.iconst(ty, step);
        let next = build.binary(Opcode::Add, counter, by, flags);
        build.jump(header, &[next]);

        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);

        (Counted { func, counter, next }, derived)
    }

    /// The analysis over a function, along with the one loop it has.
    fn analyse(func: &Func) -> (Cfg, Loops) {
        let cfg = Cfg::new(func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);
        (cfg, loops)
    }

    /// The chrec of a value in the one loop of a function.
    fn evolution(func: &Func, value: Value) -> Evolution {
        let (cfg, loops) = analyse(func);
        let id = loops.roots()[0];
        Scev::new(func, &cfg, &loops).evolution(id, value)
    }

    /// The trip count of the one loop of a function.
    fn bound(func: &Func) -> Option<Bound> {
        let (cfg, loops) = analyse(func);
        let id: LoopId = loops.roots()[0];
        Scev::new(func, &cfg, &loops).bound(id)
    }

    #[test]
    fn a_counter_from_zero_by_one_is_the_chrec_everyone_expects() {
        let it = counted(Type::int(32), 0, 100, 1, IntPred::Slt, Flags::NSW);
        let chrec = evolution(&it.func, it.counter).chrec().expect("the counter evolves");
        assert_eq!(chrec.base, Invariant::number(0));
        assert_eq!(chrec.step, Invariant::number(1));
        assert_eq!(chrec.ty, Type::int(32));
        assert!(chrec.does_not_wrap(true));
    }

    #[test]
    fn the_value_fed_back_is_the_chrec_one_step_along() {
        let it = counted(Type::int(32), 5, 100, 3, IntPred::Slt, Flags::NSW);
        let chrec = evolution(&it.func, it.next).chrec().expect("the increment evolves");
        assert_eq!(chrec.base, Invariant::number(8));
        assert_eq!(chrec.step, Invariant::number(3));
    }

    #[test]
    fn a_multiple_of_the_counter_plus_a_number_is_a_chrec_of_its_own() {
        // `j = 2 * i + 3` where `i = {0, +, 1}`, which is the shape section 7.4 says pattern
        // matching runs out of road on and chains of recurrences do not.
        let (it, shifted) =
            counted_with(Type::int(32), 0, 100, 1, IntPred::Slt, Flags::NSW, |build, counter| {
                let two = build.iconst(Type::int(32), 2);
                let three = build.iconst(Type::int(32), 3);
                let doubled = build.binary(Opcode::Mul, counter, two, Flags::NSW);
                build.binary(Opcode::Add, doubled, three, Flags::NSW)
            });

        let chrec = evolution(&it.func, shifted).chrec().expect("it evolves");
        assert_eq!(chrec.base, Invariant::number(3));
        assert_eq!(chrec.step, Invariant::number(2));
    }

    #[test]
    fn a_shift_by_a_constant_scales_the_chrec_and_a_shift_past_the_width_does_not() {
        let (it, (scaled, poison)) =
            counted_with(Type::int(32), 1, 100, 1, IntPred::Slt, Flags::NSW, |build, counter| {
                let three = build.iconst(Type::int(32), 3);
                let wide = build.iconst(Type::int(32), 32);
                (
                    build.binary(Opcode::Shl, counter, three, Flags::NSW),
                    build.binary(Opcode::Shl, counter, wide, Flags::NSW),
                )
            });

        let chrec = evolution(&it.func, scaled).chrec().expect("it evolves");
        assert_eq!(chrec.base, Invariant::number(8));
        assert_eq!(chrec.step, Invariant::number(8));
        // A count at the width is poison rather than a shift to zero, so there is no sequence to
        // describe.
        assert_eq!(evolution(&it.func, poison), Evolution::Unknown);
    }

    #[test]
    fn a_pointer_walked_by_the_element_size_is_a_chrec_in_bytes() {
        // What `for (p = a; p != end; p++)` lowers to on an array of four byte elements. Section
        // 7.4 calls this the one deliberate extension past affine and the difference between
        // analysing half of real C loops and nearly all of them.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();
        let start = func.append_param(entry, Type::PTR);
        let cursor = func.append_param(header, Type::PTR);

        let mut build = Builder::new(&mut func, entry);
        build.jump(header, &[start]);
        let mut build = Builder::new(&mut func, header);
        let done = build.icmp(IntPred::Eq, cursor, start);
        build.br_if(done, exit, &[], body, &[]);
        let mut build = Builder::new(&mut func, body);
        let four = build.iconst(Type::int(64), 4);
        let next = build.binary(Opcode::PtrAdd, cursor, four, Flags::NONE);
        build.jump(header, &[next]);
        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);

        let chrec = evolution(&func, cursor).chrec().expect("the cursor evolves");
        assert_eq!(chrec.base, Invariant::of(start));
        assert_eq!(chrec.step, Invariant::number(4));
        assert_eq!(chrec.ty, Type::PTR);
    }

    #[test]
    fn a_counter_in_unsigned_char_wraps_and_does_not_widen_without_a_promise() {
        // Section 7.7's second way of being wrong. `{0, +, 1}` in `unsigned char` is not
        // `0, 1, 2, ...`, it is that modulo two hundred and fifty six, and widening it is only
        // the same sequence if it does not get that far.
        let (it, wide) =
            counted_with(Type::int(8), 0, 100, 1, IntPred::Ult, Flags::NONE, |build, counter| {
                build.unary(Opcode::ZExt, counter, Type::int(32))
            });
        let chrec = evolution(&it.func, it.counter).chrec().expect("the counter evolves");
        assert_eq!(chrec.ty, Type::int(8));
        assert!(!chrec.does_not_wrap(false));
        assert_eq!(evolution(&it.func, wide), Evolution::Unknown);
    }

    #[test]
    fn a_counter_in_short_widens_when_the_increment_promised_it_would_not_wrap() {
        let (it, (wide, zero_extended)) =
            counted_with(Type::int(16), 0, 100, 1, IntPred::Slt, Flags::NSW, |build, counter| {
                (
                    build.unary(Opcode::SExt, counter, Type::int(32)),
                    build.unary(Opcode::ZExt, counter, Type::int(32)),
                )
            });

        let chrec = evolution(&it.func, wide).chrec().expect("it widens");
        assert_eq!(chrec.ty, Type::int(32));
        assert_eq!(chrec.base, Invariant::number(0));
        assert_eq!(chrec.step, Invariant::number(1));
        // `nsw` is a promise about the signed reading and says nothing about the unsigned one.
        assert_eq!(evolution(&it.func, zero_extended), Evolution::Unknown);
    }

    #[test]
    fn a_step_of_zero_is_invariant_and_has_no_trip_count() {
        // Section 7.7's first way of being wrong. `i += k` with `k` of zero is a valid affine
        // chrec of a loop that never leaves through this exit, and code dividing the distance by
        // the step divides by zero.
        let it = counted(Type::int(32), 0, 100, 0, IntPred::Slt, Flags::NSW);
        assert!(matches!(evolution(&it.func, it.counter), Evolution::Invariant(_)));
        assert_eq!(bound(&it.func), None);
    }

    #[test]
    fn a_counted_loop_has_the_count_anyone_would_work_out_by_hand() {
        let it = counted(Type::int(32), 0, 100, 1, IntPred::Slt, Flags::NSW);
        let found = bound(&it.func).expect("it is counted");
        let (count, assumptions) = found.parts();
        assert_eq!(count, Count::Exact(100));
        // The distance is a number and it is not negative, so being entered is not in question.
        // Signed overflow being undefined still is, which is what `-fwrapv` would withdraw.
        assert_eq!(assumptions, [Assumption::StrictOverflow]);
        assert_eq!(found.proven(), None);
    }

    #[test]
    fn a_step_that_overshoots_still_takes_the_iteration_that_overshot() {
        // Zero, three, six, nine, and the test fails at twelve, so four iterations rather than
        // three and a third. Rounding the other way is an off by one in every unroller.
        let it = counted(Type::int(32), 0, 10, 3, IntPred::Slt, Flags::NSW);
        let (count, _) = bound(&it.func).expect("it is counted").parts();
        assert_eq!(count, Count::Exact(4));
    }

    #[test]
    fn an_inclusive_test_runs_one_more_time() {
        let it = counted(Type::int(32), 0, 10, 1, IntPred::Sle, Flags::NSW);
        let (count, _) = bound(&it.func).expect("it is counted").parts();
        assert_eq!(count, Count::Exact(11));
    }

    #[test]
    fn a_loop_whose_test_fails_first_time_runs_no_times_and_rests_on_nothing() {
        let it = counted(Type::int(32), 10, 0, 1, IntPred::Slt, Flags::NSW);
        let found = bound(&it.func).expect("it is counted");
        assert_eq!(found.proven(), Some(Count::Exact(0)));
        assert!(found.assumptions().is_empty());
    }

    #[test]
    fn counting_down_is_the_same_problem_with_the_ends_swapped() {
        let it = counted(Type::int(32), 10, 0, -1, IntPred::Sgt, Flags::NSW);
        let (count, _) = bound(&it.func).expect("it is counted").parts();
        assert_eq!(count, Count::Exact(10));
    }

    #[test]
    fn an_unsigned_test_does_not_drag_in_the_signed_overflow_assumption() {
        let it = counted(Type::int(32), 0, 100, 1, IntPred::Ult, Flags::NUW);
        let found = bound(&it.func).expect("it is counted");
        assert_eq!(found.proven(), Some(Count::Exact(100)));
    }

    #[test]
    fn a_test_against_something_the_loop_does_not_change_gives_a_symbolic_count() {
        // `for (i = 0; i < n; i++)`, where the answer is `n` and is only `n` if the loop is
        // entered, because `n` of minus one runs no times and the distance is minus one.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();
        let limit = func.append_param(entry, Type::int(32));
        let counter = func.append_param(header, Type::int(32));

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(header, &[zero]);
        let mut build = Builder::new(&mut func, header);
        let test = build.icmp(IntPred::Slt, counter, limit);
        build.br_if(test, body, &[], exit, &[]);
        let mut build = Builder::new(&mut func, body);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        build.jump(header, &[next]);
        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);

        let found = bound(&func).expect("it is counted");
        let (count, assumptions) = found.parts();
        assert_eq!(count, Count::Symbolic(Invariant::of(limit)));
        assert!(assumptions.contains(&Assumption::Approaching), "{assumptions:?}");
        assert!(assumptions.contains(&Assumption::StrictOverflow), "{assumptions:?}");
        assert_eq!(found.proven(), None);
    }

    #[test]
    fn a_counter_without_a_no_wrap_promise_carries_the_assumption_instead() {
        let it = counted(Type::int(32), 0, 100, 1, IntPred::Ult, Flags::NONE);
        let found = bound(&it.func).expect("it is counted");
        let (_, assumptions) = found.parts();
        assert!(assumptions.iter().any(|a| matches!(a, Assumption::NoWrap(_))), "{assumptions:?}");
    }

    #[test]
    fn a_test_that_ends_the_loop_when_it_succeeds_is_read_the_other_way_round() {
        // `for (i = 0; ; i++) if (i >= 100) break;`, which is the same loop with the arms of the
        // branch swapped. The test that keeps the loop going is the opposite of the one written.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();
        let counter = func.append_param(header, Type::int(32));

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(header, &[zero]);
        let mut build = Builder::new(&mut func, header);
        let limit = build.iconst(Type::int(32), 100);
        let done = build.icmp(IntPred::Sge, counter, limit);
        build.br_if(done, exit, &[], body, &[]);
        let mut build = Builder::new(&mut func, body);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        build.jump(header, &[next]);
        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);

        let (count, _) = bound(&func).expect("it is counted").parts();
        assert_eq!(count, Count::Exact(100));
    }

    #[test]
    fn an_unsigned_limit_past_the_middle_of_its_type_is_not_a_negative_one() {
        // `for (unsigned char i = 0; i < 200; i++)`. Two hundred does not fit in a signed byte
        // and the constant is held as minus fifty six, so a distance taken at face value is
        // negative and reads as a loop that runs no times.
        let it = counted(Type::int(8), 0, 200, 1, IntPred::Ult, Flags::NUW);
        let found = bound(&it.func).expect("it is counted");
        assert_eq!(found.proven(), Some(Count::Exact(200)));
    }

    #[test]
    fn a_walk_that_lands_on_a_not_equal_limit_exactly_is_counted() {
        // `while (i != 10)` counting by one, which is `while (p != end)` over an array once the
        // element size has been divided out. `!=` says nothing about how its operands are read,
        // so the promise it wants is the unsigned one and an `nsw` on its own is not enough.
        let it = counted(Type::int(32), 0, 10, 1, IntPred::Ne, Flags::NSW.union(Flags::NUW));
        let found = bound(&it.func).expect("it lands on its limit");
        // The step divides the distance and both are numbers, so it was checked rather than
        // assumed and there is nothing left over.
        assert_eq!(found.proven(), Some(Count::Exact(10)));
    }

    #[test]
    fn a_counter_stepping_away_from_a_not_equal_limit_is_not_a_loop_that_runs_no_times() {
        // The distance is negative and an ordering test would read that as the loop never being
        // entered. `!=` reads it as the counter never arriving, which is an endless loop, and
        // answering zero for it was a real bug that the property test in `tests/scev.rs` found.
        let it = counted(Type::int(32), 48, 15, 1, IntPred::Ne, Flags::NSW);
        assert_eq!(bound(&it.func), None);
    }

    #[test]
    fn a_counter_stepping_over_a_not_equal_limit_never_arrives_either() {
        // Zero, three, six, nine, twelve, and ten is never one of them. An ordering test would
        // have stopped at twelve.
        let it = counted(Type::int(32), 0, 10, 3, IntPred::Ne, Flags::NSW);
        assert_eq!(bound(&it.func), None);
    }

    #[test]
    fn an_estimate_is_the_count_when_there_is_one_and_a_guess_when_there_is_not() {
        let counted_loop = counted(Type::int(32), 0, 7, 1, IntPred::Slt, Flags::NSW);
        let (cfg, loops) = analyse(&counted_loop.func);
        let id = loops.roots()[0];
        let estimate = Scev::new(&counted_loop.func, &cfg, &loops).estimate(id);
        assert_eq!(estimate.iterations(), 7);
        assert!(!estimate.is_guess());

        // A loop this cannot count still has to answer, because the caller is deciding whether
        // something is worth doing rather than whether it is legal.
        let uncounted = counted(Type::int(32), 0, 100, 0, IntPred::Slt, Flags::NSW);
        let (cfg, loops) = analyse(&uncounted.func);
        let id = loops.roots()[0];
        let estimate = Scev::new(&uncounted.func, &cfg, &loops).estimate(id);
        assert!(estimate.is_guess());
        assert_eq!(estimate.iterations(), super::ASSUMED_ITERATIONS);
    }

    #[test]
    fn a_value_the_loop_does_not_touch_is_invariant_rather_than_unknown() {
        let it = counted(Type::int(32), 0, 100, 1, IntPred::Slt, Flags::NSW);
        let (cfg, loops) = analyse(&it.func);
        let id = loops.roots()[0];
        let mut scev = Scev::new(&it.func, &cfg, &loops);
        // The counter's start is an `iconst` in the entry block, which is both.
        assert_eq!(
            scev.evolution(id, it.counter).chrec().expect("it evolves").base,
            Invariant::number(0)
        );
    }

    #[test]
    fn every_assumption_says_what_it_is_in_a_line() {
        let it = counted(Type::int(8), 0, 100, 1, IntPred::Ult, Flags::NONE);
        let found = bound(&it.func).expect("it is counted");
        for assumption in found.assumptions() {
            let line = assumption.describe();
            assert!(!line.is_empty());
            assert!(!line.contains('\n'), "an assumption is one line: {line}");
        }
    }
}
