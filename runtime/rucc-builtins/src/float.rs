//! Single precision arithmetic in integers, which is the reference for the entry points in
//! `runtime/builtins/float.c`: the four operations a target with no floating point unit calls, the
//! negation, the eight comparisons, which are calls on such a target too, the eight conversions
//! between a float and an integer, which is what a cast becomes there, and the pair at the bottom that
//! crosses between this format and the next one up.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8. The names and the conventions are libgcc's,
//! the same as everything else here.
//!
//! # Why this is a different shape on purpose
//!
//! The C carries three bits below the significand through every routine, a guard bit, a round bit
//! and one that remembers that something was lost, which is the least that rounds correctly and is
//! what every soft float library in use is built on. This does it the other way round: each operand
//! is taken apart into an exact integer and the power of two its lowest bit is worth, the two are
//! combined exactly, and the one rounding happens at the end from the exact value. There are no
//! guard bits here because there is nothing to guard: what the rounding reads is the whole
//! remainder.
//!
//! Two places cannot be exact, and both are arranged so that what is missing cannot change the
//! answer. An addition whose operands are more than forty powers of two apart would need hundreds
//! of bits to write down, and it does not need them: the smaller operand is then less than 2^-16 of
//! the lowest bit the answer can hold, so the answer is the larger operand and the proof is two
//! lines rather than a wider integer. A division does not terminate, so it is run to forty bits of
//! quotient and what the remainder says is carried as "and a little more", which is read only where
//! the rounding is otherwise an exact tie.
//!
//! # Why the division below is on a `u64`
//!
//! A `/` on a `u128` in this crate is a call to `__udivti3`, which `div.rs` next door defines, so a
//! reference written that way would be leaning on another reference. The dividend here stays inside
//! a `u64`, which every target the ladder names divides in instructions, and the shift that gets
//! forty bits of quotient out of it is worked out from the two significands rather than fixed,
//! because a subnormal operand brings as few as one bit with it.
//!
//! # What it does with a not a number
//!
//! It comes back quieted, and the left one where both operands are one. That is what this machine's
//! own arithmetic does with the operands in the same order and it is what the C does, so the two
//! sides can be compared bit for bit instead of through a canonicalization that would hide a real
//! disagreement. It is not libgcc's, which decides that one out of a per architecture header while
//! one archive here serves every row of the target matrix, and `runtime/builtins/quad.c` is where
//! that is written down at length. An operation with no answer at all, an infinity less an infinity,
//! a zero times an infinity, or a zero over a zero, gives the quiet not a number with an empty
//! payload and a clear sign, which is this library's rule for the same reason.

// The pair of routines at the bottom of this file crosses between the two formats, so it reads the
// wider format's fields and its rounding from next door rather than writing a second copy of either.
use crate::double;

/// How many bits of the significand the format writes down, the other one being implied.
const FRACTION: u32 = 23;

/// What is added to an exponent before it is stored.
const BIAS: i32 = 127;

/// The stored exponent of an infinity and of a not a number.
const TOP: u32 = 255;

const SIGN: u32 = 0x8000_0000;
const IMPLICIT: u32 = 0x0080_0000;
const FRACTION_MASK: u32 = 0x007f_ffff;

/// The top bit of the fraction, which is what tells a quiet not a number from a signalling one.
const QUIET: u32 = 0x0040_0000;

/// The not a number an operation with no answer produces.
const EMPTY_NAN: u32 = 0x7fc0_0000;

/// What the lowest bit of the smallest subnormal is worth, which is the lowest any answer can hold.
const SMALLEST: i32 = 1 - BIAS - FRACTION as i32;

/// How far apart two exponents have to be before the smaller operand cannot reach the answer.
///
/// Past this the smaller operand is below 2^-16 of the lowest bit the answer holds, so the answer
/// is the larger operand itself. Twenty seven would do; forty is here because it keeps the aligned
/// pair inside sixty four bits with room to spare and because nothing is paid for the margin.
const TOO_FAR: i32 = 40;

/// A float's magnitude taken apart exactly: it is `significand * 2^scale`. The sign is not in here
/// because every routine below reads it off the bits, where it is one bit and no arithmetic.
struct Parts {
    significand: u64,
    scale: i32,
}

/// The bits of a float as the parts the arithmetic below works on.
///
/// A subnormal and the smallest normal share the same lowest bit, which is what makes this two
/// lines rather than a case analysis: the only difference between them is the leading one.
fn parts(bits: u32) -> Parts {
    let stored = (bits >> FRACTION) & TOP;
    let fraction = bits & FRACTION_MASK;
    if stored == 0 {
        Parts { significand: u64::from(fraction), scale: SMALLEST }
    } else {
        Parts {
            significand: u64::from(fraction | IMPLICIT),
            scale: stored as i32 - BIAS - FRACTION as i32,
        }
    }
}

fn is_nan(bits: u32) -> bool {
    (bits >> FRACTION) & TOP == TOP && bits & FRACTION_MASK != 0
}

fn is_infinite(bits: u32) -> bool {
    (bits >> FRACTION) & TOP == TOP && bits & FRACTION_MASK == 0
}

fn is_zero(bits: u32) -> bool {
    bits & !SIGN == 0
}

fn quiet(bits: u32) -> f32 {
    f32::from_bits(bits | QUIET)
}

fn infinity(sign: u32) -> f32 {
    f32::from_bits(sign | (TOP << FRACTION))
}

