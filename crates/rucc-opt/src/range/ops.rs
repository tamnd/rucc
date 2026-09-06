//! What an operation does to a range, forwards and backwards.
//!
//! Design: `spec/optimizer/10-value-ranges.md`, sections 10.4 and 10.7. Section 10.4 calls this a
//! table with an entry per opcode, and says the M4 subset is addition, subtraction,
//! multiplication, the bitwise operations, the shifts, the comparisons, truncation, sign and zero
//! extension, and negation. Not division, not remainder, not the overflow builtins, not the
//! intrinsics: those are cheap to add later against the same tests and expensive to get subtly
//! wrong now.
//!
//! # Forwards and backwards
//!
//! Forwards is the easy direction and the one everything else is built on: given what the
//! operands can be, what can the result be. [`add`] and the rest of the free functions here are
//! that.
//!
//! Backwards is the direction the on-demand query needs, and it is the whole reason a branch
//! teaches the analysis anything. On the true edge of `if (x < 10)`, the fact is not about the
//! comparison's result, it is about `x`, and getting there means running the comparison inverse:
//! given that `x < y` holds and given what `y` can be, what can `x` be. [`narrow_for`] is that,
//! and it is the one inverse that pays for itself on nearly every branch in nearly every
//! function. [`backward`] is the rest, for the operations whose inverse is exact and cheap, and
//! it says so when there is no inverse worth having rather than pretending.
//!
//! # Wrapping is an argument and not a check somewhere else
//!
//! Section 10.7 names this as the way a range implementation gets a program wrong. `[100, 200] +
//! [100, 200]` in eight bits is not `[200, 400]`, and in a signed type without `-fwrapv` the
//! optimizer may assume the overflow did not happen, which is a stronger fact and a different
//! answer. So every operation that can overflow takes [`Flags`], the same `NSW` and `NUW` the
//! instruction carries, and there is no way to call one of these and forget. A range computed
//! under one assumption and used under the other is a miscompilation, and the only defence
//! against that is not having a version of the function that does not ask.
//!
//! What the flags buy is the clamp. Without them the answer is the wrapping one, exact modulo
//! `2^width`. With `NSW` the sums that do not fit cannot have happened, so the answer is
//! intersected with the ones that do, and if none of them fit the range is empty, which is the
//! analysis proving the code is unreachable.
//!
//! # Sound, and then as sharp as there is room for
//!
//! Every function here returns a range that holds every value the operation can actually produce.
//! That is the property the tests check exhaustively at width three and four, and it is the one
//! whose failure is a wrong program. Holding more than that is precision loss, which costs speed
//! and not correctness, and it happens for two reasons: an answer that needs more than
//! [`super::PAIRS`] intervals, and an operation whose exact answer is not worth computing. Both
//! are marked where they happen.

use rucc_ir::{Flags, IntPred};

use super::{Bits, PAIRS, Range, clamp, mask, sign_bit, signed_limits};

/// How many values of a shift count are worth walking one at a time.
///
/// A constant shift is one value and most of the rest are a handful, and walking them gives the
/// exact answer where reasoning about the bounds would round the whole thing off. Past this the
/// coarse answer is used, which is the bits a shift by the smallest count is bound to produce.
const COUNTS: usize = 16;

/// Whether a comparison is settled, and which way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Truth {
    /// It holds however the operands come out, so the comparison folds to one.
    Always,
    /// It never holds, so it folds to zero.
    Never,
    /// The ranges do not settle it.
    Either,
}

/// The sum, modulo the width, and narrowed by whatever the flags promise.
///
/// # Panics
///
/// Panics if the operands are of different widths.
#[must_use]
pub fn add(a: Range, b: Range, flags: Flags) -> Range {
    assert_eq!(a.width(), b.width(), "these are ranges of different widths");
    let width = a.width();
    per_pair(a, b, |(al, ah), (bl, bh)| {
        let wrapped = wrapping(al.wrapping_add(bl), span(ah - al, bh - bl, width), width);
        clamped(
            wrapped,
            (al, ah),
            (bl, bh),
            flags,
            width,
            |(al, ah), (bl, bh)| (al.saturating_add(bl), ah.saturating_add(bh)),
            |(al, ah), (bl, bh)| {
                // The smallest sum already leaving the type means every sum does.
                let lo = al.checked_add(bl).filter(|&lo| lo <= mask(width))?;
                Some((lo, ah.saturating_add(bh)))
            },
        )
    })
}

/// The difference, modulo the width, and narrowed by whatever the flags promise.
///
/// # Panics
///
/// Panics if the operands are of different widths.
#[must_use]
pub fn sub(a: Range, b: Range, flags: Flags) -> Range {
    assert_eq!(a.width(), b.width(), "these are ranges of different widths");
    let width = a.width();
    per_pair(a, b, |(al, ah), (bl, bh)| {
        let wrapped = wrapping(al.wrapping_sub(bh), span(ah - al, bh - bl, width), width);
        // The window of a difference runs from the smallest minus the largest to the largest
        // minus the smallest, which is why on the unsigned side it is the low bound that can
        // prove the whole pairing impossible.
        clamped(
            wrapped,
            (al, ah),
            (bl, bh),
            flags,
            width,
            |(al, ah), (bl, bh)| (al.saturating_sub(bh), ah.saturating_sub(bl)),
            |(al, ah), (bl, bh)| {
                // Only the largest difference going below zero rules the pairing out. The
                // smallest going below zero rules out those particular values and leaves the
                // rest, which is what the clamp to zero says.
                let hi = ah.checked_sub(bl)?;
                Some((al.saturating_sub(bh), hi))
            },
        )
    })
}

