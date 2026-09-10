//! How many times a loop goes round, and how many bytes a walk inside it covers.
//!
//! Two passes want this and they want it for opposite reasons, which is why it is here rather than
//! in either of them.
//!
//! [`crate::hoist`] wants it as a fact. It puts one check in front of a loop covering every access
//! the loop makes, so a count that is too small is a check that covers less than the loop reads and
//! a bug that goes unreported. Everything the count rests on has to be discharged before that check
//! is written, which is what the assumptions below are about.
//!
//! [`crate::split`] wants it as an estimate. What it does with the number is ask the runtime how
//! many of those bytes really belong to the object, and it believes the answer rather than the
//! question. A count that is too small there costs iterations in the half of the loop that still
//! has its checks, and a count that is too large costs a slightly longer walk in the runtime. So
//! that pass reads a count from any exit it can get one from, and this file gives the same number
//! to both callers and lets each decide what it is worth.

use rucc_ir::{Builder, Def, Flags, Func, Inst, IntPred, Opcode, Type, Value};

use crate::loops::LoopId;
use crate::scev::{Assumption, Count, Invariant, Reading, Scev};

/// What is reported for a loop whose count is not settled.
pub(crate) const NOT_COUNTED: &str =
    "loop left alone, how many times it runs is not settled before it starts";

/// What is reported for a loop whose count is settled only if its counter does not wrap.
pub(crate) const RESTS_ON_NO_WRAP: &str =
    "loop left alone, how many times it runs is known only if its counter does not wrap";

/// How many times a loop goes round, which comes out either as a number or as an expression.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Around {
    /// Exactly this many.
    Number(i128),
    /// This many, worked out from something the loop does not change and read the way its test read
    /// it.
    Computed(Invariant, Reading),
}