/// The float nearest `magnitude * 2^scale`, with `sign` for its sign bit.
///
/// `above` says the value is really a little more than that, by less than one unit of the
/// magnitude's lowest bit, which is what a division leaves behind. It is read where the rounding is
/// an exact tie and nowhere else, because anywhere else a value that small cannot reach the
/// decision.
fn round_from(sign: u32, magnitude: u128, scale: i32, above: bool) -> f32 {
    if magnitude == 0 {
        return f32::from_bits(sign);
    }

    // Which power of two the highest set bit is worth.
    let leading = scale + (127 - magnitude.leading_zeros() as i32);

    // Where the answer's lowest bit has to sit: twenty three below the leading one while that is
    // inside the normal range, and at the bottom of the format below it, which is what makes a
    // subnormal lose precision rather than range.
    let lowest = if leading >= 1 - BIAS { leading - FRACTION as i32 } else { SMALLEST };

    let drop = lowest - scale;
    let mut kept = if drop <= 0 {
        magnitude << (-drop) as u32
    } else if drop >= 128 {
        // Everything is below the answer's lowest bit, so what is left is the rounding decision
        // against zero, which only ever goes down this far out.
        0
    } else {
        let below = magnitude & ((1u128 << drop) - 1);
        let tie = 1u128 << (drop - 1);
        let mut kept = magnitude >> drop;
        if below > tie || (below == tie && (above || kept & 1 == 1)) {
            kept += 1;
        }
        kept
    };
    let mut lowest = lowest;
    if kept >> (FRACTION + 1) != 0 {
        // The rounding carried out of the top, which can only have made it a power of two.
        kept >>= 1;
        lowest += 1;
    }

    if kept == 0 {
        return f32::from_bits(sign);
    }
    if kept < u128::from(IMPLICIT) {
        // A subnormal, whose stored exponent is zero and whose fraction is the whole significand.
        return f32::from_bits(sign | kept as u32);
    }
    let stored = lowest + FRACTION as i32 + BIAS;
    if stored >= TOP as i32 {
        return infinity(sign);
    }
    f32::from_bits(sign | ((stored as u32) << FRACTION) | (kept as u32 & FRACTION_MASK))
}

/// The sum of two floats, which is also the difference once the caller flips a sign.
fn add(left: u32, right: u32) -> f32 {
    if is_nan(left) {
        return quiet(left);
    }
    if is_nan(right) {
        return quiet(right);
    }
    if is_infinite(left) {
        if is_infinite(right) && (left ^ right) & SIGN != 0 {
            return f32::from_bits(EMPTY_NAN);
        }
        return f32::from_bits(left);
    }
    if is_infinite(right) {
        return f32::from_bits(right);
    }
    if is_zero(left) && is_zero(right) {
        // Negative only when both of them are, which is what rounding to nearest asks for.
        return f32::from_bits(left & right & SIGN);
    }
    if is_zero(left) {
        return f32::from_bits(right);
    }
    if is_zero(right) {
        return f32::from_bits(left);
    }

    // The magnitude of a float is in the order of its bits, so the larger of the two is the one
    // with the larger pattern once the signs are off. The answer takes its sign from that one.
    let (larger, smaller) =
        if left & !SIGN >= right & !SIGN { (left, right) } else { (right, left) };
    let opposite = (left ^ right) & SIGN != 0;
    let big = parts(larger);
    let small = parts(smaller);
    let sign = larger & SIGN;

    if big.scale - small.scale > TOO_FAR {
        // The smaller operand is below 2^-16 of the lowest bit the answer can hold, so the answer
        // is the larger operand and no arithmetic is needed to say so.
        return f32::from_bits(larger);
    }

    let aligned = u128::from(big.significand) << (big.scale - small.scale) as u32;
    let other = u128::from(small.significand);
    if opposite {
        if aligned == other {
            // A number less itself, which is a positive zero in this rounding mode.
            return 0.0;
        }
        round_from(sign, aligned - other, small.scale, false)
    } else {
        round_from(sign, aligned + other, small.scale, false)
    }
}

/// `float __addsf3(float, float)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __addsf3(left: f32, right: f32) -> f32 {
    add(left.to_bits(), right.to_bits())
}

/// The right operand of a subtraction, which is the one it was with its sign flipped, except that a
/// not a number is handed on as it came in. That one is the answer itself rather than a number being
/// negated, and soft-fp negates after it has dealt with a not a number, so flipping it would print
/// minus where GCC prints nothing.
fn negated(right: u32) -> u32 {
    if is_nan(right) { right } else { right ^ SIGN }
}

/// `float __subsf3(float, float)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __subsf3(left: f32, right: f32) -> f32 {
    add(left.to_bits(), negated(right.to_bits()))
}

/// The product of two floats, which is exact before it is rounded: two significands of twenty four
/// bits multiply into forty eight, and the answer is that product rounded once.
fn multiply(left: u32, right: u32) -> f32 {
    if is_nan(left) {
        return quiet(left);
    }
    if is_nan(right) {
        return quiet(right);
    }
    let sign = (left ^ right) & SIGN;
    if is_infinite(left) || is_infinite(right) {
        if is_zero(left) || is_zero(right) {
            return f32::from_bits(EMPTY_NAN);
        }
        return infinity(sign);
    }
    if is_zero(left) || is_zero(right) {
        return f32::from_bits(sign);
    }
    let left = parts(left);
    let right = parts(right);
    let product = u128::from(left.significand * right.significand);
    round_from(sign, product, left.scale + right.scale, false)
}

/// `float __mulsf3(float, float)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __mulsf3(left: f32, right: f32) -> f32 {
    multiply(left.to_bits(), right.to_bits())
}

/// How many bits of quotient the division below makes sure of.
///
/// The rounding needs twenty four and then enough to see a tie, so twenty seven would do. Forty is
/// what is asked for because the shift that gets it still leaves the dividend inside a `u64`
/// whatever the two significands are, and nothing is paid for the margin.
const QUOTIENT_BITS: u32 = 40;