/// Zero minus it, modulo the width, and narrowed by whatever the flags promise.
#[must_use]
pub fn neg(a: Range, flags: Flags) -> Range {
    sub(Range::exactly(0, a.width()), a, flags)
}

/// The product, modulo the width, and narrowed by whatever the flags promise.
///
/// Exact when the largest product fits without wrapping, which is the case that matters, since a
/// multiply whose operands are known small is the one an index calculation produces. When it does
/// wrap the answer is everything, refined by the low zero bits, because the low bits of a product
/// are the low bits of the product whatever the top of it did.
///
/// # Panics
///
/// Panics if the operands are of different widths.
#[must_use]
pub fn mul(a: Range, b: Range, flags: Flags) -> Range {
    assert_eq!(a.width(), b.width(), "these are ranges of different widths");
    let width = a.width();
    if a.is_empty() || b.is_empty() {
        return Range::empty(width);
    }

    // The low zero bits of a product are the low zero bits of the two added, and that holds
    // whether or not the top wrapped, so it is the one thing worth knowing in every case.
    let zeros = a.bits().low_zeros().saturating_add(b.bits().low_zeros()).min(width);
    let low = if zeros >= width {
        Bits::exactly(0, width)
    } else {
        Bits::from_parts(0, mask(width) << zeros, width)
    };

    per_pair(a, b, |(al, ah), (bl, bh)| {
        let wrapped = if al == ah && bl == bh {
            // One value times one value is one value, whatever the top of it did. This is worth a
            // case of its own because a shift by a constant comes through here.
            Range::exactly(al.wrapping_mul(bl), width)
        } else {
            match (al.checked_mul(bl), ah.checked_mul(bh)) {
                (Some(low), Some(high)) if high <= mask(width) => Range::between(low, high, width),
                _ => Range::full(width),
            }
        };
        clamped(
            wrapped,
            (al, ah),
            (bl, bh),
            flags,
            width,
            |(al, ah), (bl, bh)| {
                let corners = [
                    al.saturating_mul(bl),
                    al.saturating_mul(bh),
                    ah.saturating_mul(bl),
                    ah.saturating_mul(bh),
                ];
                let least = corners.into_iter().min().expect("four corners");
                (least, corners.into_iter().max().expect("four corners"))
            },
            |(al, ah), (bl, bh)| {
                let lo = al.checked_mul(bl).filter(|&lo| lo <= mask(width))?;
                Some((lo, ah.saturating_mul(bh)))
            },
        )
    })
    .narrow(low)
}

/// The bitwise and.
///
/// Worked out a bit at a time, which is exact whenever the operands' bits are known, plus the one
/// interval fact that holds for every pair: an and is no larger than either of them.
///
/// # Panics
///
/// Panics if the operands are of different widths.
#[must_use]
pub fn and(a: Range, b: Range) -> Range {
    assert_eq!(a.width(), b.width(), "these are ranges of different widths");
    let width = a.width();
    let (Some((_, ah)), Some((_, bh))) = (a.unsigned_bounds(), b.unsigned_bounds()) else {
        return Range::empty(width);
    };
    let ones = ones(a) & ones(b);
    let zeros = zeros(a, width) | zeros(b, width);
    let bits = Bits::from_parts(ones, mask(width) & !ones & !zeros, width);
    Range::between(0, ah.min(bh), width).narrow(bits)
}

/// The bitwise or.
///
/// # Panics
///
/// Panics if the operands are of different widths.
#[must_use]
pub fn or(a: Range, b: Range) -> Range {
    assert_eq!(a.width(), b.width(), "these are ranges of different widths");
    let width = a.width();
    let (Some((al, _)), Some((bl, _))) = (a.unsigned_bounds(), b.unsigned_bounds()) else {
        return Range::empty(width);
    };
    let ones = ones(a) | ones(b);
    let zeros = zeros(a, width) & zeros(b, width);
    let bits = Bits::from_parts(ones, mask(width) & !ones & !zeros, width);
    // An or is no smaller than either of them, which is the mirror of the bound on an and.
    Range::between(al.max(bl), mask(width), width).narrow(bits)
}

/// The bitwise exclusive or.
///
/// A bit of the result is known only where both operands know theirs, so there is nothing here
/// but the bits. There is no interval bound on an exclusive or that is worth the line.
///
/// # Panics
///
/// Panics if the operands are of different widths.
#[must_use]
pub fn xor(a: Range, b: Range) -> Range {
    assert_eq!(a.width(), b.width(), "these are ranges of different widths");
    let width = a.width();
    if a.is_empty() || b.is_empty() {
        return Range::empty(width);
    }
    let known = !a.bits().unknown_bits() & !b.bits().unknown_bits();
    let value = (a.bits().value() ^ b.bits().value()) & known;
    Range::full(width).narrow(Bits::from_parts(value, mask(width) & !known, width))
}

/// The bitwise complement, which is exact: it reverses each interval and nothing else.
#[must_use]
pub fn not(a: Range) -> Range {
    let width = a.width();
    let pairs: Vec<(u128, u128)> =
        a.pairs().iter().map(|&(lo, hi)| (mask(width) - hi, mask(width) - lo)).collect();
    Range::from_pairs(&pairs, width)
}

