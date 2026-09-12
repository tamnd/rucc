//! Double precision arithmetic in integers, which is the reference for the entry points in
//! `runtime/builtins/double.c`: the four operations a target with no floating point unit calls for a
//! `double`, the negation, the eight comparisons, which are calls on such a target too, and the eight
//! conversions to and from an integer, which is what a cast becomes there.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8. The names and the conventions are libgcc's.
//!
//! # Why this is a different shape on purpose
//!
//! The same reason `float.rs` next door is, and the same two shapes: the C carries a guard bit, a
//! round bit and a sticky bit through every routine, while this takes each operand apart into an
//! exact integer and the power of two its lowest bit is worth, combines the two exactly, and rounds
//! once at the end from the exact value.
//!
//! # Where the extra width shows up
//!
//! One format up, a significand no longer leaves room in a word, and the two implementations answer
//! that differently, which is worth more than it sounds: it is the one place where the shipped code
//! and the reference are not the same idea written twice.
//!
//! The product of two fifty three bit significands is a hundred and six bits. Here that is a `u128`
//! multiplication and nothing else, because every target this crate is built for as a reference
//! multiplies sixty four by sixty four bits in instructions. The shipped C cannot assume that, so it
//! builds the product out of the four products of thirty two bit halves.
//!
//! The division is the other way round. The C runs the same bit at a time loop it runs one format
//! down, because a remainder stays below the divisor and the only thing that grows is the number of
//! times round. This cannot divide a `u128` by a `u128`, because a `/` at that width in this crate is
//! a call to `__udivti3` in `div.rs` next door and a reference that leant on another reference would
//! be worth nothing. So it divides a byte at a time, in the widest digit whose running remainder
//! still fits a word: the remainder is below the divisor, which is at most fifty three bits, so a
//! digit of eight bits keeps every division in the loop one the machine does in an instruction.
//!
//! # What it does with a not a number
//!
//! It comes back quieted, and the left one where both operands are one, which is the C's rule as
//! well. An operation with no answer at all gives the quiet not a number with an empty payload and a
//! clear sign. Neither of those two is libgcc's, which answers both out of a per architecture header
//! while one archive here serves every row of the target matrix, and `runtime/builtins/quad.c` is
//! where that is written down at length. A subtraction is the one place the sign of a not a number is
//! not a per machine choice there, and that one is matched.

/// How many bits of the significand the format writes down, the other one being implied.
pub(crate) const FRACTION: u32 = 52;

/// What is added to an exponent before it is stored.
const BIAS: i32 = 1023;

/// The stored exponent of an infinity and of a not a number.
pub(crate) const TOP: u64 = 2047;

pub(crate) const SIGN: u64 = 0x8000_0000_0000_0000;
const IMPLICIT: u64 = 0x0010_0000_0000_0000;
pub(crate) const FRACTION_MASK: u64 = 0x000f_ffff_ffff_ffff;

/// The top bit of the fraction, which is what tells a quiet not a number from a signalling one.
const QUIET: u64 = 0x0008_0000_0000_0000;

/// The not a number an operation with no answer produces.
const EMPTY_NAN: u64 = 0x7ff8_0000_0000_0000;

/// What the lowest bit of the smallest subnormal is worth, which is the lowest any answer can hold.
const SMALLEST: i32 = 1 - BIAS - FRACTION as i32;

/// How far apart two exponents have to be before the smaller operand cannot reach the answer.
///
/// Past this the smaller operand is below 2^-10 of the lowest bit the answer holds, so the answer is
/// the larger operand itself. Fifty six would do; sixty four is here because it keeps the aligned
/// pair inside a `u128` with room for the carry and because nothing is paid for the margin.
const TOO_FAR: i32 = 64;

/// A double's magnitude taken apart exactly: it is `significand * 2^scale`.
pub(crate) struct Parts {
    pub(crate) significand: u64,
    pub(crate) scale: i32,
}

/// The bits of a double as the parts the arithmetic below works on.
pub(crate) fn parts(bits: u64) -> Parts {
    let stored = (bits >> FRACTION) & TOP;
    let fraction = bits & FRACTION_MASK;
    if stored == 0 {
        Parts { significand: fraction, scale: SMALLEST }
    } else {
        Parts { significand: fraction | IMPLICIT, scale: stored as i32 - BIAS - FRACTION as i32 }
    }
}