/// The quotient of two floats, to forty bits and a remainder that says whether there was anything
/// after them.
fn divide(left: u32, right: u32) -> f32 {
    if is_nan(left) {
        return quiet(left);
    }
    if is_nan(right) {
        return quiet(right);
    }
    let sign = (left ^ right) & SIGN;
    if is_infinite(left) {
        if is_infinite(right) {
            return f32::from_bits(EMPTY_NAN);
        }
        return infinity(sign);
    }
    if is_infinite(right) {
        return f32::from_bits(sign);
    }
    if is_zero(right) {
        // A zero over a zero has no answer. Anything else over a zero is an infinity, which is the
        // one division by zero IEEE 754 gives a value to.
        if is_zero(left) {
            return f32::from_bits(EMPTY_NAN);
        }
        return infinity(sign);
    }
    if is_zero(left) {
        return f32::from_bits(sign);
    }
    let left = parts(left);
    let right = parts(right);

    // How far the dividend goes up before the division. It is worked out from the two widths rather
    // than fixed, because a subnormal operand is a significand with as few as one bit in it, and a
    // fixed shift would then produce a quotient with fewer bits than the answer needs. What this
    // asks for is that the quotient comes out at `QUOTIENT_BITS` wide whatever arrived.
    let up = QUOTIENT_BITS + left.significand.leading_zeros() - right.significand.leading_zeros();
    let dividend = left.significand << up;
    let quotient = dividend / right.significand;
    let above = dividend % right.significand != 0;
    round_from(sign, u128::from(quotient), left.scale - right.scale - up as i32, above)
}

/// `float __divsf3(float, float)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __divsf3(left: f32, right: f32) -> f32 {
    divide(left.to_bits(), right.to_bits())
}

/// `float __negsf2(float)`.
///
/// The sign bit and nothing else, which is true of a not a number too: the sign of one says
/// nothing, and flipping it rather than quieting it is what libgcc does here.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __negsf2(value: f32) -> f32 {
    f32::from_bits(value.to_bits() ^ SIGN)
}

/// The comparisons, worked out from the exact parts rather than from the bit patterns.
///
/// The shipped C reads the two patterns as integers, which works because the format was designed so
/// that two floats of the same sign order the way their patterns do. This does not use that at all:
/// it asks which power of two each value's highest bit is worth, and only where those agree does it
/// line the two significands up and compare them. Slower and beside the point on a real target,
/// which is why the C is the one that ships, and independent of the property the C leans on, which
/// is why it is the one that checks it.
///
/// An infinity needs no case of its own here. `parts` hands back its stored exponent like anything
/// else, which puts its highest bit one power of two above the largest finite value's. So it is
/// greater than everything finite and equal to another infinity by the ordinary path.
fn order(left: u32, right: u32) -> core::cmp::Ordering {
    use core::cmp::Ordering;

    let left_parts = parts(left);
    let right_parts = parts(right);
    let left_negative = left & SIGN != 0;
    let right_negative = right & SIGN != 0;

    // A zero of either sign equals a zero of either sign, which is the one place the sign is not
    // read, and it has to come first because a zero has no highest bit to ask about.
    if left_parts.significand == 0 && right_parts.significand == 0 {
        return Ordering::Equal;
    }
    if left_parts.significand == 0 {
        return if right_negative { Ordering::Greater } else { Ordering::Less };
    }
    if right_parts.significand == 0 {
        return if left_negative { Ordering::Less } else { Ordering::Greater };
    }
    if left_negative != right_negative {
        return if left_negative { Ordering::Less } else { Ordering::Greater };
    }

    // Which power of two the highest set bit of each is worth, and then, where those agree, the two
    // significands with their highest bits in the same place.
    let left_top = left_parts.scale + (63 - left_parts.significand.leading_zeros() as i32);
    let right_top = right_parts.scale + (63 - right_parts.significand.leading_zeros() as i32);
    let magnitudes = left_top.cmp(&right_top).then_with(|| {
        let left_lined = left_parts.significand << left_parts.significand.leading_zeros();
        let right_lined = right_parts.significand << right_parts.significand.leading_zeros();
        left_lined.cmp(&right_lined)
    });
    if left_negative { magnitudes.reverse() } else { magnitudes }
}

/// What the eight entry points below share: the comparison, and the one number that differs between
/// them, which is what to answer when an operand is a not a number. That answer has to make the
/// caller's test fail, and which answer does that depends on the test the caller is going to make,
/// which is why there are eight of these and not one.
fn compare(left: u32, right: u32, unordered: i32) -> i32 {
    use core::cmp::Ordering;

    if is_nan(left) || is_nan(right) {
        return unordered;
    }
    match order(left, right) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// `int __cmpsf2(float, float)`, which is minus one, zero or one, and one for a not a number as
/// well. The documentation says not to rely on that last part, and the compiler emits this routine
/// only where it has already ruled a not a number out.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __cmpsf2(left: f32, right: f32) -> i32 {
    compare(left.to_bits(), right.to_bits(), 1)
}

/// `int __eqsf2(float, float)`, zero when the two are equal and anything else when they are not. A
/// not a number is unequal to everything including itself.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __eqsf2(left: f32, right: f32) -> i32 {
    compare(left.to_bits(), right.to_bits(), 1)
}

/// `int __nesf2(float, float)`, which is the same work, since a caller testing for inequality tests
/// the same answer against zero the other way round. Two names because a compiler emits both.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __nesf2(left: f32, right: f32) -> i32 {
    compare(left.to_bits(), right.to_bits(), 1)
}