/// The value shifted left by the count, modulo the width.
///
/// A count at or above the width has no defined answer, so this says everything rather than
/// picking one. The count range may be of a different width from the value, since the IR does not
/// require them to match.
#[must_use]
pub fn shl(a: Range, count: Range, flags: Flags) -> Range {
    shift(a, count, flags, Kind::Left)
}

/// The value shifted right by the count with zeroes coming in.
#[must_use]
pub fn lshr(a: Range, count: Range, flags: Flags) -> Range {
    shift(a, count, flags, Kind::Logical)
}

/// The value shifted right by the count with the sign bit coming in.
#[must_use]
pub fn ashr(a: Range, count: Range, flags: Flags) -> Range {
    shift(a, count, flags, Kind::Arithmetic)
}

/// The low bits of it, at the narrower width.
///
/// Exact, including the case nobody expects: a run of consecutive values whose low bits wrap
/// round is still one wrapping interval at the narrower width, so truncating `[0xfe, 0x101]` to
/// eight bits gives `[0xfe, 0x01]` and not everything.
#[must_use]
pub fn trunc(a: Range, to: u32) -> Range {
    let to = clamp(to);
    let mut pairs: Vec<(u128, u128)> = Vec::with_capacity(PAIRS * 2);
    for &(lo, hi) in a.pairs() {
        if hi - lo >= mask(to) {
            return Range::full(to);
        }
        let (lo, hi) = (lo & mask(to), hi & mask(to));
        if lo <= hi {
            pairs.push((lo, hi));
        } else {
            pairs.push((0, hi));
            pairs.push((lo, mask(to)));
        }
    }
    Range::from_pairs(&pairs, to)
}

/// It at the wider width with zeroes on top, which keeps every interval as it was.
#[must_use]
pub fn zext(a: Range, to: u32) -> Range {
    let to = clamp(to);
    if to <= a.width() {
        return trunc(a, to);
    }
    Range::from_pairs(a.pairs(), to).narrow(Bits::from_parts(0, mask(a.width()), to))
}

/// It at the wider width with the sign bit on top.
///
/// An interval that straddles the sign boundary is two intervals afterwards, since the values
/// just below the boundary stay where they are and the ones at and above it move to the top of
/// the wider type. Splitting at the boundary first is what makes the rest of it arithmetic.
#[must_use]
pub fn sext(a: Range, to: u32) -> Range {
    let to = clamp(to);
    let from = a.width();
    if to <= from {
        return trunc(a, to);
    }
    let boundary = sign_bit(from);
    let lift = mask(to) - mask(from);
    let mut pairs: Vec<(u128, u128)> = Vec::with_capacity(PAIRS * 2);
    for &(lo, hi) in a.pairs() {
        if lo < boundary {
            pairs.push((lo, hi.min(boundary - 1)));
        }
        if hi >= boundary {
            pairs.push((lo.max(boundary) + lift, hi + lift));
        }
    }
    Range::from_pairs(&pairs, to)
}

/// Whether the ranges settle the comparison.
///
/// An empty operand means the comparison is on a path that is never taken, and this answers
/// [`Truth::Either`] for it rather than picking a side, because folding an unreachable comparison
/// to a constant is work spent on code that is about to be deleted anyway.
#[must_use]
pub fn compare(pred: IntPred, a: Range, b: Range) -> Truth {
    if a.is_empty() || b.is_empty() {
        return Truth::Either;
    }
    match (possible(pred, a, b), possible(pred.inverse(), a, b)) {
        (true, false) => Truth::Always,
        (false, true) => Truth::Never,
        _ => Truth::Either,
    }
}

/// What the left operand can be given that the comparison holds.
///
/// This is the inverse the on-demand query runs at every branch, and it is where a range comes
/// from in the first place: on the true edge of `if (x < 10)` this turns what was known about `x`
/// into what is known about `x` there. For the other operand, call it with the predicate
/// [`IntPred::swapped`] and the ranges the other way round.
///
/// # Panics
///
/// Panics if the operands are of different widths.
#[must_use]
pub fn narrow_for(pred: IntPred, a: Range, b: Range) -> Range {
    assert_eq!(a.width(), b.width(), "these are ranges of different widths");
    let width = a.width();
    if a.is_empty() || b.is_empty() {
        return Range::empty(width);
    }
    let (ul, uh) = b.unsigned_bounds().expect("not empty");
    let (sl, sh) = b.signed_bounds().expect("not empty");
    let (low, high) = signed_limits(width);
    let allowed = match pred {
        IntPred::Eq => b,
        // Only a value known exactly rules anything out, since `x != y` with `y` in `[1, 2]`
        // leaves every `x` possible: whichever one it is, the other value of `y` is still there.
        IntPred::Ne => match b.singleton() {
            Some(value) => Range::other_than(value, width),
            None => return a,
        },
        IntPred::Ult if uh == 0 => Range::empty(width),
        IntPred::Ult => Range::between(0, uh - 1, width),
        IntPred::Ule => Range::between(0, uh, width),
        IntPred::Ugt if ul == mask(width) => Range::empty(width),
        IntPred::Ugt => Range::between(ul + 1, mask(width), width),
        IntPred::Uge => Range::between(ul, mask(width), width),
        IntPred::Slt => Range::signed_between(low, sh.saturating_sub(1), width),
        IntPred::Sle => Range::signed_between(low, sh, width),
        IntPred::Sgt => Range::signed_between(sl.saturating_add(1), high, width),
        IntPred::Sge => Range::signed_between(sl, high, width),
    };
    a.intersect(allowed)
}