pub(crate) fn is_nan(bits: u64) -> bool {
    (bits >> FRACTION) & TOP == TOP && bits & FRACTION_MASK != 0
}

pub(crate) fn is_infinite(bits: u64) -> bool {
    (bits >> FRACTION) & TOP == TOP && bits & FRACTION_MASK == 0
}

fn is_zero(bits: u64) -> bool {
    bits & !SIGN == 0
}

fn quiet(bits: u64) -> f64 {
    f64::from_bits(bits | QUIET)
}

fn infinity(sign: u64) -> f64 {
    f64::from_bits(sign | (TOP << FRACTION))
}

/// The double nearest `magnitude * 2^scale`, with `sign` for its sign bit.
///
/// `above` says the value is really a little more than that, by less than one unit of the magnitude's
/// lowest bit, which is what the division leaves behind. It is read where the rounding is an exact
/// tie and nowhere else.
pub(crate) fn round_from(sign: u64, magnitude: u128, scale: i32, above: bool) -> f64 {
    if magnitude == 0 {
        return f64::from_bits(sign);
    }

    // Which power of two the highest set bit is worth.
    let leading = scale + (127 - magnitude.leading_zeros() as i32);

    // Where the answer's lowest bit has to sit: fifty two below the leading one while that is inside
    // the normal range, and at the bottom of the format below it, which is what makes a subnormal
    // lose precision rather than range.
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
        return f64::from_bits(sign);
    }
    if kept < u128::from(IMPLICIT) {
        // A subnormal, whose stored exponent is zero and whose fraction is the whole significand.
        return f64::from_bits(sign | kept as u64);
    }
    let stored = lowest + FRACTION as i32 + BIAS;
    if stored >= TOP as i32 {
        return infinity(sign);
    }
    f64::from_bits(sign | ((stored as u64) << FRACTION) | (kept as u64 & FRACTION_MASK))
}

/// The sum of two doubles, which is also the difference once the caller flips a sign.
fn add(left: u64, right: u64) -> f64 {
    if is_nan(left) {
        return quiet(left);
    }
    if is_nan(right) {
        return quiet(right);
    }
    if is_infinite(left) {
        if is_infinite(right) && (left ^ right) & SIGN != 0 {
            return f64::from_bits(EMPTY_NAN);
        }
        return f64::from_bits(left);
    }
    if is_infinite(right) {
        return f64::from_bits(right);
    }
    if is_zero(left) && is_zero(right) {
        // Negative only when both of them are, which is what rounding to nearest asks for.
        return f64::from_bits(left & right & SIGN);
    }
    if is_zero(left) {
        return f64::from_bits(right);
    }
    if is_zero(right) {
        return f64::from_bits(left);
    }

    // The magnitude of a double is in the order of its bits, so the larger of the two is the one with
    // the larger pattern once the signs are off. The answer takes its sign from that one.
    let (larger, smaller) =
        if left & !SIGN >= right & !SIGN { (left, right) } else { (right, left) };
    let opposite = (left ^ right) & SIGN != 0;
    let big = parts(larger);
    let small = parts(smaller);
    let sign = larger & SIGN;

    if big.scale - small.scale > TOO_FAR {
        // The smaller operand cannot reach the lowest bit the answer holds, so the answer is the
        // larger operand and no arithmetic is needed to say so.
        return f64::from_bits(larger);
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

/// `double __adddf3(double, double)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __adddf3(left: f64, right: f64) -> f64 {
    add(left.to_bits(), right.to_bits())
}

/// The right operand of a subtraction, which is the one it was with its sign flipped, except that a
/// not a number is handed on as it came in. That one is the answer itself rather than a number being
/// negated, and soft-fp negates after it has dealt with a not a number, so flipping it would print
/// minus where GCC prints nothing.
fn negated(right: u64) -> u64 {
    if is_nan(right) { right } else { right ^ SIGN }
}

/// `double __subdf3(double, double)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __subdf3(left: f64, right: f64) -> f64 {
    add(left.to_bits(), negated(right.to_bits()))
}