/// `int __gesf2(float, float)`, at or above zero when the left one is greater or equal, so a not a
/// number has to come back below zero for that test to fail.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __gesf2(left: f32, right: f32) -> i32 {
    compare(left.to_bits(), right.to_bits(), -1)
}

/// `int __gtsf2(float, float)`, above zero when the left one is greater.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __gtsf2(left: f32, right: f32) -> i32 {
    compare(left.to_bits(), right.to_bits(), -1)
}

/// `int __lesf2(float, float)`, at or below zero when the left one is less or equal.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __lesf2(left: f32, right: f32) -> i32 {
    compare(left.to_bits(), right.to_bits(), 1)
}

/// `int __ltsf2(float, float)`, below zero when the left one is less.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __ltsf2(left: f32, right: f32) -> i32 {
    compare(left.to_bits(), right.to_bits(), 1)
}

/// `int __unordsf2(float, float)`, not zero when the two cannot be ordered at all.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __unordsf2(left: f32, right: f32) -> i32 {
    i32::from(is_nan(left.to_bits()) || is_nan(right.to_bits()))
}

/// The largest magnitude each of the four integer types holds. A signed type holds one more going
/// down than it does going up, which is why the sign and the magnitude travel separately below.
const SIGNED_32: u128 = 1 << 31;
const SIGNED_64: u128 = 1 << 63;
const UNSIGNED_32: u128 = u32::MAX as u128;
const UNSIGNED_64: u128 = u64::MAX as u128;

/// The float nearest an integer, with the sign handed in separately so that the caller can take the
/// magnitude of the most negative value of its type without overflowing it.
///
/// One line, because an integer is already a significand and a scale of zero, and `round_from` is
/// the routine that turns those into the nearest float. The shipped C has to do the alignment and
/// the sticky bit by hand, which is the difference between the two shapes rather than a difference
/// in what they answer.
fn float_from_integer(sign: u32, magnitude: u64) -> f32 {
    round_from(sign, u128::from(magnitude), 0, false)
}

/// `float __floatsisf(int)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatsisf(value: i32) -> f32 {
    float_from_integer(if value < 0 { SIGN } else { 0 }, u64::from(value.unsigned_abs()))
}

/// `float __floatunsisf(unsigned int)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatunsisf(value: u32) -> f32 {
    float_from_integer(0, u64::from(value))
}

/// `float __floatdisf(long long)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatdisf(value: i64) -> f32 {
    float_from_integer(if value < 0 { SIGN } else { 0 }, value.unsigned_abs())
}

/// `float __floatundisf(unsigned long long)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatundisf(value: u64) -> f32 {
    float_from_integer(0, value)
}

/// The part of a float that is on the integer side of the point, as a sign and a magnitude.
///
/// `None` where there is no integer part to hand back at all, which is an infinity, a not a number,
/// and a magnitude past what the widest of the four types holds. A value below one is not that case:
/// its integer part is zero, which is an answer.
fn integer_from_float(bits: u32) -> Option<(u32, u128)> {
    if (bits >> FRACTION) & TOP == TOP {
        return None;
    }
    let sign = bits & SIGN;
    let parts = parts(bits);
    if parts.scale >= 0 {
        if parts.scale > 64 {
            return None;
        }
        Some((sign, u128::from(parts.significand) << parts.scale))
    } else if -parts.scale >= 64 {
        Some((sign, 0))
    } else {
        Some((sign, u128::from(parts.significand >> -parts.scale)))
    }
}

/// What the two signed routines share: the integer part held to the bounds of the caller's type.
/// `bound` is the largest magnitude that type holds going down, and one more than the largest it
/// holds going up.
///
/// `None` where the value has no answer that type can hold, which the entry points turn into the
/// zero that section 12.8 records as the shared convention for a case C leaves undefined.
fn truncate(bits: u32, bound: u128) -> Option<i128> {
    let (sign, magnitude) = integer_from_float(bits)?;
    let negative = sign != 0 && magnitude != 0;
    let largest = if negative { bound } else { bound - 1 };
    if magnitude > largest {
        return None;
    }
    Some(if negative { -(magnitude as i128) } else { magnitude as i128 })
}

/// What the two unsigned routines share. A negative value is undefined for these in C, so it is the
/// same zero as the out of range case, and a negative value whose integer part is zero is not that:
/// the answer there is zero because that is the value.
fn truncate_unsigned(bits: u32, bound: u128) -> Option<u128> {
    let (sign, magnitude) = integer_from_float(bits)?;
    if sign != 0 && magnitude != 0 {
        return None;
    }
    if magnitude > bound {
        return None;
    }
    Some(magnitude)
}

/// `int __fixsfsi(float)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixsfsi(value: f32) -> i32 {
    truncate(value.to_bits(), SIGNED_32).map_or(0, |answer| answer as i32)
}

/// `unsigned int __fixunssfsi(float)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixunssfsi(value: f32) -> u32 {
    truncate_unsigned(value.to_bits(), UNSIGNED_32).map_or(0, |answer| answer as u32)
}

/// `long long __fixsfdi(float)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixsfdi(value: f32) -> i64 {
    truncate(value.to_bits(), SIGNED_64).map_or(0, |answer| answer as i64)
}

/// `unsigned long long __fixunssfdi(float)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixunssfdi(value: f32) -> u64 {
    truncate_unsigned(value.to_bits(), UNSIGNED_64).map_or(0, |answer| answer as u64)
}