/// Which operation an inverse is being asked for.
///
/// Only the ones whose inverse is exact and cheap are here. An and, an or and a multiply have
/// inverses that are either everything or an expensive approximation of everything, and section
/// 10.4's advice about the ones not worth having applies to them: a wrong answer is a
/// miscompilation and a vague answer is a slow program, so the vague one is what this gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Undo {
    /// The left operand of an addition, given the other one.
    AddLeft,
    /// The right operand of a subtraction, given the left one.
    SubRight,
    /// The left operand of a subtraction, given the right one.
    SubLeft,
    /// The operand of a negation.
    Neg,
    /// The operand of a complement.
    Not,
    /// The operand of an exclusive or, given the other one.
    Xor,
    /// The operand of a zero extension, at the narrower width.
    Zext(u32),
    /// The operand of a sign extension, at the narrower width.
    Sext(u32),
}

/// What the operand must have been for the operation to have produced this.
///
/// The `other` range is the operation's second operand where it has one, at the result's width,
/// and is ignored where it does not. The answer is at the operand's width, which is the result's
/// width except for the two extensions.
#[must_use]
pub fn backward(undo: Undo, result: Range, other: Range) -> Range {
    let width = result.width();
    match undo {
        // Wrapping addition and subtraction are exactly reversible, and that stays true whatever
        // the original carried, so the flags do not come into it: the inverse takes the result
        // back to an operand that could have produced it, and any narrowing the flags allow was
        // already done on the way forward.
        Undo::AddLeft => sub(result, other, Flags::NONE),
        // The right operand of a subtraction is the left one less the result, which is the other
        // way round from the left operand of an addition however alike the two look.
        Undo::SubRight => sub(other, result, Flags::NONE),
        Undo::SubLeft => add(result, other, Flags::NONE),
        Undo::Neg => neg(result, Flags::NONE),
        Undo::Not => not(result),
        Undo::Xor => xor(result, other),
        // A result outside what the extension can produce means the operand cannot exist, which
        // is the intersection coming back empty, and that is the analysis proving a path dead.
        Undo::Zext(from) => trunc(result.intersect(zext(Range::full(from), width)), from),
        Undo::Sext(from) => trunc(result.intersect(sext(Range::full(from), width)), from),
    }
}

/// Whether some pair of values, one from each, satisfies the comparison.
fn possible(pred: IntPred, a: Range, b: Range) -> bool {
    let (Some((ul, uh)), Some((vl, vh))) = (a.unsigned_bounds(), b.unsigned_bounds()) else {
        return false;
    };
    let (Some((sl, sh)), Some((tl, th))) = (a.signed_bounds(), b.signed_bounds()) else {
        return false;
    };
    match pred {
        IntPred::Eq => !a.intersect(b).is_empty(),
        // Two ranges have a differing pair unless both are the same one value.
        IntPred::Ne => !matches!((a.singleton(), b.singleton()), (Some(x), Some(y)) if x == y),
        IntPred::Ult => ul < vh,
        IntPred::Ule => ul <= vh,
        IntPred::Ugt => uh > vl,
        IntPred::Uge => uh >= vl,
        IntPred::Slt => sl < th,
        IntPred::Sle => sl <= th,
        IntPred::Sgt => sh > tl,
        IntPred::Sge => sh >= tl,
    }
}

/// The bits known to be one.
fn ones(a: Range) -> u128 {
    a.bits().value()
}

/// The bits known to be zero.
fn zeros(a: Range, width: u32) -> u128 {
    !a.bits().value() & !a.bits().unknown_bits() & mask(width)
}

/// How many values wide a result is, or `None` when that is all of them.
///
/// The number of values a wrapping interval covers is one more than this, so a span equal to the
/// largest value already covers everything and there is no interval to write down.
fn span(a: u128, b: u128, width: u32) -> Option<u128> {
    match a.checked_add(b) {
        Some(span) if span < mask(width) => Some(span),
        _ => None,
    }
}

/// Every interval of one against every interval of the other, unioned.
///
/// Doing it per pairing rather than once on the two hulls is what keeps the overflow promises
/// sharp. `[1, 1] u [4, 4]` plus `[3, 3] u [6, 6]` in three signed bits has exactly one pairing
/// whose sum fits, and a version of this that worked on the hulls would look at `[-4, 1]` and
/// `[-2, 3]`, see a window that fits, and learn nothing.
fn per_pair(a: Range, b: Range, each: impl Fn((u128, u128), (u128, u128)) -> Range) -> Range {
    let width = a.width();
    if a.is_empty() || b.is_empty() {
        return Range::empty(width);
    }
    let mut out = Range::empty(width);
    for &left in a.pairs() {
        for &right in b.pairs() {
            out = out.union(each(left, right));
        }
    }
    out
}

/// The interval that starts here and runs that far, wrapping round if it has to.
fn wrapping(lo: u128, span: Option<u128>, width: u32) -> Range {
    let Some(span) = span else {
        return Range::full(width);
    };
    let lo = lo & mask(width);
    Range::between(lo, lo.wrapping_add(span) & mask(width), width)
}