/// The product of two doubles, which is exact before it is rounded: two significands of fifty three
/// bits multiply into a hundred and six, and the answer is that product rounded once.
fn multiply(left: u64, right: u64) -> f64 {
    if is_nan(left) {
        return quiet(left);
    }
    if is_nan(right) {
        return quiet(right);
    }
    let sign = (left ^ right) & SIGN;
    if is_infinite(left) || is_infinite(right) {
        if is_zero(left) || is_zero(right) {
            return f64::from_bits(EMPTY_NAN);
        }
        return infinity(sign);
    }
    if is_zero(left) || is_zero(right) {
        return f64::from_bits(sign);
    }
    let left = parts(left);
    let right = parts(right);
    let product = u128::from(left.significand) * u128::from(right.significand);
    round_from(sign, product, left.scale + right.scale, false)
}

/// `double __muldf3(double, double)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __muldf3(left: f64, right: f64) -> f64 {
    multiply(left.to_bits(), right.to_bits())
}

/// How many bits of quotient the division below makes sure of.
///
/// The rounding needs fifty three and then enough to see a tie, so fifty six would do. Seventy two is
/// what is asked for because the shift that gets it still leaves the dividend inside a `u128`
/// whatever the two significands are, and nothing is paid for the margin.
const QUOTIENT_BITS: u32 = 72;

/// A wide dividend over a `u64` divisor, a byte at a time, with whether anything was left over.
///
/// Eight bits at a time because the running remainder is below the divisor, which is at most fifty
/// three bits, so shifting it up by eight keeps it inside sixty one and every division in here is one
/// the machine does in an instruction. A `/` on a `u128` would be a call to `__udivti3` next door,
/// which is the thing this file will not do.
fn divide_wide(dividend: u128, divisor: u64) -> (u128, bool) {
    let mut quotient = 0u128;
    let mut remainder = 0u64;
    for at in (0..16).rev() {
        let digit = u64::from((dividend >> (at * 8)) as u8);
        let value = (remainder << 8) | digit;
        quotient |= u128::from(value / divisor) << (at * 8);
        remainder = value % divisor;
    }
    (quotient, remainder != 0)
}

/// The quotient of two doubles, to seventy two bits and a remainder that says whether there was
/// anything after them.
fn divide(left: u64, right: u64) -> f64 {
    if is_nan(left) {
        return quiet(left);
    }
    if is_nan(right) {
        return quiet(right);
    }
    let sign = (left ^ right) & SIGN;
    if is_infinite(left) {
        if is_infinite(right) {
            return f64::from_bits(EMPTY_NAN);
        }
        return infinity(sign);
    }
    if is_infinite(right) {
        return f64::from_bits(sign);
    }
    if is_zero(right) {
        // A zero over a zero has no answer. Anything else over a zero is an infinity, which is the
        // one division by zero IEEE 754 gives a value to.
        if is_zero(left) {
            return f64::from_bits(EMPTY_NAN);
        }
        return infinity(sign);
    }
    if is_zero(left) {
        return f64::from_bits(sign);
    }
    let left = parts(left);
    let right = parts(right);

    // How far the dividend goes up before the division. It is worked out from the two widths rather
    // than fixed, because a subnormal operand is a significand with as few as one bit in it and a
    // fixed shift would then produce a quotient with fewer bits than the answer needs.
    let up = QUOTIENT_BITS + left.significand.leading_zeros() - right.significand.leading_zeros();
    let dividend = u128::from(left.significand) << up;
    let (quotient, above) = divide_wide(dividend, right.significand);
    round_from(sign, quotient, left.scale - right.scale - up as i32, above)
}

/// `double __divdf3(double, double)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __divdf3(left: f64, right: f64) -> f64 {
    divide(left.to_bits(), right.to_bits())
}

/// `double __negdf2(double)`, which is the sign bit and nothing else, a not a number included.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __negdf2(value: f64) -> f64 {
    f64::from_bits(value.to_bits() ^ SIGN)
}