/// How far a fraction moves between the two formats, which is also how far a payload moves.
///
/// The one consequence worth naming is that the quiet bit needs no case of its own in either
/// direction: the narrow format's is bit twenty two, the wide one's is bit fifty one, and the
/// distance between the two fractions is twenty nine.
const BETWEEN: u32 = double::FRACTION - FRACTION;

/// `double __extendsfdf2(float)`, the widening.
///
/// Both directions are one line of arithmetic here, and the line is the same one: take the input
/// apart into the exact value it is and round that once into the other format. That the widening
/// never rounds is then a property of the two formats rather than something this code relies on,
/// which is the better way round for a reference to have it, and the test is what demonstrates it.
/// The shipped C in `runtime/builtins/float.c` moves the fields instead, and normalizes by hand for
/// the one input where a field move is not enough, which is a float subnormal.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __extendsfdf2(value: f32) -> f64 {
    widen(value.to_bits())
}

fn widen(bits: u32) -> f64 {
    let sign = u64::from(bits & SIGN) << 32;
    if is_nan(bits) {
        let payload = u64::from(bits & FRACTION_MASK) << BETWEEN;
        let quiet = u64::from(QUIET) << BETWEEN;
        return f64::from_bits(sign | (double::TOP << double::FRACTION) | payload | quiet);
    }
    if is_infinite(bits) {
        return f64::from_bits(sign | (double::TOP << double::FRACTION));
    }
    let taken = parts(bits);
    double::round_from(sign, u128::from(taken.significand), taken.scale, false)
}

/// `float __truncdfsf2(double)`, the narrowing, which is the direction that rounds.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __truncdfsf2(value: f64) -> f32 {
    narrow(value.to_bits())
}

fn narrow(bits: u64) -> f32 {
    let sign = (bits >> 32) as u32 & SIGN;
    if double::is_nan(bits) {
        // The payload loses its bottom twenty nine bits, so one that lived only down there comes
        // back empty. The quiet bit goes on afterwards, so what comes back is a not a number either
        // way rather than the infinity that would be wrong rather than merely lossy.
        let payload = ((bits & double::FRACTION_MASK) >> BETWEEN) as u32;
        return f32::from_bits(sign | (TOP << FRACTION) | payload | QUIET);
    }
    if double::is_infinite(bits) {
        return infinity(sign);
    }
    let taken = double::parts(bits);
    round_from(sign, u128::from(taken.significand), taken.scale, false)
}

#[cfg(test)]
mod tests {
    use std::hint::black_box;
    use std::vec::Vec;

    use super::*;

    /// A pseudorandom stream, so the cases below are the same cases on every machine and a failure
    /// is a failure anyone can reproduce. xorshift64, the same one `div.rs` uses.
    struct Stream(u64);

    impl Stream {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// Any float at all, out of random bits, which is how the infinities, the not a numbers and
        /// the subnormals get in: each is a slice of the bit patterns and one in every two hundred
        /// and fifty six patterns lands in it.
        fn any(&mut self) -> f32 {
            f32::from_bits(self.next() as u32)
        }

        /// A float whose exponent is near another one's, which is where the interesting additions
        /// are: two values far apart add to the larger and say nothing about the alignment.
        fn near(&mut self, other: f32) -> f32 {
            let bits = other.to_bits();
            let stored = (bits >> FRACTION) & TOP;
            let moved = (stored as i32 + (self.next() % 9) as i32 - 4).clamp(0, 254) as u32;
            f32::from_bits((self.next() as u32 & (SIGN | FRACTION_MASK)) | (moved << FRACTION))
        }
    }

    /// The machine's own arithmetic, which is the answer these four routines have to give.
    ///
    /// `black_box` on the way in and out because a constant folded at compile time is the
    /// compiler's arithmetic rather than the machine's, and the two have been known to differ on a
    /// not a number. What this checks is the hardware instruction.
    fn machine(which: u8, left: f32, right: f32) -> f32 {
        let left = black_box(left);
        let right = black_box(right);
        black_box(match which {
            0 => left + right,
            1 => left - right,
            2 => left * right,
            _ => left / right,
        })
    }

    fn ours(which: u8, left: f32, right: f32) -> f32 {
        match which {
            0 => add(left.to_bits(), right.to_bits()),
            1 => add(left.to_bits(), negated(right.to_bits())),
            2 => multiply(left.to_bits(), right.to_bits()),
            _ => divide(left.to_bits(), right.to_bits()),
        }
    }