/// How many times the loop goes round, when a caller may believe it.
///
/// Goes round, and not runs, and the difference is the whole of an off by one. What the analysis
/// answers is the iteration at which the exit test first fails, which is how many times the back
/// edge is taken. A block that runs before that test runs one more time than that, because it ran
/// on the way to the test that ended the loop as well as on the way to all the ones that did not.
/// Every check hoisting takes out is in such a block, which is what it reads this number with.
///
/// Not [`crate::scev::Bound::proven`], and the difference is one assumption, which is why a count
/// that is a number is read through [`crate::scev::Bound::under_undefined_overflow`] and the
/// reasoning behind that is written there.
///
/// A count that is an expression is read here instead, because it is allowed one assumption that
/// accessor refuses. [`Assumption::Approaching`] says the counter starts on the near side of its
/// limit, and [`covered`] discharges it rather than believing it, by clamping the count at zero. A
/// count that comes out negative is a loop whose test failed the first time it ran, which for a
/// bottom tested loop is a loop that went round no times, and zero is what that loop's extent is
/// worked out from.
///
/// What the reading is for is the widening. The count is built out of the limit operand of the exit
/// test, that operand is a value of the counter's own type, and which number it is depends on how
/// the test read it. A limit past the middle of a thirty two bit type is a large number to an
/// unsigned test and a negative one to a signed test, so an extent computed by sign extending what
/// an unsigned test compared would clamp to zero and leave a check covering one element in front of
/// a loop reading thousands. The reading is carried through to [`covered`], which spends it on a
/// sign extension or a zero extension, and to the caller, which is where anything about how large
/// the count can be belongs.
pub(crate) fn counted(scev: &mut Scev<'_>, id: LoopId) -> Result<Around, &'static str> {
    let bound = scev.bound(id).ok_or(NOT_COUNTED)?;
    if let Some(Count::Exact(exact)) = bound.under_undefined_overflow() {
        return i128::try_from(exact).map(Around::Number).map_err(|_| NOT_COUNTED);
    }
    let reading = bound.reading();
    let (Count::Symbolic(count), assumptions) = bound.parts() else {
        return Err(NOT_COUNTED);
    };
    // A count that rests on the counter not wrapping is reported as that rather than as a count
    // nobody worked out, because the two are different pieces of work. This one has an expression
    // for how many times the loop goes round and a condition attached to it, and what it needs is
    // either the condition discharged or a check written that stands in for it. See #782.
    for rests_on in assumptions {
        match rests_on {
            Assumption::StrictOverflow | Assumption::Approaching => {}
            Assumption::NoWrap(_) => return Err(RESTS_ON_NO_WRAP),
        }
    }
    Ok(Around::Computed(count, reading))
}

/// Builds how many bytes a walk covers, out of a count nobody has as a number.
///
/// `max(scale * value + offset, 0) * step + reach`, in the order it reads. The widening is the one
/// the exit test the count came from asks for where there is one to do, a sign extension for a
/// signed test and a zero extension for an unsigned one. The clamp is [`Assumption::Approaching`]
/// paid for rather than assumed, and it is a `select` rather than a branch because the whole of this
/// has to be straight line code in a preheader.
///
/// The `flags` are what the caller is willing to say about the arithmetic, and the two callers say
/// different things. Hoisting bounds the count against the width of its type before it asks for any
/// of this, so nothing here can leave sixty four bits and `nsw` is a fact it earned. Splitting does
/// no such bounding, because the number is a limit on how far the runtime looks and a limit that
/// wrapped is still answered with a true count of bytes, so it asks for none and takes what plain
/// wrapping arithmetic gives it.
///
/// The clamp stays on the unsigned side even though a zero extension is never negative, because what
/// can be negative is the count rather than the value it is built out of: `for (unsigned i = 5; i <
/// n; i++)` has an offset of minus five and an `n` of one is a loop that runs no times.
///
/// The trivial steps are left out where the numbers make them trivial. Nothing after this pass folds
/// a multiply by one, so a walk of single bytes would otherwise leave one in every preheader.
pub(crate) fn covered(
    build: &mut Builder<'_>,
    made: &mut Vec<Value>,
    count: Invariant,
    step: i128,
    reach: i128,
    reading: Reading,
    flags: Flags,
) -> Value {
    let word = Type::int(64);
    let value = count.value.expect("a count that is an expression is built on a value");
    // A count already as wide as the arithmetic is taken as it stands. The widening is what carries
    // the exit test's reading of a narrower count into sixty four bits, and there is nothing to
    // carry when the count is sixty four bits to begin with. A count wider than that is one the
    // caller was supposed to have refused.
    let mut wide = value;
    if build.func()[value].ty.bits() < 64 {
        let widen = match reading {
            Reading::Signed => Opcode::SExt,
            Reading::Unsigned => Opcode::ZExt,
        };
        wide = build.unary(widen, value, word);
        made.push(wide);
    }
    if count.scale != 1 {
        let scale = build.iconst(word, count.scale);
        made.push(scale);
        wide = build.binary(Opcode::Mul, wide, scale, flags);
        made.push(wide);
    }
    if count.offset != 0 {
        let offset = build.iconst(word, count.offset);
        made.push(offset);
        wide = build.binary(Opcode::Add, wide, offset, flags);
        made.push(wide);
    }

    let zero = build.iconst(word, 0);
    made.push(zero);
    let entered = build.icmp(IntPred::Sgt, wide, zero);
    made.push(entered);
    let mut span = build.select(entered, wide, zero);
    made.push(span);

    if step != 1 {
        let by = build.iconst(word, step);
        made.push(by);
        span = build.binary(Opcode::Mul, span, by, flags);
        made.push(span);
    }
    if reach != 0 {
        let last = build.iconst(word, reach);
        made.push(last);
        span = build.binary(Opcode::Add, span, last, flags);
        made.push(span);
    }
    span
}

/// The instruction that produced a value the builder just made.
pub(crate) fn inst_of(func: &Func, value: Value) -> Inst {
    let Def::Result { inst, .. } = func[value].def else {
        unreachable!("the builder was just asked for an instruction that produces this")
    };
    inst
}