/// The comparisons, worked out from the exact parts rather than from the bit patterns.
///
/// The same split as `float.rs` one format down, and for the same reason. The shipped C reads the two
/// patterns as unsigned integers, which works because the format was designed so that two values of
/// the same sign order the way their patterns do. This does not use that property at all: it asks
/// which power of two each value's highest bit is worth, and only where those agree does it line the
/// two significands up and compare them. It is slower and pointless on a real target, which is why
/// the C ships, and it is independent of the property the C leans on, which is why it checks it.
///
/// An infinity needs no case of its own. `parts` hands back its stored exponent like anything else,
/// which puts its highest bit one power of two above the largest finite value's, so it is greater than
/// everything finite and equal to another infinity by the ordinary path.
fn order(left: u64, right: u64) -> core::cmp::Ordering {
    use core::cmp::Ordering;

    let left_parts = parts(left);
    let right_parts = parts(right);
    let left_negative = left & SIGN != 0;
    let right_negative = right & SIGN != 0;

    // A zero of either sign equals a zero of either sign, which is the one place the sign is not
    // read, and it comes first because a zero has no highest bit to ask about.
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
/// caller's own test fail, and which answer does that depends on the test, which is why there are
/// eight of these rather than one.
fn compare(left: u64, right: u64, unordered: i32) -> i32 {
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

/// `int __cmpdf2(double, double)`, which is minus one, zero or one, and one for a not a number as
/// well. The documentation says not to rely on that last part, and the compiler emits this routine
/// only where it has already ruled a not a number out.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __cmpdf2(left: f64, right: f64) -> i32 {
    compare(left.to_bits(), right.to_bits(), 1)
}

/// `int __eqdf2(double, double)`, zero when the two are equal and anything else when they are not. A
/// not a number is unequal to everything including itself.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __eqdf2(left: f64, right: f64) -> i32 {
    compare(left.to_bits(), right.to_bits(), 1)
}

/// `int __nedf2(double, double)`, which is the same work, since a caller testing for inequality tests
/// the same answer against zero the other way round. Two names because a compiler emits both.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __nedf2(left: f64, right: f64) -> i32 {
    compare(left.to_bits(), right.to_bits(), 1)
}

/// `int __gedf2(double, double)`, at or above zero when the left one is greater or equal, so a not a
/// number has to come back below zero for that test to fail.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __gedf2(left: f64, right: f64) -> i32 {
    compare(left.to_bits(), right.to_bits(), -1)
}

/// `int __gtdf2(double, double)`, above zero when the left one is greater.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __gtdf2(left: f64, right: f64) -> i32 {
    compare(left.to_bits(), right.to_bits(), -1)
}

/// `int __ledf2(double, double)`, at or below zero when the left one is less or equal.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __ledf2(left: f64, right: f64) -> i32 {
    compare(left.to_bits(), right.to_bits(), 1)
}

/// `int __ltdf2(double, double)`, below zero when the left one is less.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __ltdf2(left: f64, right: f64) -> i32 {
    compare(left.to_bits(), right.to_bits(), 1)
}

/// `int __unorddf2(double, double)`, not zero when the two cannot be ordered at all.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __unorddf2(left: f64, right: f64) -> i32 {
    i32::from(is_nan(left.to_bits()) || is_nan(right.to_bits()))
}

/// The largest magnitude each of the four integer types holds. A signed type holds one more going
/// down than it does going up, which is why the sign and the magnitude travel separately below.
const SIGNED_32: u128 = 1 << 31;
const SIGNED_64: u128 = 1 << 63;
const UNSIGNED_32: u128 = u32::MAX as u128;
const UNSIGNED_64: u128 = u64::MAX as u128;

/// The double nearest an integer, with the sign handed in separately so that the caller can take the
/// magnitude of the most negative value of its type without overflowing it.
///
/// One line, for the reason the single precision one is one line: an integer already is a significand
/// and a scale of zero, and `round_from` turns those into the nearest double. Every `int` and every
/// `unsigned int` comes out exactly, since fifty three bits hold all of them, and only a sixty four
/// bit integer can round at all.
fn double_from_integer(sign: u64, magnitude: u64) -> f64 {
    round_from(sign, u128::from(magnitude), 0, false)
}

/// `double __floatsidf(int)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatsidf(value: i32) -> f64 {
    double_from_integer(if value < 0 { SIGN } else { 0 }, u64::from(value.unsigned_abs()))
}