    /// What `which` was, for a failure message somebody has to read.
    fn name(which: u8) -> &'static str {
        match which {
            0 => "+",
            1 => "-",
            2 => "*",
            _ => "/",
        }
    }

    /// Holds one case against the machine.
    ///
    /// Two not a numbers count as the same answer whatever their payloads say. A payload is a per
    /// architecture convention rather than an answer, this runs on three architectures, and the
    /// thing that does hold the payloads to each other is `cargo xtask builtins-diff`, where both
    /// sides are on the same machine and the comparison is bit for bit.
    fn check(which: u8, left: f32, right: f32) {
        let wanted = machine(which, left, right);
        let got = ours(which, left, right);
        if wanted.is_nan() && got.is_nan() {
            return;
        }
        assert_eq!(
            got.to_bits(),
            wanted.to_bits(),
            "{:08x} {} {:08x}: got {:08x}, the machine says {:08x}",
            left.to_bits(),
            name(which),
            right.to_bits(),
            got.to_bits(),
            wanted.to_bits()
        );
    }

    /// The values worth asking about by name rather than by luck: both zeros, the ends of the
    /// subnormal range, the ends of the normal one, the powers of two either side of a rounding
    /// decision, and the two kinds of not a number.
    fn corners() -> Vec<f32> {
        let mut out = Vec::new();
        for bits in [
            0x0000_0000, // zero
            0x0000_0001, // the smallest subnormal
            0x0000_0002,
            0x007f_ffff, // the largest subnormal
            0x0080_0000, // the smallest normal
            0x0080_0001,
            0x3f7f_ffff, // one less than one
            0x3f80_0000, // one
            0x3f80_0001, // one more than one
            0x4000_0000, // two
            0x4b00_0000, // 2^23, where the spacing of the floats reaches one
            0x4b80_0000, // 2^24, where it passes one
            0x7f7f_ffff, // the largest finite
            0x7f80_0000, // an infinity
            0x7fc0_0000, // a quiet not a number
            0x7f80_0001, // a signalling one
            0x3400_0000, // 2^-23, half the spacing at one
            0x3380_0000, // 2^-24, which makes a tie when added to one
        ] {
            out.push(f32::from_bits(bits));
            out.push(f32::from_bits(bits | SIGN));
        }
        out
    }

    #[test]
    fn the_four_operations_are_the_machines_over_random_bits() {
        let mut stream = Stream(0x2545_F491_4F6C_DD1D);
        for _ in 0..60_000 {
            let left = stream.any();
            let right = stream.any();
            let close = stream.near(left);
            for which in 0..4 {
                check(which, left, right);
                check(which, left, close);
                check(which, close, left);
            }
        }
    }

    #[test]
    fn the_corners_come_out_the_way_the_machine_says_too() {
        let values = corners();
        for left in &values {
            for right in &values {
                for which in 0..4 {
                    check(which, *left, *right);
                }
            }
        }
    }

    #[test]
    fn a_tie_goes_to_the_even_one() {
        // One plus half the spacing at one is exactly between one and the float above it, and one
        // is the even of the two. One more than one plus the same amount is exactly between two
        // odd neighbours, and rounding up is what reaches the even one there.
        let half = f32::from_bits(0x3380_0000);
        let mut stream = Stream(0x9E37_79B9_7F4A_7C15);
        check(0, 1.0, half);
        check(0, f32::from_bits(0x3f80_0001), half);
        for _ in 0..20_000 {
            // A random normal and exactly half of its own spacing, which is a tie at every
            // exponent rather than only at the one above.
            let value = f32::from_bits(stream.next() as u32 & 0x3fff_ffff | 0x2000_0000);
            let stored = (value.to_bits() >> FRACTION) & TOP;
            let step = f32::from_bits((stored - FRACTION - 1) << FRACTION);
            check(0, value, step);
            check(1, value, step);
        }
    }

    /// What the eight comparisons have to agree with, which is the machine's own comparison of the
    /// same two values: each routine is a sign and a test, and the test has to come out the way `<`
    /// or `==` does here, including where one operand is a not a number and every test but the last
    /// two is false.
    fn check_comparisons(left: f32, right: f32) {
        let one = black_box(left);
        let two = black_box(right);
        let bits = (left.to_bits(), right.to_bits());
        let unordered = left.is_nan() || right.is_nan();
        // The three way answer first, since it is the one routine whose whole sign is specified
        // rather than one side of zero: minus one below, one above, zero equal, and one for a not a
        // number because that is what libgcc hands back there.
        let wanted = if unordered {
            1
        } else if black_box(one < two) {
            -1
        } else if black_box(one > two) {
            1
        } else {
            0
        };
        assert_eq!(
            compare(bits.0, bits.1, 1).signum(),
            wanted,
            "cmp of {:08x} and {:08x}",
            bits.0,
            bits.1
        );

        let answers: [(&str, bool, bool); 7] = [
            ("eq", compare(bits.0, bits.1, 1) == 0, black_box(one == two)),
            ("ne", compare(bits.0, bits.1, 1) != 0, black_box(one != two)),
            ("ge", compare(bits.0, bits.1, -1) >= 0, black_box(one >= two)),
            ("gt", compare(bits.0, bits.1, -1) > 0, black_box(one > two)),
            ("le", compare(bits.0, bits.1, 1) <= 0, black_box(one <= two)),
            ("lt", compare(bits.0, bits.1, 1) < 0, black_box(one < two)),
            ("unord", i32::from(is_nan(bits.0) || is_nan(bits.1)) != 0, unordered),
        ];
        for (name, ours, machine) in answers {
            assert_eq!(
                ours, machine,
                "{name} of {:08x} and {:08x}: we say {ours}, the machine says {machine}",
                bits.0, bits.1
            );
        }
    }

    #[test]
    fn the_comparisons_answer_what_the_machine_answers() {
        let mut stream = Stream(0x1234_5678_9ABC_DEF1);
        for _ in 0..60_000 {
            let left = stream.any();
            let right = stream.any();
            check_comparisons(left, right);
            check_comparisons(left, stream.near(left));
            // The same value on both sides, which is the case an implementation that compares
            // patterns and one that compares values can still differ on: a negative zero against a
            // positive one, and a not a number against itself.
            check_comparisons(left, left);
        }
        let values = corners();
        for left in &values {
            for right in &values {
                check_comparisons(*left, *right);
            }
        }
    }

    #[test]
    fn the_signs_of_zero_are_the_ones_ieee_asks_for() {
        let plus = 0.0f32;
        let minus = f32::from_bits(SIGN);
        // A positive and a negative zero add to a positive one, a number less itself is positive,
        // and a product or a quotient takes the sign of the two signs multiplied.
        for which in 0..4 {
            check(which, plus, minus);
            check(which, minus, plus);
            check(which, minus, minus);
            check(which, plus, plus);
        }
        check(1, 1.5, 1.5);
        check(1, -1.5, -1.5);
        check(2, -0.0, 3.0);
        check(3, -0.0, 3.0);
        check(3, 3.0, -0.0);
    }

    #[test]
    fn the_bottom_of_the_range_loses_precision_rather_than_range() {
        let mut stream = Stream(0x0BAD_C0DE_0BAD_C0DE);
        for _ in 0..20_000 {
            // Subnormals, and normals near the boundary, through all four operations: this is
            // where a result has fewer bits than the format usually keeps and where an
            // implementation that rounded before it shifted gets a different answer.
            let small = f32::from_bits(stream.next() as u32 & (SIGN | 0x00ff_ffff));
            let other = f32::from_bits(stream.next() as u32 & (SIGN | 0x00ff_ffff));
            let large = f32::from_bits(stream.next() as u32 | 0x7000_0000);
            for which in 0..4 {
                check(which, small, other);
                check(which, small, large);
                check(which, large, small);
            }
        }
    }

    /// Holds one integer against the float the machine makes of it, which for a value with more
    /// than twenty four significant bits is a rounding and not a widening.
    fn check_up_signed(value: i64) {
        let wanted = black_box(black_box(value) as f32);
        let sign = if value < 0 { SIGN } else { 0 };
        let got = float_from_integer(sign, value.unsigned_abs());
        assert_eq!(
            got.to_bits(),
            wanted.to_bits(),
            "{value} as a float: got {:08x}, the machine says {:08x}",
            got.to_bits(),
            wanted.to_bits()
        );
    }

    fn check_up_unsigned(value: u64) {
        let wanted = black_box(black_box(value) as f32);
        let got = float_from_integer(0, value);
        assert_eq!(
            got.to_bits(),
            wanted.to_bits(),
            "{value} as a float: got {:08x}, the machine says {:08x}",
            got.to_bits(),
            wanted.to_bits()
        );
    }

    /// Holds one float against the integers the machine truncates it to, in all four types.
    ///
    /// Rust's own conversion saturates and hands back zero for a not a number, where C leaves all
    /// three of those undefined and these routines answer zero. So the machine is the oracle inside
    /// the range, and outside it what is checked is the convention both implementations keep.
    fn check_down(value: f32) {
        let bits = value.to_bits();
        let ours = (
            truncate(bits, SIGNED_32).map_or(0, |answer| answer as i32),
            truncate(bits, SIGNED_64).map_or(0, |answer| answer as i64),
            truncate_unsigned(bits, UNSIGNED_32).map_or(0, |answer| answer as u32),
            truncate_unsigned(bits, UNSIGNED_64).map_or(0, |answer| answer as u64),
        );
        if value.is_nan() {
            assert_eq!(ours, (0, 0, 0, 0), "{:08x} is a not a number", bits);
            return;
        }
        // The integer part, and then the four bounds as floats. Each bound is a power of two and so
        // is exact, and the test is strict on the way up because the type holds one less there.
        let whole = black_box(black_box(value).trunc());
        let inside_signed_32 = (-2147483648.0..2147483648.0).contains(&whole);
        let inside_signed_64 = (-9223372036854775808.0..9223372036854775808.0).contains(&whole);
        let inside_unsigned_32 = (0.0..4294967296.0).contains(&whole);
        let inside_unsigned_64 = (0.0..18446744073709551616.0).contains(&whole);
        let wanted = (
            if inside_signed_32 { whole as i32 } else { 0 },
            if inside_signed_64 { whole as i64 } else { 0 },
            if inside_unsigned_32 { whole as u32 } else { 0 },
            if inside_unsigned_64 { whole as u64 } else { 0 },
        );
        assert_eq!(ours, wanted, "{:08x} as integers, which truncates to {whole}", bits);
    }

    #[test]
    fn an_integer_becomes_the_float_the_machine_makes_of_it() {
        let mut stream = Stream(0x5DEE_CE66_D000_0005);
        for _ in 0..60_000 {
            let wide = stream.next();
            // Each value at both widths, so that one that fits in thirty two bits is asked of the
            // thirty two bit routine and of the sixty four bit one as well.
            check_up_signed(i64::from(wide as i32));
            check_up_signed(wide as i64);
            check_up_unsigned(u64::from(wide as u32));
            check_up_unsigned(wide);
            // And a value just past 2^24, which is where a float stops holding every integer and
            // where the rounding starts to matter. Random bits land above it nearly always and
            // these land just above it, which is a different case.
            let near = (wide % (1 << 30)) + (1 << 24);
            check_up_signed(near as i64);
            check_up_unsigned(near);
        }
        for value in [
            0i64,
            1,
            -1,
            (1 << 24) - 1,
            1 << 24,
            (1 << 24) + 1, // the first integer a float cannot hold, which rounds down to 2^24
            (1 << 24) + 2,
            (1 << 24) + 3, // exactly between two floats, so the even one wins
            i64::from(i32::MAX),
            i64::from(i32::MIN),
            i64::MAX, // rounds up, and the rounding carries into the exponent
            i64::MIN,
        ] {
            check_up_signed(value);
            // Wrapping, because the most negative value is in the list and it is its own negation.
            check_up_signed(value.wrapping_neg());
            check_up_unsigned(value.unsigned_abs());
        }
        check_up_unsigned(u64::MAX);
        check_up_unsigned(u64::from(u32::MAX));
    }

    #[test]
    fn a_float_becomes_the_integer_it_truncates_to_where_the_type_holds_one() {
        let mut stream = Stream(0xDEAD_BEEF_CAFE_0001);
        for _ in 0..60_000 {
            check_down(stream.any());
            // Random bits are almost never a value with an integer part inside any of the four
            // ranges, so most of those cases would be the out of range convention and would say
            // nothing about the shift. This puts the exponent where the shift happens: from 2^-27,
            // which truncates to zero, to 2^67, which is past all four types.
            let stored = 100 + (stream.next() % 95) as u32;
            let fraction = stream.next() as u32 & (SIGN | FRACTION_MASK);
            check_down(f32::from_bits(fraction | (stored << FRACTION)));
        }
        for value in corners() {
            check_down(value);
        }
        for value in [
            0.5f32,
            -0.5, // a negative value whose integer part is zero, which is defined for unsigned too
            1.5,
            -1.5,
            2147483648.0,  // one past the largest int, and exactly the most negative one
            -2147483648.0, // the most negative int, which is the case a negation would overflow
            4294967296.0,
            9223372036854775808.0,
            -9223372036854775808.0,
            18446744073709551616.0,
            1e30,
            -1e30,
        ] {
            check_down(value);
        }
    }

    #[test]
    fn the_top_of_the_range_rounds_to_an_infinity() {
        let largest = f32::from_bits(0x7f7f_ffff);
        let half_step = f32::from_bits(0x73000000);
        check(0, largest, largest);
        check(0, largest, half_step);
        check(2, largest, 2.0);
        check(3, largest, 0.5);
        check(1, -largest, largest);
    }

    /// Holds the widening against the machine's own, bit for bit, which includes the payload of a not
    /// a number since the instruction that does this moves it the same way.
    fn check_widen(bits: u32) {
        let value = f32::from_bits(bits);
        let wanted = black_box(f64::from(black_box(value)));
        let got = widen(bits);
        assert_eq!(
            got.to_bits(),
            wanted.to_bits(),
            "{bits:08x} widened: got {:016x}, the machine says {:016x}",
            got.to_bits(),
            wanted.to_bits()
        );
    }

    fn check_narrow(bits: u64) {
        let value = f64::from_bits(bits);
        let wanted = black_box(black_box(value) as f32);
        let got = narrow(bits);
        assert_eq!(
            got.to_bits(),
            wanted.to_bits(),
            "{bits:016x} narrowed: got {:08x}, the machine says {:08x}",
            got.to_bits(),
            wanted.to_bits()
        );
    }

    #[test]
    fn widening_a_float_is_the_double_the_machine_widens_it_to() {
        let mut stream = Stream(0x1234_5678_9abc_def1);
        for _ in 0..60_000 {
            check_widen(stream.next() as u32);
            // And a subnormal, which is the one input where the fields do not simply move: it is a
            // normal double, since the wider format's range reaches far below the smallest float.
            // Random bits land on one once in every two hundred and fifty six patterns and this lands
            // on one every time.
            check_widen((stream.next() as u32) & (SIGN | FRACTION_MASK));
        }
        for value in corners() {
            check_widen(value.to_bits());
        }
    }

    /// The doubles at the ends of the narrow format's range, which is where the narrowing decides
    /// between a finite answer and an infinity and between a subnormal and a zero.
    fn narrow_edges() -> Vec<u64> {
        let mut out = Vec::new();
        for bits in [
            0x47ef_ffff_e000_0000, // the largest finite float, exactly
            0x47ef_ffff_efff_ffff, // just under halfway to 2^128, so the largest float again
            0x47ef_ffff_f000_0000, // exactly halfway, so the even neighbour wins and it is infinite
            0x4800_0000_0000_0000, // 2^128, which is past the format
            0x3810_0000_0000_0000, // 2^-126, the smallest normal float
            0x380f_ffff_ffff_ffff, // just under it, so the largest subnormal rounded up
            0x36a0_0000_0000_0000, // 2^-149, the smallest subnormal float
            0x3690_0000_0000_0000, // 2^-150, exactly half of it, so a zero by the tie to even
            0x3690_0000_0000_0001, // a hair more than half, so the smallest subnormal
            0x3698_0000_0000_0000, // three quarters of it, which rounds up for the same reason
            0x7ff0_0000_0000_0001, // a not a number whose payload is only in the bits that are lost
            0x7ff8_0000_0000_0000, // a quiet one with an empty payload
            0x7ff0_0000_0000_0000, // an infinity
            0x0000_0000_0000_0001, // the smallest subnormal double, far below the format
        ] {
            out.push(bits);
            out.push(bits | double::SIGN);
        }
        out
    }

    #[test]
    fn narrowing_a_double_is_the_float_the_machine_rounds_it_to() {
        let mut stream = Stream(0xfeed_face_1234_5671);
        for _ in 0..60_000 {
            check_narrow(stream.next());
            // Random bits are almost never inside the narrow format's range, where the rounding is,
            // so this puts the exponent from below the smallest subnormal to above the largest finite
            // and keeps the fraction and the sign random.
            let stored = 870 + stream.next() % 290;
            let rest = stream.next() & (double::SIGN | double::FRACTION_MASK);
            check_narrow(rest | (stored << double::FRACTION));
        }
        for bits in narrow_edges() {
            check_narrow(bits);
        }
    }

    /// The two directions against each other, which is the property that makes the pair worth having
    /// in one place: a float widened and narrowed again is the float it started as, every time, since
    /// the widening loses nothing for the narrowing to round.
    #[test]
    fn widening_a_float_and_narrowing_it_back_is_where_it_started() {
        let mut stream = Stream(0x0bad_c0de_0bad_c0d1);
        for _ in 0..60_000 {
            let bits = stream.next() as u32;
            let back = narrow(widen(bits).to_bits()).to_bits();
            let wanted = if is_nan(bits) { bits | QUIET } else { bits };
            assert_eq!(back, wanted, "{bits:08x} there and back came to {back:08x}");
        }
    }
}