/// The wrapping answer for one pairing, narrowed by whichever overflow promises are made.
///
/// A window is the operation done in the integers rather than in the type. `NSW` says the signed
/// one did not leave the type, so anything outside it did not happen, and if none of it fits the
/// pairing contributes nothing at all, which is the analysis proving that pairing impossible.
/// `NUW` says the same of the unsigned one.
fn clamped(
    wrapped: Range,
    a: (u128, u128),
    b: (u128, u128),
    flags: Flags,
    width: u32,
    signed_window: impl Fn((i128, i128), (i128, i128)) -> (i128, i128),
    unsigned_window: impl Fn((u128, u128), (u128, u128)) -> Option<(u128, u128)>,
) -> Range {
    let mut range = wrapped;
    if flags.contains(Flags::NSW) {
        let (lo, hi) = signed_window(as_signed(a, width), as_signed(b, width));
        range = range.intersect(Range::signed_between(lo, hi, width));
    }
    if flags.contains(Flags::NUW) {
        range = match unsigned_window(a, b) {
            // `None` from the window means the pairing overflowed however it came out, and the
            // promise says it did not, so there is nothing left of it.
            Some((lo, hi)) if lo <= mask(width) => {
                range.intersect(Range::between(lo, hi.min(mask(width)), width))
            }
            _ => Range::empty(width),
        };
    }
    range
}

/// The least and greatest signed values in that interval of bit patterns.
fn as_signed(interval: (u128, u128), width: u32) -> (i128, i128) {
    Range::between(interval.0, interval.1, width).signed_bounds().expect("not empty")
}

/// Which way a shift goes and what comes in behind it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Left,
    Logical,
    Arithmetic,
}

/// The shift, walking the counts one at a time where there are few enough of them.
fn shift(a: Range, count: Range, flags: Flags, kind: Kind) -> Range {
    let width = a.width();
    if a.is_empty() || count.is_empty() {
        return Range::empty(width);
    }
    let Some((low, _)) = count.unsigned_bounds() else {
        return Range::empty(width);
    };
    // A count at or above the width is undefined, and an analysis that answered anything but
    // everything here would be exploiting the undefinedness, which is a decision for the pass
    // that wants it and not for the table.
    if low >= u128::from(width) {
        return Range::full(width);
    }

    match count.list(COUNTS) {
        Some(counts) => {
            let mut range = Range::empty(width);
            for at in counts {
                if at >= u128::from(width) {
                    return Range::full(width);
                }
                range = range.union(one_shift(a, at as u32, flags, kind, width));
            }
            range
        }
        // Too many counts to walk, so what is left is the part of the answer that holds for every
        // count in the range at once.
        None => coarse(a, low as u32, width, kind),
    }
}

/// The shift by one count.
fn one_shift(a: Range, at: u32, flags: Flags, kind: Kind, width: u32) -> Range {
    match kind {
        // A shift left is a multiply by a power of two and gets that entry's exactness for free,
        // including what the flags say about it.
        Kind::Left => mul(a, Range::exactly(1u128 << at, width), flags),
        Kind::Logical => {
            let pairs: Vec<(u128, u128)> =
                a.pairs().iter().map(|&(lo, hi)| (lo >> at, hi >> at)).collect();
            Range::from_pairs(&pairs, width)
        }
        Kind::Arithmetic => {
            // An arithmetic shift is monotone on the signed reading, so the ends stay the ends.
            let Some((lo, hi)) = a.signed_bounds() else {
                return Range::empty(width);
            };
            Range::signed_between(lo >> at, hi >> at, width)
        }
    }
}