/// `double __floatunsidf(unsigned int)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatunsidf(value: u32) -> f64 {
    double_from_integer(0, u64::from(value))
}

/// `double __floatdidf(long long)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatdidf(value: i64) -> f64 {
    double_from_integer(if value < 0 { SIGN } else { 0 }, value.unsigned_abs())
}

/// `double __floatundidf(unsigned long long)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatundidf(value: u64) -> f64 {
    double_from_integer(0, value)
}

/// The part of a double that is on the integer side of the point, as a sign and a magnitude.
///
/// `None` where there is no integer part to hand back at all, which is an infinity, a not a number,
/// and a magnitude past what the widest of the four types holds. A value below one is not that case:
/// its integer part is zero, which is an answer.
fn integer_from_double(bits: u64) -> Option<(u64, u128)> {
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
/// `None` where the value has no answer that type can hold, which the entry points turn into the zero
/// that section 12.8 records as the shared convention for a case C leaves undefined.
fn truncate(bits: u64, bound: u128) -> Option<i128> {
    let (sign, magnitude) = integer_from_double(bits)?;
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
fn truncate_unsigned(bits: u64, bound: u128) -> Option<u128> {
    let (sign, magnitude) = integer_from_double(bits)?;
    if sign != 0 && magnitude != 0 {
        return None;
    }
    if magnitude > bound {
        return None;
    }
    Some(magnitude)
}

/// `int __fixdfsi(double)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixdfsi(value: f64) -> i32 {
    truncate(value.to_bits(), SIGNED_32).map_or(0, |answer| answer as i32)
}

/// `unsigned int __fixunsdfsi(double)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixunsdfsi(value: f64) -> u32 {
    truncate_unsigned(value.to_bits(), UNSIGNED_32).map_or(0, |answer| answer as u32)
}

/// `long long __fixdfdi(double)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixdfdi(value: f64) -> i64 {
    truncate(value.to_bits(), SIGNED_64).map_or(0, |answer| answer as i64)
}

/// `unsigned long long __fixunsdfdi(double)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixunsdfdi(value: f64) -> u64 {
    truncate_unsigned(value.to_bits(), UNSIGNED_64).map_or(0, |answer| answer as u64)
}

#[cfg(test)]
mod tests {
    use std::hint::black_box;
    use std::vec::Vec;

    use super::*;

    /// A pseudorandom stream, so the cases below are the same cases on every machine. xorshift64,
    /// the same one `float.rs` and `div.rs` use.
    struct Stream(u64);

    impl Stream {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// Any double at all, out of random bits, which is how the infinities, the not a numbers and
        /// the subnormals get in without a generator for each.
        fn any(&mut self) -> f64 {
            f64::from_bits(self.next())
        }

        /// A double whose exponent is near another one's, which is where the interesting additions
        /// are: two values far apart add to the larger and say nothing about the alignment.
        fn near(&mut self, other: f64) -> f64 {
            let bits = other.to_bits();
            let stored = (bits >> FRACTION) & TOP;
            let moved = (stored as i64 + (self.next() % 9) as i64 - 4).clamp(0, 2046) as u64;
            f64::from_bits((self.next() & (SIGN | FRACTION_MASK)) | (moved << FRACTION))
        }
    }

    /// The machine's own arithmetic, which is the answer these four routines have to give.
    ///
    /// `black_box` on the way in and out because a constant folded at compile time is the compiler's
    /// arithmetic rather than the machine's, and the two have been known to differ on a not a number.
    fn machine(which: u8, left: f64, right: f64) -> f64 {
        let left = black_box(left);
        let right = black_box(right);
        black_box(match which {
            0 => left + right,
            1 => left - right,
            2 => left * right,
            _ => left / right,
        })
    }