/// What holds for every count from this one up.
fn coarse(a: Range, low: u32, width: u32, kind: Kind) -> Range {
    match kind {
        // Shifting left by at least this much leaves that many low zeroes whatever the count was.
        Kind::Left => Range::full(width).narrow(Bits::from_parts(0, mask(width) << low, width)),
        // Shifting right by at least this much leaves the top clear.
        Kind::Logical => Range::between(0, mask(width) >> low, width),
        Kind::Arithmetic => {
            let Some((lo, hi)) = a.signed_bounds() else {
                return Range::empty(width);
            };
            // Shifting right moves a value towards zero and stops at zero for a positive one and
            // at minus one for a negative one, so neither end can pass where it started.
            Range::signed_between(lo.min(0), hi.max(-1), width)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::signed;

    /// An operation run the way round the inverse claims to undo.
    type Forwards = Box<dyn Fn(u128, u128) -> u128>;

    /// The width the exhaustive checks run at.
    ///
    /// Three, because these walk every range against every range and every value against every
    /// value, and section 10.4 says a claim about ranges at a small width is either exhaustively
    /// checkable or not worth stating. Eight values is one hundred and twenty seven ranges, and
    /// the whole table goes past in a second.
    const W: u32 = 3;

    /// Every range this width describes exactly.
    fn all() -> Vec<Range> {
        let mut ranges = Vec::new();
        for subset in 0u32..1 << (1u32 << W) {
            let values: Vec<u128> =
                (0..=mask(W)).filter(|&value| subset & (1 << value) != 0).collect();
            let mut pairs: Vec<(u128, u128)> = Vec::new();
            for &value in &values {
                match pairs.last_mut() {
                    Some(last) if last.1 + 1 == value => last.1 = value,
                    _ => pairs.push((value, value)),
                }
            }
            if pairs.len() > PAIRS {
                continue;
            }
            let range = Range::from_pairs(&pairs, W);
            if held(range) == values {
                ranges.push(range);
            }
        }
        ranges
    }

    /// The values a range says it holds.
    fn held(range: Range) -> Vec<u128> {
        (0..=mask(range.width())).filter(|&value| range.contains(value)).collect()
    }

    /// The result holds every value the operation can produce, and where the operands were one
    /// value each it holds nothing else.
    ///
    /// Soundness is checked always, since losing a value is a wrong program. Sharpness is checked
    /// on operands that are single values, because an implementation that answered everything
    /// from every entry would pass a soundness check on its own and be worth nothing, and because
    /// that is the one case where every entry in the table can be exact. Wider operands are
    /// allowed to be vague: an exact wrapping product of two intervals is not an interval, and
    /// section 10.4 is clear that a vague answer is a slow program while a wrong one is a wrong
    /// program.
    fn check(got: Range, want: &[u128], what: &str, sharp: bool) {
        for value in want {
            assert!(got.contains(*value), "{what} lost {value:#x}, got {got:?}");
        }
        if !sharp {
            return;
        }
        let listed: Vec<u128> = held(got);
        assert_eq!(listed, want, "{what} is vaguer than it has any excuse to be");
    }

    /// Check a binary operation against every value in every pair of ranges.
    ///
    /// `truth` gives the value the operation produces, or `None` when the flags say that pairing
    /// cannot have happened, which is how an overflow promise is expressed as ground truth.
    fn binary(
        name: &str,
        op: impl Fn(Range, Range) -> Range,
        truth: impl Fn(u128, u128) -> Option<u128>,
    ) {
        let ranges = all();
        for &a in &ranges {
            for &b in &ranges {
                let mut want: Vec<u128> = Vec::new();
                for x in held(a) {
                    for y in held(b) {
                        if let Some(value) = truth(x, y) {
                            if !want.contains(&value) {
                                want.push(value);
                            }
                        }
                    }
                }
                want.sort_unstable();
                let sharp = a.singleton().is_some() && b.singleton().is_some();
                check(op(a, b), &want, &format!("{name}({a:?}, {b:?})"), sharp);
            }
        }
    }

    /// Check a unary operation against every value in every range.
    fn unary(name: &str, op: impl Fn(Range) -> Range, truth: impl Fn(u128) -> u128) {
        for a in all() {
            let mut want: Vec<u128> = held(a).into_iter().map(&truth).collect();
            want.sort_unstable();
            want.dedup();
            check(op(a), &want, &format!("{name}({a:?})"), a.singleton().is_some());
        }
    }

    /// How many runs of consecutive values these are, which is how many intervals it takes to say
    /// them exactly.
    fn runs(values: &[u128]) -> usize {
        let mut count = 0;
        let mut previous: Option<u128> = None;
        for &value in values {
            match previous {
                Some(last) if last + 1 == value => {}
                _ => count += 1,
            }
            previous = Some(value);
        }
        count
    }

    /// That bit pattern read as a signed number at the test width.
    fn as_signed(value: u128) -> i128 {
        signed(value, W)
    }

    /// Whether that mathematical value fits in a signed number at the test width.
    fn fits_signed(value: i128) -> bool {
        let (low, high) = signed_limits(W);
        (low..=high).contains(&value)
    }

    #[test]
    fn addition_wraps_and_says_so() {
        binary("add", |a, b| add(a, b, Flags::NONE), |x, y| Some(x.wrapping_add(y) & mask(W)));
    }

    #[test]
    fn addition_that_promised_not_to_overflow_leaves_out_the_pairs_that_would_have() {
        binary(
            "add nsw",
            |a, b| add(a, b, Flags::NSW),
            |x, y| {
                let sum = as_signed(x) + as_signed(y);
                fits_signed(sum).then(|| x.wrapping_add(y) & mask(W))
            },
        );
        binary("add nuw", |a, b| add(a, b, Flags::NUW), |x, y| (x + y <= mask(W)).then_some(x + y));
    }

    #[test]
    fn subtraction_wraps_and_says_so() {
        binary("sub", |a, b| sub(a, b, Flags::NONE), |x, y| Some(x.wrapping_sub(y) & mask(W)));
        binary(
            "sub nsw",
            |a, b| sub(a, b, Flags::NSW),
            |x, y| {
                let difference = as_signed(x) - as_signed(y);
                fits_signed(difference).then(|| x.wrapping_sub(y) & mask(W))
            },
        );
        binary("sub nuw", |a, b| sub(a, b, Flags::NUW), |x, y| (x >= y).then(|| x - y));
    }

    #[test]
    fn the_overflow_promise_is_checked_against_each_pairing_and_not_the_whole_range() {
        // In three signed bits, one and minus four against three and minus two. Only two of the
        // four pairings have a sum that fits, and both come to minus one. Looking at the two
        // ranges as `[-4, 1]` and `[-2, 3]` would give a window of `[-6, 4]`, which overlaps what
        // fits, and the answer would have been the three values the wrapping sum allows.
        let a = Range::from_pairs(&[(1, 1), (4, 4)], 3);
        let b = Range::from_pairs(&[(3, 3), (6, 6)], 3);
        assert_eq!(add(a, b, Flags::NSW).singleton(), Some(7));
        assert_eq!(add(a, b, Flags::NONE).list(8), Some(vec![2, 4, 7]));
    }

    #[test]
    fn a_promise_that_nothing_can_keep_proves_the_code_unreachable() {
        // A hundred plus a hundred does not fit in a signed byte, and the promise says it did not
        // overflow, so there is no pair of values this could have been.
        let hundred = Range::exactly(100, 8);
        assert!(add(hundred, hundred, Flags::NSW).is_empty());
        // And the same addition without the promise is just the wrapping answer, which reads as
        // two hundred unsigned and as minus fifty six signed.
        assert_eq!(add(hundred, hundred, Flags::NONE).singleton(), Some(200));
        // The unsigned promise says the same of a sum that goes past two hundred and fifty five.
        assert!(add(Range::exactly(200, 8), hundred, Flags::NUW).is_empty());
    }

    #[test]
    fn negation_is_zero_minus_it() {
        unary("neg", |a| neg(a, Flags::NONE), |x| x.wrapping_neg() & mask(W));
    }

    #[test]
    fn multiplication_wraps_and_says_so() {
        binary("mul", |a, b| mul(a, b, Flags::NONE), |x, y| Some(x.wrapping_mul(y) & mask(W)));
        binary(
            "mul nsw",
            |a, b| mul(a, b, Flags::NSW),
            |x, y| {
                let product = as_signed(x) * as_signed(y);
                fits_signed(product).then(|| x.wrapping_mul(y) & mask(W))
            },
        );
        binary("mul nuw", |a, b| mul(a, b, Flags::NUW), |x, y| (x * y <= mask(W)).then_some(x * y));
    }

    #[test]
    fn a_product_of_even_numbers_is_known_to_be_a_multiple_of_four() {
        let evens = Range::full(32).narrow(Bits::from_parts(0, mask(32) - 1, 32));
        let product = mul(evens, evens, Flags::NONE);
        assert_eq!(product.bits().low_zeros(), 2);
        assert!(!product.contains(2));
        assert!(product.contains(4));
    }

    #[test]
    fn the_bitwise_operations_are_what_they_do_to_every_pair() {
        binary("and", and, |x, y| Some(x & y));
        binary("or", or, |x, y| Some(x | y));
        binary("xor", xor, |x, y| Some(x ^ y));
        unary("not", not, |x| !x & mask(W));
    }

    #[test]
    fn the_shifts_are_what_they_do_to_every_pair() {
        let counts = Range::between(0, u128::from(W) - 1, W);
        for count in all() {
            let count = count.intersect(counts);
            if count.is_empty() {
                continue;
            }
            for a in all() {
                let sharp = a.singleton().is_some() && count.singleton().is_some();
                for (name, got) in [
                    ("shl", shl(a, count, Flags::NONE)),
                    ("lshr", lshr(a, count, Flags::NONE)),
                    ("ashr", ashr(a, count, Flags::NONE)),
                ] {
                    let mut want: Vec<u128> = Vec::new();
                    for x in held(a) {
                        for at in held(count) {
                            let at = at as u32;
                            let value = match name {
                                "shl" => (x << at) & mask(W),
                                "lshr" => x >> at,
                                _ => (as_signed(x) >> at) as u128 & mask(W),
                            };
                            if !want.contains(&value) {
                                want.push(value);
                            }
                        }
                    }
                    want.sort_unstable();
                    check(got, &want, &format!("{name}({a:?}, {count:?})"), sharp);
                }
            }
        }
    }

    #[test]
    fn a_shift_count_that_might_be_too_large_gives_up_rather_than_guessing() {
        let a = Range::exactly(1, 8);
        assert!(shl(a, Range::between(8, 9, 8), Flags::NONE).is_full());
        assert!(shl(a, Range::between(7, 8, 8), Flags::NONE).is_full());
        assert_eq!(shl(a, Range::exactly(7, 8), Flags::NONE).singleton(), Some(0x80));
    }

    #[test]
    fn a_shift_count_with_more_values_than_are_worth_walking_still_says_something() {
        // Every count from four up, which is past the walking limit, so this takes the coarse
        // answer: whatever it shifted, the low four bits came out zero.
        let wide = Range::between(4, 31, 32);
        let shifted = shl(Range::full(32), wide, Flags::NONE);
        assert_eq!(shifted.bits().low_zeros(), 4);
        // And the same going the other way leaves the top four clear.
        assert_eq!(
            lshr(Range::full(32), wide, Flags::NONE).unsigned_bounds(),
            Some((0, 0x0fff_ffff))
        );
    }

    #[test]
    fn the_casts_are_what_they_do_to_every_value() {
        for a in all() {
            for to in 1..=6u32 {
                let mut want: Vec<u128> =
                    held(a).into_iter().map(|x| x & mask(to)).collect::<Vec<_>>();
                want.sort_unstable();
                want.dedup();
                let sharp = runs(&want) <= PAIRS;
                check(trunc(a, to), &want, &format!("trunc({a:?}, {to})"), sharp);

                let mut want: Vec<u128> = held(a).into_iter().map(|x| x & mask(W)).collect();
                want.sort_unstable();
                want.dedup();
                let sharp = runs(&want) <= PAIRS;
                check(zext(a, W + to), &want, &format!("zext({a:?}, {})", W + to), sharp);

                let mut want: Vec<u128> =
                    held(a).into_iter().map(|x| as_signed(x) as u128 & mask(W + to)).collect();
                want.sort_unstable();
                want.dedup();
                let sharp = runs(&want) <= PAIRS;
                check(sext(a, W + to), &want, &format!("sext({a:?}, {})", W + to), sharp);
            }
        }
    }

    #[test]
    fn truncating_a_run_that_wraps_round_is_still_exact() {
        let range = Range::between(0xfe, 0x101, 32);
        let low = trunc(range, 8);
        assert_eq!(held(low), [0x00, 0x01, 0xfe, 0xff]);
    }

    #[test]
    fn a_comparison_is_settled_only_when_every_pair_agrees() {
        let ranges = all();
        for &a in &ranges {
            for &b in &ranges {
                for pred in IntPred::all() {
                    let mut yes = false;
                    let mut no = false;
                    for x in held(a) {
                        for y in held(b) {
                            if holds(pred, x, y) {
                                yes = true;
                            } else {
                                no = true;
                            }
                        }
                    }
                    let want = match (yes, no) {
                        (true, false) => Truth::Always,
                        (false, true) => Truth::Never,
                        _ => Truth::Either,
                    };
                    let got = compare(pred, a, b);
                    if want == Truth::Either {
                        assert_eq!(got, Truth::Either, "{pred} {a:?} {b:?}");
                    } else {
                        assert!(
                            got == want || got == Truth::Either,
                            "{pred} {a:?} {b:?} said {got:?} and it is {want:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn narrowing_for_a_comparison_keeps_every_value_that_could_satisfy_it() {
        let ranges = all();
        for &a in &ranges {
            for &b in &ranges {
                for pred in IntPred::all() {
                    let mut want: Vec<u128> = Vec::new();
                    for x in held(a) {
                        if held(b).into_iter().any(|y| holds(pred, x, y)) {
                            want.push(x);
                        }
                    }
                    let sharp = runs(&want) <= PAIRS && b.singleton().is_some();
                    check(narrow_for(pred, a, b), &want, &format!("{pred} {a:?} {b:?}"), sharp);
                }
            }
        }
    }

    #[test]
    fn a_branch_on_a_constant_bound_gives_the_range_the_bound_says() {
        let full = Range::full(32);
        let ten = Range::exactly(10, 32);
        assert_eq!(narrow_for(IntPred::Ult, full, ten).unsigned_bounds(), Some((0, 9)));
        assert_eq!(narrow_for(IntPred::Uge, full, ten).unsigned_bounds(), Some((10, 0xffff_ffff)));
        assert_eq!(
            narrow_for(IntPred::Slt, full, ten).signed_bounds(),
            Some((i128::from(i32::MIN), 9))
        );
        assert!(narrow_for(IntPred::Ne, full, Range::exactly(0, 32)).nonzero());
    }

    #[test]
    fn the_inverses_take_a_result_back_to_an_operand_that_could_have_made_it() {
        let ranges = all();
        for &result in &ranges {
            for &other in &ranges {
                let cases: [(Undo, Forwards); 6] = [
                    (Undo::AddLeft, Box::new(|r: u128, o: u128| r.wrapping_sub(o) & mask(W))),
                    (Undo::SubRight, Box::new(|r: u128, o: u128| o.wrapping_sub(r) & mask(W))),
                    (Undo::SubLeft, Box::new(|r: u128, o: u128| r.wrapping_add(o) & mask(W))),
                    (Undo::Neg, Box::new(|r: u128, _| r.wrapping_neg() & mask(W))),
                    (Undo::Not, Box::new(|r: u128, _| !r & mask(W))),
                    (Undo::Xor, Box::new(|r: u128, o: u128| r ^ o)),
                ];
                for (undo, forwards) in cases {
                    // Every operand that could have produced a value in the result has to be in
                    // the answer, and for these operations that set is what running the operation
                    // the other way round gives.
                    let mut want: Vec<u128> = Vec::new();
                    for r in held(result) {
                        for o in held(other) {
                            let value = forwards(r, o);
                            if !want.contains(&value) {
                                want.push(value);
                            }
                        }
                    }
                    want.sort_unstable();
                    let got = backward(undo, result, other);
                    for value in &want {
                        assert!(
                            got.contains(*value),
                            "{undo:?} of {result:?} and {other:?} lost {value:#x}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn undoing_an_extension_narrows_and_can_prove_a_path_dead() {
        // A zero extension of a byte cannot have produced anything above 255, and the part of the
        // result that it could have produced is the answer.
        let result = Range::between(0x0f0, 0x1ff, 32);
        assert_eq!(
            backward(Undo::Zext(8), result, Range::full(32)).unsigned_bounds(),
            Some((0xf0, 0xff))
        );
        // Nothing a sign extension of a byte produces is in here, so the operand cannot exist.
        let impossible = Range::between(0x100, 0x1ff, 32);
        assert!(backward(Undo::Sext(8), impossible, Range::full(32)).is_empty());
    }

    /// Whether the comparison holds of these two values at the test width.
    fn holds(pred: IntPred, x: u128, y: u128) -> bool {
        let (sx, sy) = (as_signed(x), as_signed(y));
        match pred {
            IntPred::Eq => x == y,
            IntPred::Ne => x != y,
            IntPred::Ult => x < y,
            IntPred::Ule => x <= y,
            IntPred::Ugt => x > y,
            IntPred::Uge => x >= y,
            IntPred::Slt => sx < sy,
            IntPred::Sle => sx <= sy,
            IntPred::Sgt => sx > sy,
            IntPred::Sge => sx >= sy,
        }
    }
}