    fn ours(which: u8, left: f64, right: f64) -> f64 {
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

    /// Holds one case against the machine. Two not a numbers count as the same answer whatever their
    /// payloads say, for the reason `float.rs` gives: a payload is a per architecture convention and
    /// holding the payloads to each other is what `cargo xtask builtins-diff` does.
    fn check(which: u8, left: f64, right: f64) {
        let wanted = machine(which, left, right);
        let got = ours(which, left, right);
        if wanted.is_nan() && got.is_nan() {
            return;
        }
        assert_eq!(
            got.to_bits(),
            wanted.to_bits(),
            "{:016x} {} {:016x}: got {:016x}, the machine says {:016x}",
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
    fn corners() -> Vec<f64> {
        let mut out = Vec::new();
        for bits in [
            0x0000_0000_0000_0000, // zero
            0x0000_0000_0000_0001, // the smallest subnormal
            0x0000_0000_0000_0002,
            0x000f_ffff_ffff_ffff, // the largest subnormal
            0x0010_0000_0000_0000, // the smallest normal
            0x0010_0000_0000_0001,
            0x3fef_ffff_ffff_ffff, // one less than one
            0x3ff0_0000_0000_0000, // one
            0x3ff0_0000_0000_0001, // one more than one
            0x4000_0000_0000_0000, // two
            0x4330_0000_0000_0000, // 2^52, where the spacing of the doubles reaches one
            0x4340_0000_0000_0000, // 2^53, where it passes one
            0x7fef_ffff_ffff_ffff, // the largest finite
            0x7ff0_0000_0000_0000, // an infinity
            0x7ff8_0000_0000_0000, // a quiet not a number
            0x7ff0_0000_0000_0001, // a signalling one
            0x3ca0_0000_0000_0000, // 2^-53, half the spacing at one
            0x3cb0_0000_0000_0000, // 2^-52, the spacing at one
        ] {
            out.push(f64::from_bits(bits));
            out.push(f64::from_bits(bits | SIGN));
        }
        out
    }

    #[test]
    fn the_four_operations_are_the_machines_over_random_bits() {
        let mut stream = Stream(0x2545_f491_4f6c_dd1d);
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
        // One plus half the spacing at one is exactly between one and the double above it, and one is
        // the even of the two. One more than one plus the same amount is exactly between two odd
        // neighbours, and rounding up is what reaches the even one there.
        let half = f64::from_bits(0x3ca0_0000_0000_0000);
        let mut stream = Stream(0x9e37_79b9_7f4a_7c15);
        check(0, 1.0, half);
        check(0, f64::from_bits(0x3ff0_0000_0000_0001), half);
        for _ in 0..20_000 {
            // A random normal and exactly half of its own spacing, which is a tie at every exponent
            // rather than only at the one above.
            let value =
                f64::from_bits(stream.next() & 0x3fff_ffff_ffff_ffff | 0x2000_0000_0000_0000);
            let stored = (value.to_bits() >> FRACTION) & TOP;
            let step = f64::from_bits((stored - u64::from(FRACTION) - 1) << FRACTION);
            check(0, value, step);
            check(1, value, step);
        }
    }

    /// What the eight comparisons have to agree with, which is the machine's own comparison of the
    /// same two values: each routine is a sign and a test, and the test has to come out the way `<` or
    /// `==` does here, including where one operand is a not a number and every test but the last two
    /// is false.
    fn check_comparisons(left: f64, right: f64) {
        let one = black_box(left);
        let two = black_box(right);
        let bits = (left.to_bits(), right.to_bits());
        let unordered = left.is_nan() || right.is_nan();
        // The three way answer first, since it is the one routine whose whole sign is specified rather
        // than one side of zero: minus one below, one above, zero equal, and one for a not a number
        // because that is what libgcc hands back there.
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
            "cmp of {:016x} and {:016x}",
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
                "{name} of {:016x} and {:016x}: we say {ours}, the machine says {machine}",
                bits.0, bits.1
            );
        }
    }

    #[test]
    fn the_comparisons_answer_what_the_machine_answers() {
        let mut stream = Stream(0x1234_5678_9abc_def1);
        for _ in 0..60_000 {
            let left = stream.any();
            let right = stream.any();
            check_comparisons(left, right);
            check_comparisons(left, stream.near(left));
            // The same value on both sides, which is the case an implementation that compares patterns
            // and one that compares values can still differ on: a negative zero against a positive
            // one, and a not a number against itself.
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
    fn two_values_that_differ_only_in_the_fraction_order_the_way_the_machine_orders_them() {
        // The part the shipped C gets from reading patterns as integers and this gets from lining the
        // significands up: two values of one exponent, and the same pair made negative, where the
        // order reverses. Random fractions at a random exponent rather than a table, because the
        // property is about every exponent.
        let mut stream = Stream(0xfeed_face_dead_beef);
        for _ in 0..40_000 {
            let stored = 1 + stream.next() % 2046;
            let one = f64::from_bits((stored << FRACTION) | (stream.next() & FRACTION_MASK));
            let two = f64::from_bits((stored << FRACTION) | (stream.next() & FRACTION_MASK));
            check_comparisons(one, two);
            check_comparisons(-one, -two);
            check_comparisons(one, -two);
        }
    }

    #[test]
    fn the_signs_of_zero_are_the_ones_ieee_asks_for() {
        let plus = 0.0f64;
        let minus = f64::from_bits(SIGN);
        // A positive and a negative zero add to a positive one, a number less itself is positive, and
        // a product or a quotient takes the sign of the two signs multiplied.
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
        let mut stream = Stream(0x0bad_c0de_0bad_c0de);
        for _ in 0..20_000 {
            // Subnormals, and normals near the boundary, through all four operations: this is where a
            // result has fewer bits than the format usually keeps and where an implementation that
            // rounded before it shifted gets a different answer.
            let small = f64::from_bits(stream.next() & (SIGN | 0x001f_ffff_ffff_ffff));
            let other = f64::from_bits(stream.next() & (SIGN | 0x001f_ffff_ffff_ffff));
            let large = f64::from_bits(stream.next() | 0x7000_0000_0000_0000);
            for which in 0..4 {
                check(which, small, other);
                check(which, small, large);
                check(which, large, small);
            }
        }
    }

    #[test]
    fn the_top_of_the_range_rounds_to_an_infinity() {
        let largest = f64::from_bits(0x7fef_ffff_ffff_ffff);
        let half_step = f64::from_bits(0x7c90_0000_0000_0000);
        check(0, largest, largest);
        check(0, largest, half_step);
        check(2, largest, 2.0);
        check(3, largest, 0.5);
        check(1, -largest, largest);
    }

    /// Holds one integer against the double the machine makes of it, which for a value with more than
    /// fifty three significant bits is a rounding and not a widening.
    fn check_up_signed(value: i64) {
        let wanted = black_box(black_box(value) as f64);
        let sign = if value < 0 { SIGN } else { 0 };
        let got = double_from_integer(sign, value.unsigned_abs());
        assert_eq!(
            got.to_bits(),
            wanted.to_bits(),
            "{value} as a double: got {:016x}, the machine says {:016x}",
            got.to_bits(),
            wanted.to_bits()
        );
    }

    fn check_up_unsigned(value: u64) {
        let wanted = black_box(black_box(value) as f64);
        let got = double_from_integer(0, value);
        assert_eq!(
            got.to_bits(),
            wanted.to_bits(),
            "{value} as a double: got {:016x}, the machine says {:016x}",
            got.to_bits(),
            wanted.to_bits()
        );
    }

    /// Holds one double against the integers the machine truncates it to, in all four types.
    ///
    /// Rust's own conversion saturates and hands back zero for a not a number, where C leaves all
    /// three of those undefined and these routines answer zero. So the machine is the oracle inside
    /// the range, and outside it what is checked is the convention both implementations keep.
    fn check_down(value: f64) {
        let bits = value.to_bits();
        let ours = (
            truncate(bits, SIGNED_32).map_or(0, |answer| answer as i32),
            truncate(bits, SIGNED_64).map_or(0, |answer| answer as i64),
            truncate_unsigned(bits, UNSIGNED_32).map_or(0, |answer| answer as u32),
            truncate_unsigned(bits, UNSIGNED_64).map_or(0, |answer| answer as u64),
        );
        if value.is_nan() {
            assert_eq!(ours, (0, 0, 0, 0), "{bits:016x} is a not a number");
            return;
        }
        // The integer part, and then the four bounds as doubles. Each bound is a power of two and so
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
        assert_eq!(ours, wanted, "{bits:016x} as integers, which truncates to {whole}");
    }

    #[test]
    fn an_integer_becomes_the_double_the_machine_makes_of_it() {
        let mut stream = Stream(0x5dee_ce66_d000_0005);
        for _ in 0..60_000 {
            let wide = stream.next();
            // Each value at both widths, so that one that fits in thirty two bits is asked of the
            // thirty two bit routine and of the sixty four bit one as well. The thirty two bit pair is
            // exact at this width, which is a case worth running rather than one worth skipping: it is
            // where a shift that went the wrong way would show up without any rounding on top of it.
            check_up_signed(i64::from(wide as i32));
            check_up_signed(wide as i64);
            check_up_unsigned(u64::from(wide as u32));
            check_up_unsigned(wide);
            // And a value just past 2^53, which is where a double stops holding every integer and
            // where the rounding starts to matter. Random bits land far above it nearly always and
            // these land just above it, which is a different case.
            let near = (wide % (1 << 59)) + (1 << 53);
            check_up_signed(near as i64);
            check_up_unsigned(near);
        }
        for value in [
            0i64,
            1,
            -1,
            (1 << 53) - 1,
            1 << 53,
            (1 << 53) + 1, // the first integer a double cannot hold, which rounds down to 2^53
            (1 << 53) + 2,
            (1 << 53) + 3, // exactly between two doubles, so the even one wins
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
    fn a_double_becomes_the_integer_it_truncates_to_where_the_type_holds_one() {
        let mut stream = Stream(0xdead_beef_cafe_0001);
        for _ in 0..60_000 {
            check_down(stream.any());
            // Random bits are almost never a value with an integer part inside any of the four
            // ranges, so most of those cases would be the out of range convention and would say
            // nothing about the shift. This puts the exponent where the shift happens: from 2^-27,
            // which truncates to zero, to 2^67, which is past all four types.
            let stored = 996 + stream.next() % 95;
            let fraction = stream.next() & (SIGN | FRACTION_MASK);
            check_down(f64::from_bits(fraction | (stored << FRACTION)));
        }
        for value in corners() {
            check_down(value);
        }
        for value in [
            0.5f64,
            -0.5, // a negative value whose integer part is zero, which is defined for unsigned too
            1.5,
            -1.5,
            2147483648.0,  // one past the largest int, and exactly the most negative one
            -2147483648.0, // the most negative int, which is the case a negation would overflow
            4294967296.0,
            9223372036854775808.0,
            -9223372036854775808.0,
            18446744073709551616.0,
            // The two values either side of the largest `long long`, which a double cannot hold and
            // which is therefore the pair that decides whether the bound is tested against the value
            // the cast would produce or against the value asked for.
            9223372036854774784.0,
            9223372036854777856.0,
            1e300,
            -1e300,
        ] {
            check_down(value);
        }
    }

    #[test]
    fn the_wide_division_is_the_one_a_native_divide_would_give() {
        // The byte at a time loop against the machine's own division, at a width the machine does in
        // one instruction, which is the part of it that can be checked directly rather than through
        // the answers it feeds.
        let mut stream = Stream(0x1234_5678_9abc_def1);
        for _ in 0..60_000 {
            let dividend = stream.next();
            // A divisor inside the fifty three bits the loop is built for, never zero.
            let divisor = (stream.next() & FRACTION_MASK | IMPLICIT).max(1);
            let (quotient, above) = divide_wide(u128::from(dividend), divisor);
            assert_eq!(quotient, u128::from(dividend / divisor), "{dividend} over {divisor}");
            assert_eq!(above, dividend % divisor != 0, "{dividend} over {divisor}");
        }
        // And a dividend that needs the whole width, where the answer cannot be checked against a
        // native division: the quotient times the divisor plus what is left has to be the dividend.
        for _ in 0..20_000 {
            let dividend = u128::from(stream.next()) << 60 | u128::from(stream.next() >> 4);
            let divisor = (stream.next() & FRACTION_MASK | IMPLICIT).max(1);
            let (quotient, above) = divide_wide(dividend, divisor);
            let back = quotient * u128::from(divisor);
            assert!(back <= dividend, "{dividend} over {divisor} came out too large");
            assert!(dividend - back < u128::from(divisor), "{dividend} over {divisor} is short");
            assert_eq!(above, dividend != back, "{dividend} over {divisor}");
        }
    }
}
