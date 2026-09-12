//! Quad precision arithmetic in integers, which is the reference for the entry points in
//! `runtime/builtins/quad.c`: the four operations on a `_Float128`, the negation and the eight
//! comparisons. Every one of those is a call on every target rather than only on a target without a
//! floating point unit, because no machine anyone compiles for has an instruction at this format.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8 and `spec/cross-compile/10-runtime.md` section
//! 10.2. The names and the conventions are libgcc's.
//!
//! # Why this is a different shape on purpose
//!
//! The same reason `float.rs` and `double.rs` are, and the same two shapes: the C carries a guard
//! bit, a round bit and a sticky bit through every routine, while this takes each operand apart into
//! an exact integer and the power of two its lowest bit is worth, combines the two exactly, and
//! rounds once at the end from the exact value.
//!
//! # Where the extra width shows up
//!
//! A significand is a hundred and thirteen bits at this format, so an exact product is two hundred
//! and twenty six of them and the widest integer the language has is not wide enough. `Wide` below is
//! the answer, two `u128` halves with the shifts, the comparison, the add and the subtract written
//! out. That is as much arithmetic by hand as the shipped C does, which is the cost of the format
//! rather than of either implementation.
//!
//! The multiplication is four products of sixty four bit halves, the way `double.c` builds its
//! product out of thirty two bit ones, because a `u128` multiplied by a `u128` is a call to
//! `__multi3` and a reference that leant on another runtime would be worth nothing. The division is
//! the one routine here that is the same idea as the shipped C's, a shift and a comparison and a
//! subtraction a bit at a time. `double.rs` divides a byte at a time because its divisor fits a word
//! and `value / divisor` is then an instruction the machine has, and here the divisor is a hundred
//! and thirteen bits and a `/` on a `u128` is a call to `__udivti3` in `div.rs` next door.
//!
//! # The type the entry points take
//!
//! `f128` is unstable in the Rust this crate is built with, so there is no primitive to write in a
//! signature. What is written instead is a sixteen byte vector, which the psABI passes in exactly the
//! register a `_Float128` is passed in: one SSE register on x86-64 and one `v` register on AArch64,
//! which are the two architectures `cargo xtask builtins-diff` holds this against the C on. Nothing
//! here does arithmetic on that type, only a move into the `u128` the routines work on, so the choice
//! of vector says nothing beyond where the bits arrive.
//!
//! # What it does with a not a number
//!
//! It comes back quieted, and the left one where both operands are one. An operation with no answer
//! at all gives the quiet not a number with an empty payload and a clear sign. Both of those are this
//! library's own convention rather than libgcc's, which answers them out of a per architecture header
//! while one archive here serves every row of the target matrix, and `runtime/builtins/quad.c` says
//! so at more length. A subtraction hands back the not a number that came in rather than one with its
//! sign flipped, and that one is not a per machine choice in libgcc, so it is matched here.

// rustc calls a vector type in an `extern "C"` signature not FFI-safe because the type has no layout
// the language promises. That warning is about reading such a value as a C structure, and nothing here
// does: the only thing done with one is a move of sixteen bytes, and where those bytes arrive is the
// one property the paragraph above relies on.
#![allow(improper_ctypes_definitions)]

/// How many bits of the significand the format writes down, the other one being implied.
pub(crate) const FRACTION: u32 = 112;

/// What is added to an exponent before it is stored.
const BIAS: i32 = 16383;

/// The stored exponent of an infinity and of a not a number.
pub(crate) const TOP: u128 = 32767;

pub(crate) const SIGN: u128 = 1 << 127;
const IMPLICIT: u128 = 1 << FRACTION;
pub(crate) const FRACTION_MASK: u128 = IMPLICIT - 1;

/// The top bit of the fraction, which is what tells a quiet not a number from a signalling one.
const QUIET: u128 = 1 << (FRACTION - 1);

/// The not a number an operation with no answer produces.
const EMPTY_NAN: u128 = (TOP << FRACTION) | QUIET;

/// What the lowest bit of the smallest subnormal is worth, which is the lowest any answer can hold.
const SMALLEST: i32 = 1 - BIAS - FRACTION as i32;

/// How far apart two exponents have to be before the smaller operand cannot reach the answer.
///
/// Past this the smaller operand is below 2^-15 of the lowest bit the answer holds, so the answer is
/// the larger operand itself. A hundred and sixteen would do; a hundred and twenty eight is here
/// because it keeps the aligned pair inside a `Wide` with room for the carry and because nothing is
/// paid for the margin.
const TOO_FAR: i32 = 128;

/// Two hundred and fifty six bits, which is what an exact product at this format needs and what the
/// language does not have.
///
/// Only what the arithmetic below asks for is written: the shifts, the comparison, the add, the
/// subtract and how many bits a value takes to write down. Deriving the comparison is the whole of
/// it, since a pair of halves in this order compares the way the number does.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct Wide {
    high: u128,
    low: u128,
}

impl Wide {
    const ZERO: Wide = Wide { high: 0, low: 0 };

    /// A value that fits in a half, which is every operand before anything is combined.
    const fn narrow(value: u128) -> Wide {
        Wide { high: 0, low: value }
    }

    const fn is_zero(self) -> bool {
        self.high == 0 && self.low == 0
    }

    /// How many bits it takes to write down, which is one more than the power of two of its highest
    /// set bit and zero for a zero.
    const fn bits(self) -> u32 {
        if self.high != 0 {
            256 - self.high.leading_zeros()
        } else {
            128 - self.low.leading_zeros()
        }
    }

    fn shifted_up(self, up: u32) -> Wide {
        if up == 0 {
            self
        } else if up >= 256 {
            Wide::ZERO
        } else if up >= 128 {
            Wide { high: self.low << (up - 128), low: 0 }
        } else {
            Wide { high: (self.high << up) | (self.low >> (128 - up)), low: self.low << up }
        }
    }

    fn shifted_down(self, down: u32) -> Wide {
        if down == 0 {
            self
        } else if down >= 256 {
            Wide::ZERO
        } else if down >= 128 {
            Wide { high: 0, low: self.high >> (down - 128) }
        } else {
            Wide { high: self.high >> down, low: (self.low >> down) | (self.high << (128 - down)) }
        }
    }

    /// The lowest `count` bits of it and nothing above them, which is what the rounding weighs.
    fn low_bits(self, count: u32) -> Wide {
        if count == 0 {
            Wide::ZERO
        } else if count >= 256 {
            self
        } else if count >= 128 {
            Wide { high: self.high & ((1 << (count - 128)) - 1), low: self.low }
        } else {
            Wide { high: 0, low: self.low & ((1 << count) - 1) }
        }
    }

    fn plus(self, other: Wide) -> Wide {
        let (low, carried) = self.low.overflowing_add(other.low);
        let high = self.high.wrapping_add(other.high).wrapping_add(u128::from(carried));
        Wide { high, low }
    }

    fn minus(self, other: Wide) -> Wide {
        let (low, borrowed) = self.low.overflowing_sub(other.low);
        let high = self.high.wrapping_sub(other.high).wrapping_sub(u128::from(borrowed));
        Wide { high, low }
    }
}

/// The product of two numbers of up to a hundred and thirteen bits, which is up to two hundred and
/// twenty six and so is the reason `Wide` is here at all.
///
/// Four products of sixty four bit halves, which every machine this is built for does in one
/// instruction, with the two middle ones added in across the boundary between the halves.
fn wide_product(left: u128, right: u128) -> Wide {
    let left_low = left as u64;
    let left_high = (left >> 64) as u64;
    let right_low = right as u64;
    let right_high = (right >> 64) as u64;

    let low = u128::from(left_low) * u128::from(right_low);
    let high = u128::from(left_high) * u128::from(right_high);
    let first = u128::from(left_low) * u128::from(right_high);
    let second = u128::from(left_high) * u128::from(right_low);

    let out = Wide { high, low };
    let out = out.plus(Wide { high: first >> 64, low: first << 64 });
    out.plus(Wide { high: second >> 64, low: second << 64 })
}

/// A quad's magnitude taken apart exactly: it is `significand * 2^scale`.
pub(crate) struct Parts {
    pub(crate) significand: u128,
    pub(crate) scale: i32,
}

/// The bits of a quad as the parts the arithmetic below works on.
pub(crate) fn parts(bits: u128) -> Parts {
    let stored = (bits >> FRACTION) & TOP;
    let fraction = bits & FRACTION_MASK;
    if stored == 0 {
        Parts { significand: fraction, scale: SMALLEST }
    } else {
        Parts { significand: fraction | IMPLICIT, scale: stored as i32 - BIAS - FRACTION as i32 }
    }
}

pub(crate) fn is_nan(bits: u128) -> bool {
    (bits >> FRACTION) & TOP == TOP && bits & FRACTION_MASK != 0
}

pub(crate) fn is_infinite(bits: u128) -> bool {
    (bits >> FRACTION) & TOP == TOP && bits & FRACTION_MASK == 0
}

fn is_zero(bits: u128) -> bool {
    bits & !SIGN == 0
}

fn quiet(bits: u128) -> u128 {
    bits | QUIET
}

fn infinity(sign: u128) -> u128 {
    sign | (TOP << FRACTION)
}

/// The quad nearest `magnitude * 2^scale`, with `sign` for its sign bit.
///
/// `above` says the value is really a little more than that, by less than one unit of the magnitude's
/// lowest bit, which is what the division leaves behind. It is read where the rounding is an exact
/// tie and nowhere else.
pub(crate) fn round_from(sign: u128, magnitude: Wide, scale: i32, above: bool) -> u128 {
    if magnitude.is_zero() {
        return sign;
    }

    // Which power of two the highest set bit is worth.
    let leading = scale + magnitude.bits() as i32 - 1;

    // Where the answer's lowest bit has to sit: a hundred and twelve below the leading one while that
    // is inside the normal range, and at the bottom of the format below it, which is what makes a
    // subnormal lose precision rather than range.
    let mut lowest = if leading >= 1 - BIAS { leading - FRACTION as i32 } else { SMALLEST };

    let drop = lowest - scale;
    let mut kept = if drop <= 0 {
        magnitude.shifted_up((-drop) as u32)
    } else if drop >= 256 {
        // Everything is below the answer's lowest bit, so what is left is the rounding decision
        // against zero, which only ever goes down this far out.
        Wide::ZERO
    } else {
        let below = magnitude.low_bits(drop as u32);
        let tie = Wide::narrow(1).shifted_up(drop as u32 - 1);
        let mut kept = magnitude.shifted_down(drop as u32);
        if below > tie || (below == tie && (above || kept.low & 1 == 1)) {
            kept = kept.plus(Wide::narrow(1));
        }
        kept
    };
    if kept.bits() > FRACTION + 1 {
        // The rounding carried out of the top, which can only have made it a power of two.
        kept = kept.shifted_down(1);
        lowest += 1;
    }

    if kept.is_zero() {
        return sign;
    }
    if kept.low < IMPLICIT {
        // A subnormal, whose stored exponent is zero and whose fraction is the whole significand.
        return sign | kept.low;
    }
    let stored = lowest + FRACTION as i32 + BIAS;
    if stored >= TOP as i32 {
        return infinity(sign);
    }
    sign | ((stored as u128) << FRACTION) | (kept.low & FRACTION_MASK)
}

/// The sum of two quads, which is also the difference once the caller flips a sign.
fn add(left: u128, right: u128) -> u128 {
    if is_nan(left) {
        return quiet(left);
    }
    if is_nan(right) {
        return quiet(right);
    }
    if is_infinite(left) {
        if is_infinite(right) && (left ^ right) & SIGN != 0 {
            return EMPTY_NAN;
        }
        return left;
    }
    if is_infinite(right) {
        return right;
    }
    if is_zero(left) && is_zero(right) {
        // Negative only when both of them are, which is what rounding to nearest asks for.
        return left & right & SIGN;
    }
    if is_zero(left) {
        return right;
    }
    if is_zero(right) {
        return left;
    }

    // The magnitude of a quad is in the order of its bits, so the larger of the two is the one with
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
        return larger;
    }

    let aligned = Wide::narrow(big.significand).shifted_up((big.scale - small.scale) as u32);
    let other = Wide::narrow(small.significand);
    if opposite {
        if aligned == other {
            // A number less itself, which is a positive zero in this rounding mode.
            return 0;
        }
        round_from(sign, aligned.minus(other), small.scale, false)
    } else {
        round_from(sign, aligned.plus(other), small.scale, false)
    }
}

/// The right operand of a subtraction, which is the one it was with its sign flipped, except that a
/// not a number is handed on as it came in, for the reason `double.rs` gives.
fn negated(right: u128) -> u128 {
    if is_nan(right) { right } else { right ^ SIGN }
}

/// The product of two quads, which is exact before it is rounded: two significands of a hundred and
/// thirteen bits multiply into two hundred and twenty six, and the answer is that product rounded
/// once.
fn multiply(left: u128, right: u128) -> u128 {
    if is_nan(left) {
        return quiet(left);
    }
    if is_nan(right) {
        return quiet(right);
    }
    let sign = (left ^ right) & SIGN;
    if is_infinite(left) || is_infinite(right) {
        if is_zero(left) || is_zero(right) {
            return EMPTY_NAN;
        }
        return infinity(sign);
    }
    if is_zero(left) || is_zero(right) {
        return sign;
    }
    let left = parts(left);
    let right = parts(right);
    let product = wide_product(left.significand, right.significand);
    round_from(sign, product, left.scale + right.scale, false)
}

/// How many bits of quotient the division below makes sure of.
///
/// The rounding needs a hundred and thirteen and then enough to see a tie, so a hundred and sixteen
/// would do. A hundred and twenty eight is what is asked for because the shift that gets it still
/// leaves the dividend inside a `Wide` whatever the two significands are, and nothing is paid for the
/// margin.
const QUOTIENT_BITS: u32 = 128;

/// A wide dividend over a divisor of up to a hundred and thirteen bits, with whether anything was
/// left over.
///
/// A bit at a time, which is the one place this file and the shipped C are the same idea. One format
/// down the reference divides a byte at a time, because the running remainder stays below a fifty
/// three bit divisor and `value / divisor` is then a single instruction. Here the divisor does not fit
/// a word and a `/` at this width is a call to `__udivti3` in `div.rs` next door, so what is left is
/// a shift, a comparison and a subtraction.
fn divide_wide(dividend: Wide, divisor: u128) -> (Wide, bool) {
    let divisor = Wide::narrow(divisor);
    let mut quotient = Wide::ZERO;
    let mut remainder = Wide::ZERO;
    for at in (0..dividend.bits()).rev() {
        let bit = dividend.shifted_down(at).low & 1;
        remainder = remainder.shifted_up(1).plus(Wide::narrow(bit));
        quotient = quotient.shifted_up(1);
        if remainder >= divisor {
            remainder = remainder.minus(divisor);
            quotient = quotient.plus(Wide::narrow(1));
        }
    }
    (quotient, !remainder.is_zero())
}

/// The quotient of two quads, to a hundred and twenty eight bits and a remainder that says whether
/// there was anything after them.
fn divide(left: u128, right: u128) -> u128 {
    if is_nan(left) {
        return quiet(left);
    }
    if is_nan(right) {
        return quiet(right);
    }
    let sign = (left ^ right) & SIGN;
    if is_infinite(left) {
        if is_infinite(right) {
            return EMPTY_NAN;
        }
        return infinity(sign);
    }
    if is_infinite(right) {
        return sign;
    }
    if is_zero(right) {
        // A zero over a zero has no answer. Anything else over a zero is an infinity, which is the
        // one division by zero IEEE 754 gives a value to.
        if is_zero(left) {
            return EMPTY_NAN;
        }
        return infinity(sign);
    }
    if is_zero(left) {
        return sign;
    }
    let left = parts(left);
    let right = parts(right);

    // How far the dividend goes up before the division. It is worked out from the two widths rather
    // than fixed, because a subnormal operand is a significand with as few as one bit in it and a
    // fixed shift would then produce a quotient with fewer bits than the answer needs.
    let up = QUOTIENT_BITS + left.significand.leading_zeros() - right.significand.leading_zeros();
    let dividend = Wide::narrow(left.significand).shifted_up(up);
    let (quotient, above) = divide_wide(dividend, right.significand);
    round_from(sign, quotient, left.scale - right.scale - up as i32, above)
}

/// The comparisons, worked out from the exact parts rather than from the bit patterns, the way
/// `double.rs` does them and for the reason it gives: the shipped C reads the two patterns as
/// integers, and this does not use that property at all, so it checks it.
fn order(left: u128, right: u128) -> core::cmp::Ordering {
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
    let left_top = left_parts.scale + (127 - left_parts.significand.leading_zeros() as i32);
    let right_top = right_parts.scale + (127 - right_parts.significand.leading_zeros() as i32);
    let magnitudes = left_top.cmp(&right_top).then_with(|| {
        let left_lined = left_parts.significand << left_parts.significand.leading_zeros();
        let right_lined = right_parts.significand << right_parts.significand.leading_zeros();
        left_lined.cmp(&right_lined)
    });
    if left_negative { magnitudes.reverse() } else { magnitudes }
}

/// What the eight entry points share: the comparison, and the one number that differs between them,
/// which is what to answer when an operand is a not a number. That answer has to make the caller's
/// own test fail, and which answer does that depends on the test.
fn compare(left: u128, right: u128, unordered: i32) -> i32 {
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

/// The type a `_Float128` parameter and return value is written as, for the reason the module
/// documentation gives. On the two architectures the differential test runs on it is the vector the
/// psABI puts one in, and anywhere else it is a sixteen byte aggregate, which is how a target with no
/// register for one passes it.
#[cfg(target_arch = "x86_64")]
pub type Quad = core::arch::x86_64::__m128i;

/// The same thing on AArch64, where a quad arrives in a `v` register.
#[cfg(target_arch = "aarch64")]
pub type Quad = core::arch::aarch64::uint64x2_t;

/// The same thing anywhere else.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct Quad([u64; 2]);

/// The bits of a quad, which is all the parameter ever carried.
#[cfg(not(test))]
#[inline]
fn bits_of(value: Quad) -> u128 {
    // SAFETY: sixteen bytes read as the integer they are. Both types are sixteen bytes wide and
    // neither has a bit pattern the other refuses, so every input is a value of the output type and
    // this is a move and nothing more.
    unsafe { core::mem::transmute(value) }
}

/// A quad made of bits, which is the same move the other way.
#[cfg(not(test))]
#[inline]
fn quad_of(bits: u128) -> Quad {
    // SAFETY: the same two types and the same widths as above, in the other direction.
    unsafe { core::mem::transmute(bits) }
}

/// `_Float128 __addtf3(_Float128, _Float128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __addtf3(left: Quad, right: Quad) -> Quad {
    quad_of(add(bits_of(left), bits_of(right)))
}

/// `_Float128 __subtf3(_Float128, _Float128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __subtf3(left: Quad, right: Quad) -> Quad {
    quad_of(add(bits_of(left), negated(bits_of(right))))
}

/// `_Float128 __multf3(_Float128, _Float128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __multf3(left: Quad, right: Quad) -> Quad {
    quad_of(multiply(bits_of(left), bits_of(right)))
}

/// `_Float128 __divtf3(_Float128, _Float128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __divtf3(left: Quad, right: Quad) -> Quad {
    quad_of(divide(bits_of(left), bits_of(right)))
}

/// `_Float128 __negtf2(_Float128)`, which is the sign bit and nothing else, a not a number included.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __negtf2(value: Quad) -> Quad {
    quad_of(bits_of(value) ^ SIGN)
}

/// `int __cmptf2(_Float128, _Float128)`, which is minus one, zero or one, and one for a not a number
/// as well. The documentation says not to rely on that last part, and the compiler emits this routine
/// only where it has already ruled a not a number out.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __cmptf2(left: Quad, right: Quad) -> i32 {
    compare(bits_of(left), bits_of(right), 1)
}

/// `int __eqtf2(_Float128, _Float128)`, zero when the two are equal and anything else when they are
/// not. A not a number is unequal to everything including itself.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __eqtf2(left: Quad, right: Quad) -> i32 {
    compare(bits_of(left), bits_of(right), 1)
}

/// `int __netf2(_Float128, _Float128)`, which is the same work, since a caller testing for inequality
/// tests the same answer against zero the other way round. Two names because a compiler emits both.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __netf2(left: Quad, right: Quad) -> i32 {
    compare(bits_of(left), bits_of(right), 1)
}

/// `int __getf2(_Float128, _Float128)`, at or above zero when the left one is greater or equal, so a
/// not a number has to come back below zero for that test to fail.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __getf2(left: Quad, right: Quad) -> i32 {
    compare(bits_of(left), bits_of(right), -1)
}

/// `int __gttf2(_Float128, _Float128)`, above zero when the left one is greater.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __gttf2(left: Quad, right: Quad) -> i32 {
    compare(bits_of(left), bits_of(right), -1)
}

/// `int __letf2(_Float128, _Float128)`, at or below zero when the left one is less or equal.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __letf2(left: Quad, right: Quad) -> i32 {
    compare(bits_of(left), bits_of(right), 1)
}

/// `int __lttf2(_Float128, _Float128)`, below zero when the left one is less.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __lttf2(left: Quad, right: Quad) -> i32 {
    compare(bits_of(left), bits_of(right), 1)
}

/// `int __unordtf2(_Float128, _Float128)`, not zero when the two cannot be ordered at all.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __unordtf2(left: Quad, right: Quad) -> i32 {
    i32::from(is_nan(bits_of(left)) || is_nan(bits_of(right)))
}

/// The largest magnitude each of the four integer types holds. A signed type holds one more going
/// down than it does going up, which is why the sign and the magnitude travel separately below.
const SIGNED_32: u128 = 1 << 31;
const SIGNED_64: u128 = 1 << 63;
const UNSIGNED_32: u128 = u32::MAX as u128;
const UNSIGNED_64: u128 = u64::MAX as u128;

/// The quad nearest an integer, with the sign handed in separately so that the caller can take the
/// magnitude of the most negative value of its type without overflowing it.
///
/// One line, for the reason the two narrower files give: an integer already is a significand and a
/// scale of zero, and `round_from` turns those into the nearest quad. What is different here is that
/// the rounding never does anything, since a hundred and thirteen bits hold every sixty four bit
/// integer exactly, so all four of these are exact where two of the four one format down are not.
fn quad_from_integer(sign: u128, magnitude: u64) -> u128 {
    round_from(sign, Wide::narrow(u128::from(magnitude)), 0, false)
}

/// The part of a quad that is on the integer side of the point, as a sign and a magnitude.
///
/// `None` where there is no integer part to hand back at all, which is an infinity, a not a number,
/// and a magnitude past what the widest of the four types holds. A value below one is not that case:
/// its integer part is zero, which is an answer.
fn integer_from_quad(bits: u128) -> Option<(u128, u128)> {
    if (bits >> FRACTION) & TOP == TOP {
        return None;
    }
    let sign = bits & SIGN;
    let parts = parts(bits);
    if parts.significand == 0 {
        return Some((sign, 0));
    }
    // Which power of two the highest bit of the value is worth. Nothing above sixty three has an
    // answer in any of the four types, and refusing here rather than after the shift is also what
    // keeps the shift inside a `u128`, since a significand is a hundred and thirteen bits wide and a
    // scale can take it far past the top of one.
    let leading = parts.scale + parts.significand.ilog2() as i32;
    if leading > 63 {
        return None;
    }
    if parts.scale >= 0 {
        Some((sign, parts.significand << parts.scale))
    } else if -parts.scale >= 128 {
        Some((sign, 0))
    } else {
        Some((sign, parts.significand >> -parts.scale))
    }
}

/// What the two signed routines share: the integer part held to the bounds of the caller's type.
/// `bound` is the largest magnitude that type holds going down, and one more than the largest it
/// holds going up.
///
/// `None` where the value has no answer that type can hold, which the entry points turn into the zero
/// that section 12.8 records as the shared convention for a case C leaves undefined.
fn truncate(bits: u128, bound: u128) -> Option<i128> {
    let (sign, magnitude) = integer_from_quad(bits)?;
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
fn truncate_unsigned(bits: u128, bound: u128) -> Option<u128> {
    let (sign, magnitude) = integer_from_quad(bits)?;
    if sign != 0 && magnitude != 0 {
        return None;
    }
    if magnitude > bound {
        return None;
    }
    Some(magnitude)
}

/// `_Float128 __floatsitf(int)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatsitf(value: i32) -> Quad {
    quad_of(quad_from_integer(if value < 0 { SIGN } else { 0 }, u64::from(value.unsigned_abs())))
}

/// `_Float128 __floatunsitf(unsigned int)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatunsitf(value: u32) -> Quad {
    quad_of(quad_from_integer(0, u64::from(value)))
}

/// `_Float128 __floatditf(long long)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatditf(value: i64) -> Quad {
    quad_of(quad_from_integer(if value < 0 { SIGN } else { 0 }, value.unsigned_abs()))
}

/// `_Float128 __floatunditf(unsigned long long)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatunditf(value: u64) -> Quad {
    quad_of(quad_from_integer(0, value))
}

/// `int __fixtfsi(_Float128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixtfsi(value: Quad) -> i32 {
    truncate(bits_of(value), SIGNED_32).map_or(0, |answer| answer as i32)
}

/// `unsigned int __fixunstfsi(_Float128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixunstfsi(value: Quad) -> u32 {
    truncate_unsigned(bits_of(value), UNSIGNED_32).map_or(0, |answer| answer as u32)
}

/// `long long __fixtfdi(_Float128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixtfdi(value: Quad) -> i64 {
    truncate(bits_of(value), SIGNED_64).map_or(0, |answer| answer as i64)
}

/// `unsigned long long __fixunstfdi(_Float128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixunstfdi(value: Quad) -> u64 {
    truncate_unsigned(bits_of(value), UNSIGNED_64).map_or(0, |answer| answer as u64)
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;

    /// A pseudorandom stream, so the cases below are the same cases on every machine. xorshift64,
    /// the same one the other files here use.
    struct Stream(u64);

    impl Stream {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// Any quad at all, out of random bits, which is how the infinities, the not a numbers and
        /// the subnormals get in without a generator for each.
        fn any(&mut self) -> u128 {
            (u128::from(self.next()) << 64) | u128::from(self.next())
        }

        /// A quad with a random sign and fraction at the exponent asked for, which is how a case
        /// lands in the middle of the range rather than wherever random bits put it.
        fn at(&mut self, stored: u128) -> u128 {
            (self.any() & (SIGN | FRACTION_MASK)) | (stored << FRACTION)
        }

        /// A quad whose exponent is within four of another's, which is where the interesting
        /// additions are: two values far apart add to the larger and say nothing about the alignment.
        fn near(&mut self, other: u128) -> u128 {
            let stored = (other >> FRACTION) & TOP;
            let moved = (stored as i64 + (self.next() % 9) as i64 - 4).clamp(0, TOP as i64 - 1);
            self.at(moved as u128)
        }
    }

    fn ours(which: u8, left: u128, right: u128) -> u128 {
        match which {
            0 => add(left, right),
            1 => add(left, negated(right)),
            2 => multiply(left, right),
            _ => divide(left, right),
        }
    }

    fn name(which: u8) -> &'static str {
        match which {
            0 => "+",
            1 => "-",
            2 => "*",
            _ => "/",
        }
    }

    /// The answers libgcc gives for thirty pairs through all four operations, which is the one oracle
    /// there is for this format inside `cargo test`: there is no `f128` in this Rust to compare
    /// against and no machine instruction under it either.
    ///
    /// Ten pairs by name and twenty by luck, each row a left pattern, a right pattern, which of the
    /// four operations it is and the answer. The named ones are one and three, a value and half the
    /// spacing of its own exponent either side of a tie, both ends of the range, both ends of the
    /// subnormals, and a pair with opposite signs. `cargo xtask builtins-diff` is what holds the
    /// whole hazard list to the shipped C; this is what holds the file to something outside the
    /// workspace on a machine that cannot run that.
    #[rustfmt::skip]
    const LIBGCC: &[(u128, u128, u8, u128)] = &[
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x4000_8000_0000_0000_0000_0000_0000_0000, 0, 0x4001_0000_0000_0000_0000_0000_0000_0000),
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x4000_8000_0000_0000_0000_0000_0000_0000, 1, 0xc000_0000_0000_0000_0000_0000_0000_0000),
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x4000_8000_0000_0000_0000_0000_0000_0000, 2, 0x4000_8000_0000_0000_0000_0000_0000_0000),
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x4000_8000_0000_0000_0000_0000_0000_0000, 3, 0x3ffd_5555_5555_5555_5555_5555_5555_5555),
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x3f8e_0000_0000_0000_0000_0000_0000_0000, 0, 0x3fff_0000_0000_0000_0000_0000_0000_0000),
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x3f8e_0000_0000_0000_0000_0000_0000_0000, 1, 0x3ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff),
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x3f8e_0000_0000_0000_0000_0000_0000_0000, 2, 0x3f8e_0000_0000_0000_0000_0000_0000_0000),
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x3f8e_0000_0000_0000_0000_0000_0000_0000, 3, 0x4070_0000_0000_0000_0000_0000_0000_0000),
        (0x3fff_0000_0000_0000_0000_0000_0000_0001, 0x3f8e_0000_0000_0000_0000_0000_0000_0000, 0, 0x3fff_0000_0000_0000_0000_0000_0000_0002),
        (0x3fff_0000_0000_0000_0000_0000_0000_0001, 0x3f8e_0000_0000_0000_0000_0000_0000_0000, 1, 0x3fff_0000_0000_0000_0000_0000_0000_0000),
        (0x3fff_0000_0000_0000_0000_0000_0000_0001, 0x3f8e_0000_0000_0000_0000_0000_0000_0000, 2, 0x3f8e_0000_0000_0000_0000_0000_0000_0001),
        (0x3fff_0000_0000_0000_0000_0000_0000_0001, 0x3f8e_0000_0000_0000_0000_0000_0000_0000, 3, 0x4070_0000_0000_0000_0000_0000_0000_0001),
        (0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0, 0x7fff_0000_0000_0000_0000_0000_0000_0000),
        (0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 1, 0x0000_0000_0000_0000_0000_0000_0000_0000),
        (0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 2, 0x7fff_0000_0000_0000_0000_0000_0000_0000),
        (0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 3, 0x3fff_0000_0000_0000_0000_0000_0000_0000),
        (0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x4000_0000_0000_0000_0000_0000_0000_0000, 0, 0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff),
        (0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x4000_0000_0000_0000_0000_0000_0000_0000, 1, 0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff),
        (0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x4000_0000_0000_0000_0000_0000_0000_0000, 2, 0x7fff_0000_0000_0000_0000_0000_0000_0000),
        (0x7ffe_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x4000_0000_0000_0000_0000_0000_0000_0000, 3, 0x7ffd_ffff_ffff_ffff_ffff_ffff_ffff_ffff),
        (0x0000_0000_0000_0000_0000_0000_0000_0001, 0x0000_0000_0000_0000_0000_0000_0000_0003, 0, 0x0000_0000_0000_0000_0000_0000_0000_0004),
        (0x0000_0000_0000_0000_0000_0000_0000_0001, 0x0000_0000_0000_0000_0000_0000_0000_0003, 1, 0x8000_0000_0000_0000_0000_0000_0000_0002),
        (0x0000_0000_0000_0000_0000_0000_0000_0001, 0x0000_0000_0000_0000_0000_0000_0000_0003, 2, 0x0000_0000_0000_0000_0000_0000_0000_0000),
        (0x0000_0000_0000_0000_0000_0000_0000_0001, 0x0000_0000_0000_0000_0000_0000_0000_0003, 3, 0x3ffd_5555_5555_5555_5555_5555_5555_5555),
        (0x0000_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x0001_0000_0000_0000_0000_0000_0000_0000, 0, 0x0001_ffff_ffff_ffff_ffff_ffff_ffff_ffff),
        (0x0000_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x0001_0000_0000_0000_0000_0000_0000_0000, 1, 0x8000_0000_0000_0000_0000_0000_0000_0001),
        (0x0000_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x0001_0000_0000_0000_0000_0000_0000_0000, 2, 0x0000_0000_0000_0000_0000_0000_0000_0000),
        (0x0000_ffff_ffff_ffff_ffff_ffff_ffff_ffff, 0x0001_0000_0000_0000_0000_0000_0000_0000, 3, 0x3ffe_ffff_ffff_ffff_ffff_ffff_ffff_fffe),
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x0000_0000_0000_0000_0000_0000_0000_0007, 0, 0x3fff_0000_0000_0000_0000_0000_0000_0000),
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x0000_0000_0000_0000_0000_0000_0000_0007, 1, 0x3fff_0000_0000_0000_0000_0000_0000_0000),
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x0000_0000_0000_0000_0000_0000_0000_0007, 2, 0x0000_0000_0000_0000_0000_0000_0000_0007),
        (0x3fff_0000_0000_0000_0000_0000_0000_0000, 0x0000_0000_0000_0000_0000_0000_0000_0007, 3, 0x7fff_0000_0000_0000_0000_0000_0000_0000),
        (0x4005_0000_0000_0000_0000_0000_0000_0000, 0x4002_0000_0000_0000_0000_0000_0000_0000, 0, 0x4005_2000_0000_0000_0000_0000_0000_0000),
        (0x4005_0000_0000_0000_0000_0000_0000_0000, 0x4002_0000_0000_0000_0000_0000_0000_0000, 1, 0x4004_c000_0000_0000_0000_0000_0000_0000),
        (0x4005_0000_0000_0000_0000_0000_0000_0000, 0x4002_0000_0000_0000_0000_0000_0000_0000, 2, 0x4008_0000_0000_0000_0000_0000_0000_0000),
        (0x4005_0000_0000_0000_0000_0000_0000_0000, 0x4002_0000_0000_0000_0000_0000_0000_0000, 3, 0x4002_0000_0000_0000_0000_0000_0000_0000),
        (0xbfff_0000_0000_0000_0000_0000_0000_0000, 0x4001_4000_0000_0000_0000_0000_0000_0000, 0, 0x4001_0000_0000_0000_0000_0000_0000_0000),
        (0xbfff_0000_0000_0000_0000_0000_0000_0000, 0x4001_4000_0000_0000_0000_0000_0000_0000, 1, 0xc001_8000_0000_0000_0000_0000_0000_0000),
        (0xbfff_0000_0000_0000_0000_0000_0000_0000, 0x4001_4000_0000_0000_0000_0000_0000_0000, 2, 0xc001_4000_0000_0000_0000_0000_0000_0000),
        (0xbfff_0000_0000_0000_0000_0000_0000_0000, 0x4001_4000_0000_0000_0000_0000_0000_0000, 3, 0xbffc_9999_9999_9999_9999_9999_9999_999a),
        (0xb000_77ae_0bf3_4dad_64f0_eeb9_026e_6076, 0x3100_ce91_e590_6136_305f_050c_368d_cc74, 0, 0x3100_ce91_e590_6136_305f_050c_368d_cc74),
        (0xb000_77ae_0bf3_4dad_64f0_eeb9_026e_6076, 0x3100_ce91_e590_6136_305f_050c_368d_cc74, 1, 0xb100_ce91_e590_6136_305f_050c_368d_cc74),
        (0xb000_77ae_0bf3_4dad_64f0_eeb9_026e_6076, 0x3100_ce91_e590_6136_305f_050c_368d_cc74, 2, 0xa102_5369_1a04_361e_e079_488d_6e2f_b15d),
        (0xb000_77ae_0bf3_4dad_64f0_eeb9_026e_6076, 0x3100_ce91_e590_6136_305f_050c_368d_cc74, 3, 0xbefe_9fd3_2da9_2e8a_bb56_e2b9_581f_d8b3),
        (0xdc1b_77ae_0bf3_4dad_64f0_eeb9_026e_6076, 0x5c1b_ce91_e590_6136_305f_050c_368d_cc74, 0, 0x5c19_5b8f_6674_4e23_2db8_594c_d07d_aff8),
        (0xdc1b_77ae_0bf3_4dad_64f0_eeb9_026e_6076, 0x5c1b_ce91_e590_6136_305f_050c_368d_cc74, 1, 0xdc1c_a31f_f8c1_d771_caa7_f9e2_9c7e_1675),
        (0xdc1b_77ae_0bf3_4dad_64f0_eeb9_026e_6076, 0x5c1b_ce91_e590_6136_305f_050c_368d_cc74, 2, 0xf838_5369_1a04_361e_e079_488d_6e2f_b15d),
        (0xdc1b_77ae_0bf3_4dad_64f0_eeb9_026e_6076, 0x5c1b_ce91_e590_6136_305f_050c_368d_cc74, 3, 0xbffe_9fd3_2da9_2e8a_bb56_e2b9_581f_d8b3),
        (0x3190_16e0_a1c5_4aec_9710_1dce_4e7b_fb79, 0xb290_e144_d6e8_f2cf_d9aa_792e_1af4_70ea, 0, 0xb290_e144_d6e8_f2cf_d9aa_792e_1af4_70ea),
        (0x3190_16e0_a1c5_4aec_9710_1dce_4e7b_fb79, 0xb290_e144_d6e8_f2cf_d9aa_792e_1af4_70ea, 1, 0x3290_e144_d6e8_f2cf_d9aa_792e_1af4_70ea),
        (0x3190_16e0_a1c5_4aec_9710_1dce_4e7b_fb79, 0xb290_e144_d6e8_f2cf_d9aa_792e_1af4_70ea, 2, 0xa422_0623_86de_1abf_5de4_4151_1bcd_afe2),
        (0x3190_16e0_a1c5_4aec_9710_1dce_4e7b_fb79, 0xb290_e144_d6e8_f2cf_d9aa_792e_1af4_70ea, 3, 0xbefe_28af_5c05_19ae_105f_1738_fb02_9379),
        (0x2ceb_16e0_a1c5_4aec_9710_1dce_4e7b_fb79, 0xaceb_e144_d6e8_f2cf_d9aa_792e_1af4_70ea, 0, 0xacea_94c8_6a47_4fc6_8534_b6bf_98f0_eae2),
        (0x2ceb_16e0_a1c5_4aec_9710_1dce_4e7b_fb79, 0xaceb_e144_d6e8_f2cf_d9aa_792e_1af4_70ea, 1, 0x2cec_7c12_bc57_1ede_385d_4b7e_34b8_3632),
        (0x2ceb_16e0_a1c5_4aec_9710_1dce_4e7b_fb79, 0xaceb_e144_d6e8_f2cf_d9aa_792e_1af4_70ea, 2, 0x99d8_0623_86de_1abf_5de4_4151_1bcd_afe2),
        (0x2ceb_16e0_a1c5_4aec_9710_1dce_4e7b_fb79, 0xaceb_e144_d6e8_f2cf_d9aa_792e_1af4_70ea, 3, 0xbffe_28af_5c05_19ae_105f_1738_fb02_9379),
        (0xb320_4e85_b0d6_e28b_8f8e_a9d3_4942_8d8e, 0x3420_74ff_b8e8_ab15_2ead_8547_56d7_1f03, 0, 0x3420_74ff_b8e8_ab15_2ead_8547_56d7_1f03),
        (0xb320_4e85_b0d6_e28b_8f8e_a9d3_4942_8d8e, 0x3420_74ff_b8e8_ab15_2ead_8547_56d7_1f03, 1, 0xb420_74ff_b8e8_ab15_2ead_8547_56d7_1f03),
        (0xb320_4e85_b0d6_e28b_8f8e_a9d3_4942_8d8e, 0x3420_74ff_b8e8_ab15_2ead_8547_56d7_1f03, 2, 0xa741_e768_6dc3_8710_2646_bac6_6cf5_64e0),
        (0xb320_4e85_b0d6_e28b_8f8e_a9d3_4942_8d8e, 0x3420_74ff_b8e8_ab15_2ead_8547_56d7_1f03, 3, 0xbefe_cb2f_4623_811a_0fa5_0753_6587_1850),
        (0xddaa_4e85_b0d6_e28b_8f8e_a9d3_4942_8d8e, 0x5daa_74ff_b8e8_ab15_2ead_8547_56d7_1f03, 0, 0x5da7_33d0_408e_444c_f8f6_dba0_6ca4_8ba8),
        (0xddaa_4e85_b0d6_e28b_8f8e_a9d3_4942_8d8e, 0x5daa_74ff_b8e8_ab15_2ead_8547_56d7_1f03, 1, 0xddab_61c2_b4df_c6d0_5f1e_178d_500c_d648),
        (0xddaa_4e85_b0d6_e28b_8f8e_a9d3_4942_8d8e, 0x5daa_74ff_b8e8_ab15_2ead_8547_56d7_1f03, 2, 0xfb55_e768_6dc3_8710_2646_bac6_6cf5_64e0),
        (0xddaa_4e85_b0d6_e28b_8f8e_a9d3_4942_8d8e, 0x5daa_74ff_b8e8_ab15_2ead_8547_56d7_1f03, 3, 0xbffe_cb2f_4623_811a_0fa5_0753_6587_1850),
        (0x34b0_79f8_ada7_11fd_0e1f_c49b_d63b_809e, 0xb5b0_99e8_3f5a_101f_c576_5079_fc5d_43ff, 0, 0xb5b0_99e8_3f5a_101f_c576_5079_fc5d_43ff),
        (0x34b0_79f8_ada7_11fd_0e1f_c49b_d63b_809e, 0xb5b0_99e8_3f5a_101f_c576_5079_fc5d_43ff, 1, 0x35b0_99e8_3f5a_101f_c576_5079_fc5d_43ff),
        (0x34b0_79f8_ada7_11fd_0e1f_c49b_d63b_809e, 0xb5b0_99e8_3f5a_101f_c576_5079_fc5d_43ff, 2, 0xaa62_2e9a_9a2b_3b8e_9b48_d663_2d6b_17ea),
        (0x34b0_79f8_ada7_11fd_0e1f_c49b_d63b_809e, 0xb5b0_99e8_3f5a_101f_c576_5079_fc5d_43ff, 3, 0xbefe_d81c_32a5_ffa4_4156_24d7_55de_e9fa),
        (0x55bc_79f8_ada7_11fd_0e1f_c49b_d63b_809e, 0xd5bc_99e8_3f5a_101f_c576_5079_fc5d_43ff, 0, 0xd5b8_fef9_1b2f_e22b_7568_bde2_621c_3610),
        (0x55bc_79f8_ada7_11fd_0e1f_c49b_d63b_809e, 0xd5bc_99e8_3f5a_101f_c576_5079_fc5d_43ff, 1, 0x55bd_89f0_7680_910e_69cb_0a8a_e94c_624e),
        (0x55bc_79f8_ada7_11fd_0e1f_c49b_d63b_809e, 0xd5bc_99e8_3f5a_101f_c576_5079_fc5d_43ff, 2, 0xeb7a_2e9a_9a2b_3b8e_9b48_d663_2d6b_17ea),
        (0x55bc_79f8_ada7_11fd_0e1f_c49b_d63b_809e, 0xd5bc_99e8_3f5a_101f_c576_5079_fc5d_43ff, 3, 0xbffe_d81c_32a5_ffa4_4156_24d7_55de_e9fa),
        (0x3640_fc38_7dfa_e6b8_a32e_dabf_5585_bd75, 0xb740_39b1_6b71_4b4f_92fb_2dcf_c8ae_9a19, 0, 0xb740_39b1_6b71_4b4f_92fb_2dcf_c8ae_9a19),
        (0x3640_fc38_7dfa_e6b8_a32e_dabf_5585_bd75, 0xb740_39b1_6b71_4b4f_92fb_2dcf_c8ae_9a19, 1, 0x3740_39b1_6b71_4b4f_92fb_2dcf_c8ae_9a19),
        (0x3640_fc38_7dfa_e6b8_a32e_dabf_5585_bd75, 0xb740_39b1_6b71_4b4f_92fb_2dcf_c8ae_9a19, 2, 0xad82_3760_a531_b2d5_a2bf_3061_1047_c110),
        (0x3640_fc38_7dfa_e6b8_a32e_dabf_5585_bd75, 0xb740_39b1_6b71_4b4f_92fb_2dcf_c8ae_9a19, 3, 0xbeff_9ec0_3ef8_32a5_e757_1689_a349_42c0),
        (0x353c_fc38_7dfa_e6b8_a32e_dabf_5585_bd75, 0xb53c_39b1_6b71_4b4f_92fb_2dcf_c8ae_9a19, 0, 0x353b_850e_2513_36d2_2067_59df_19ae_46b8),
        (0x353c_fc38_7dfa_e6b8_a32e_dabf_5585_bd75, 0xb53c_39b1_6b71_4b4f_92fb_2dcf_c8ae_9a19, 1, 0x353d_9af4_f4b6_1904_1b15_0447_8f1a_2bc7),
        (0x353c_fc38_7dfa_e6b8_a32e_dabf_5585_bd75, 0xb53c_39b1_6b71_4b4f_92fb_2dcf_c8ae_9a19, 2, 0xaa7a_3760_a531_b2d5_a2bf_3061_1047_c110),
        (0x353c_fc38_7dfa_e6b8_a32e_dabf_5585_bd75, 0xb53c_39b1_6b71_4b4f_92fb_2dcf_c8ae_9a19, 3, 0xbfff_9ec0_3ef8_32a5_e757_1689_a349_42c0),
    ];

    #[test]
    fn the_answers_libgcc_gives_come_out_bit_for_bit() {
        for &(left, right, which, wanted) in LIBGCC {
            let got = ours(which, left, right);
            assert_eq!(
                got,
                wanted,
                "{left:032x} {} {right:032x}: got {got:032x}, libgcc says {wanted:032x}",
                name(which)
            );
        }
    }

    /// A quad taken apart again, written here rather than called from above so that a mistake in
    /// `parts` cannot hide behind itself.
    fn exact_of(bits: u128) -> (u128, i32) {
        let stored = ((bits >> FRACTION) & TOP) as i32;
        let fraction = bits & FRACTION_MASK;
        if stored == 0 {
            (fraction, SMALLEST)
        } else {
            (fraction + IMPLICIT, stored - BIAS - FRACTION as i32)
        }
    }

    /// The distance between an exact value and one candidate answer, both as an integer and a scale.
    ///
    /// The two are lined up at whichever scale is lower and subtracted there, so the answer is exact.
    /// The shifts are asserted rather than clamped because a silent zero here would pass every test
    /// below for the wrong reason.
    fn distance(value: (Wide, i32), candidate: (Wide, i32)) -> Wide {
        let scale = value.1.min(candidate.1);
        let lined = |what: (Wide, i32)| {
            let up = (what.1 - scale) as u32;
            assert!(up < 128, "lining the two up takes more width than Wide has");
            what.0.shifted_up(up)
        };
        let one = lined(value);
        let two = lined(candidate);
        if one >= two { one.minus(two) } else { two.minus(one) }
    }

    /// Holds one answer against the exact value it is supposed to be the nearest quad to.
    ///
    /// This is the property rounding to nearest is defined by rather than a second rounding routine
    /// that could be wrong the same way the first one is: the answer is no further from the exact
    /// value than either of the two quads beside it, and where it is exactly as far from one of them
    /// the answer is the one whose lowest bit is clear. The neighbours are the answer's own pattern
    /// plus and minus one, which is what the next quad up and the next one down are at every exponent
    /// the format has.
    fn check_nearest(what: &str, answer: u128, sign: u128, magnitude: Wide, scale: i32) {
        assert_eq!(answer & SIGN, sign, "{what}: the sign of {answer:032x}");
        let pattern = answer & !SIGN;
        assert!(
            pattern >> FRACTION & TOP != TOP,
            "{what}: {answer:032x} is an infinity or a not a number"
        );
        assert!(pattern != 0, "{what}: {answer:032x} is a zero");

        let value = (magnitude, scale);
        let here = exact_of(pattern);
        let mine = distance(value, (Wide::narrow(here.0), here.1));
        for neighbour in [pattern - 1, pattern + 1] {
            let there = exact_of(neighbour);
            let theirs = distance(value, (Wide::narrow(there.0), there.1));
            assert!(mine <= theirs, "{what}: {answer:032x} is further off than {neighbour:032x}");
            if mine == theirs {
                assert!(pattern & 1 == 0, "{what}: the tie at {answer:032x} went to the odd one");
            }
        }
    }

    /// The exact product of two significands, by shifting and adding rather than by the routine under
    /// test, so that the test knows the answer independently of how the file works it out.
    fn product_by_hand(left: u128, right: u128) -> Wide {
        let mut out = Wide::ZERO;
        for at in 0..128 {
            if right >> at & 1 == 1 {
                out = out.plus(Wide::narrow(left).shifted_up(at));
            }
        }
        out
    }

    #[test]
    fn a_sum_is_the_nearest_quad_to_the_exact_sum() {
        let mut stream = Stream(0x2545_f491_4f6c_dd1d);
        for _ in 0..4_000 {
            // The middle of the range, where the exact answer and the grid the answer sits on are
            // close enough together to line up inside a `Wide`. The two ends have a test of their
            // own below and rows of their own in the table above.
            let exponent = 0x3000 + u128::from(stream.next() % 0x2000);
            let left = stream.at(exponent);
            for right in [stream.near(left), stream.at(0x4000), left ^ SIGN] {
                let (big, small) =
                    if left & !SIGN >= right & !SIGN { (left, right) } else { (right, left) };
                let big_parts = exact_of(big & !SIGN);
                let small_parts = exact_of(small & !SIGN);
                let shift = (big_parts.1 - small_parts.1) as u32;
                if shift >= 128 {
                    continue;
                }
                let aligned = Wide::narrow(big_parts.0).shifted_up(shift);
                let other = Wide::narrow(small_parts.0);
                let opposite = (big ^ small) & SIGN != 0;
                let magnitude = if opposite { aligned.minus(other) } else { aligned.plus(other) };
                if magnitude.is_zero() {
                    continue;
                }
                let answer = add(left, right);
                check_nearest("sum", answer, big & SIGN, magnitude, small_parts.1);
            }
        }
    }

    #[test]
    fn a_product_is_the_nearest_quad_to_the_exact_product() {
        let mut stream = Stream(0x9e37_79b9_7f4a_7c15);
        for _ in 0..4_000 {
            // Exponents that add up to something the format holds, so that the answer is a number and
            // not an infinity, and the nearest property is the one being checked.
            let left_exponent = 0x3f00 + u128::from(stream.next() % 0x200);
            let left = stream.at(left_exponent);
            let right_exponent = 0x3f00 + u128::from(stream.next() % 0x200);
            let right = stream.at(right_exponent);
            let left_parts = exact_of(left & !SIGN);
            let right_parts = exact_of(right & !SIGN);
            let magnitude = product_by_hand(left_parts.0, right_parts.0);
            let answer = multiply(left, right);
            check_nearest(
                "product",
                answer,
                (left ^ right) & SIGN,
                magnitude,
                left_parts.1 + right_parts.1,
            );
        }
    }

    #[test]
    fn a_quotient_times_the_divisor_is_the_dividend_again() {
        let mut stream = Stream(0x1234_5678_9abc_def1);
        for _ in 0..2_000 {
            // A dividend that is exactly one quad times another, so the quotient is exact and the
            // answer is known without rounding anything: two factors of fifty six bits multiply into
            // a hundred and twelve, which the format holds.
            let first = u128::from(stream.next() >> 8) | 1 << 55;
            let second = u128::from(stream.next() >> 8) | 1 << 55;
            let one = round_from(0, Wide::narrow(first), 0, false);
            let two = round_from(0, Wide::narrow(second), 0, false);
            let product = round_from(0, Wide::narrow(first * second), 0, false);
            assert_eq!(divide(product, one), two, "{product:032x} over {one:032x}");
            assert_eq!(divide(product, two), one, "{product:032x} over {two:032x}");
            // And a power of two, where the answer is the dividend with its exponent moved and every
            // bit of its fraction where it was.
            let exponent = 0x4000 + u128::from(stream.next() % 0x1000);
            let value = stream.at(exponent);
            let half = divide(value, round_from(0, Wide::narrow(2), 0, false));
            assert_eq!(half, value - (1 << FRACTION), "half of {value:032x}");
        }
    }

    #[test]
    fn a_tie_goes_to_the_even_one() {
        let mut stream = Stream(0xfeed_face_dead_beef);
        for _ in 0..4_000 {
            // A value and exactly half the spacing of its own exponent, which is a tie at every
            // exponent rather than only at the one above. The fraction is forced away from zero so
            // that the spacing below the value is the spacing above it, and the lowest bit is forced
            // each way so that both answers a tie can have are asked for.
            let stored = 200 + u128::from(stream.next() % 32_000);
            let value = (stream.at(stored) & !SIGN) | (1 << (FRACTION - 1));
            let half = (stored - FRACTION as u128 - 1) << FRACTION;
            for value in [value & !1, value | 1] {
                let up = if value & 1 == 0 { value } else { value + 1 };
                let down = if value & 1 == 0 { value } else { value - 1 };
                assert_eq!(add(value, half), up, "{value:032x} plus half a step");
                assert_eq!(add(value, half ^ SIGN), down, "{value:032x} less half a step");
                assert_eq!(add(value ^ SIGN, half ^ SIGN), up ^ SIGN, "the same going down");
            }
        }
    }

    #[test]
    fn two_subnormals_add_exactly() {
        // The bottom of the range, where every value is a multiple of the same lowest bit, so a sum
        // and a difference are the integer sum and difference of the two patterns and nothing is
        // rounded at all. That is what makes this end checkable without lining anything up.
        let mut stream = Stream(0x0bad_c0de_0bad_c0de);
        for _ in 0..4_000 {
            let left = stream.any() & FRACTION_MASK;
            let right = stream.any() & FRACTION_MASK;
            assert_eq!(add(left, right), left + right, "{left:032x} plus {right:032x}");
            let larger = left.max(right);
            let smaller = left.min(right);
            assert_eq!(
                add(larger, smaller ^ SIGN),
                larger - smaller,
                "{larger:032x} less {smaller:032x}"
            );
        }
        // And the step out of the subnormals, which is the one place a carry moves the exponent from
        // zero to one and the implicit bit starts being implied.
        assert_eq!(add(FRACTION_MASK, 1), IMPLICIT, "the largest subnormal and one step");
        assert_eq!(add(IMPLICIT, negated(1)), FRACTION_MASK, "and the step back down");
    }

    #[test]
    fn the_top_of_the_range_rounds_to_an_infinity() {
        let largest = (TOP - 1) << FRACTION | FRACTION_MASK;
        let infinity = TOP << FRACTION;
        // Half the spacing at the largest finite value, which is what it takes to round up from it.
        let half_step = (TOP - 1 - FRACTION as u128 - 1) << FRACTION;
        assert_eq!(add(largest, largest), infinity);
        assert_eq!(add(largest, half_step), infinity);
        assert_eq!(multiply(largest, round_from(0, Wide::narrow(2), 0, false)), infinity);
        assert_eq!(divide(largest, round_from(0, Wide::narrow(1), -1, false)), infinity);
        assert_eq!(add(largest | SIGN, largest), 0);
        assert_eq!(add(largest | SIGN, largest | SIGN), infinity | SIGN);
        // And just under it, where the rounding goes the other way and the answer is still a number.
        assert_eq!(add(largest, half_step - 1), largest);
    }

    #[test]
    fn the_signs_of_zero_are_the_ones_ieee_asks_for() {
        let one = round_from(0, Wide::narrow(1), 0, false);
        let three = round_from(0, Wide::narrow(3), 0, false);
        // A positive and a negative zero add to a positive one, a number less itself is positive, and
        // a product or a quotient takes the sign of the two signs multiplied.
        assert_eq!(add(0, SIGN), 0);
        assert_eq!(add(SIGN, 0), 0);
        assert_eq!(add(SIGN, SIGN), SIGN);
        assert_eq!(add(0, 0), 0);
        assert_eq!(add(one, negated(one)), 0);
        assert_eq!(add(one | SIGN, negated(one | SIGN)), 0);
        assert_eq!(multiply(SIGN, three), SIGN);
        assert_eq!(divide(SIGN, three), SIGN);
        assert_eq!(divide(three, SIGN), TOP << FRACTION | SIGN);
    }

    #[test]
    fn an_operation_with_no_answer_gives_the_empty_not_a_number() {
        let infinity = TOP << FRACTION;
        let one = round_from(0, Wide::narrow(1), 0, false);
        assert_eq!(add(infinity, infinity | SIGN), EMPTY_NAN);
        assert_eq!(add(infinity, infinity), infinity);
        assert_eq!(multiply(infinity, 0), EMPTY_NAN);
        assert_eq!(multiply(infinity | SIGN, 0), EMPTY_NAN);
        assert_eq!(divide(0, 0), EMPTY_NAN);
        assert_eq!(divide(infinity, infinity), EMPTY_NAN);
        assert_eq!(divide(one, 0), infinity);
        assert_eq!(divide(one, SIGN), infinity | SIGN);
    }

    #[test]
    fn a_not_a_number_comes_back_quieted_and_a_subtraction_does_not_flip_it() {
        let signalling = (TOP << FRACTION) | 1;
        let quiet_one = EMPTY_NAN | 7;
        let one = round_from(0, Wide::narrow(1), 0, false);
        for which in 0..4 {
            assert_eq!(ours(which, signalling, one), signalling | QUIET, "{}", name(which));
            assert_eq!(ours(which, one, signalling), signalling | QUIET, "{}", name(which));
            // Both operands a not a number: the left one comes back, which is this library's rule.
            assert_eq!(ours(which, quiet_one, signalling), quiet_one, "{}", name(which));
            // And the sign of the one that came in is the sign of the one that comes out, in a
            // subtraction as much as in an addition.
            assert_eq!(ours(which, one, signalling | SIGN), signalling | SIGN | QUIET);
        }
        assert_eq!(negated(signalling), signalling, "negated for a subtraction");
        assert_eq!(signalling ^ SIGN, (TOP << FRACTION) | SIGN | 1, "but __negtf2 flips one");
    }

    /// What the comparisons have to answer, which is the order the two patterns are already in.
    ///
    /// This is the property the shipped C leans on and this file does not use: two values of the same
    /// sign order the way their patterns do as integers, and two of opposite signs order the other way
    /// round. Holding one against the other is the point of having both.
    fn check_comparisons(left: u128, right: u128) {
        let unordered = is_nan(left) || is_nan(right);
        let wanted = if unordered {
            None
        } else if left & !SIGN == 0 && right & !SIGN == 0 {
            Some(core::cmp::Ordering::Equal)
        } else if left & SIGN != right & SIGN {
            Some(if left & SIGN != 0 {
                core::cmp::Ordering::Less
            } else {
                core::cmp::Ordering::Greater
            })
        } else {
            let order = (left & !SIGN).cmp(&(right & !SIGN));
            Some(if left & SIGN != 0 { order.reverse() } else { order })
        };

        let answers: [(&str, i32, bool); 7] = [
            ("eq", compare(left, right, 1), wanted == Some(core::cmp::Ordering::Equal)),
            ("ne", compare(left, right, 1), wanted != Some(core::cmp::Ordering::Equal)),
            (
                "ge",
                compare(left, right, -1),
                matches!(wanted, Some(core::cmp::Ordering::Greater | core::cmp::Ordering::Equal)),
            ),
            ("gt", compare(left, right, -1), wanted == Some(core::cmp::Ordering::Greater)),
            (
                "le",
                compare(left, right, 1),
                matches!(wanted, Some(core::cmp::Ordering::Less | core::cmp::Ordering::Equal)),
            ),
            ("lt", compare(left, right, 1), wanted == Some(core::cmp::Ordering::Less)),
            ("unord", i32::from(unordered), unordered),
        ];
        for (name, answer, wanted) in answers {
            let ours = match name {
                "eq" => answer == 0,
                "ne" => answer != 0,
                "ge" => answer >= 0,
                "gt" => answer > 0,
                "le" => answer <= 0,
                "lt" => answer < 0,
                _ => answer != 0,
            };
            assert_eq!(
                ours, wanted,
                "{name} of {left:032x} and {right:032x}: we say {ours}, the patterns say {wanted}"
            );
        }
        // And the three way answer, whose whole sign is specified rather than one side of zero.
        let three_way = match wanted {
            None => 1,
            Some(core::cmp::Ordering::Less) => -1,
            Some(core::cmp::Ordering::Equal) => 0,
            Some(core::cmp::Ordering::Greater) => 1,
        };
        assert_eq!(compare(left, right, 1), three_way, "cmp of {left:032x} and {right:032x}");
    }

    /// The values worth asking about by name rather than by luck: both zeros, the ends of the
    /// subnormal range, the ends of the normal one, one either side of a rounding decision, and the
    /// two kinds of not a number.
    fn corners() -> Vec<u128> {
        let mut out = Vec::new();
        for bits in [
            0,
            1,
            2,
            FRACTION_MASK,
            IMPLICIT,
            IMPLICIT + 1,
            (0x3ffe << FRACTION) | FRACTION_MASK,
            0x3fff << FRACTION,
            (0x3fff << FRACTION) | 1,
            0x4000 << FRACTION,
            0x406f << FRACTION,
            ((TOP - 1) << FRACTION) | FRACTION_MASK,
            TOP << FRACTION,
            EMPTY_NAN,
            (TOP << FRACTION) | 1,
            0x3f8e << FRACTION,
        ] {
            out.push(bits);
            out.push(bits | SIGN);
        }
        out
    }

    #[test]
    fn the_comparisons_answer_what_the_patterns_say() {
        let mut stream = Stream(0x0123_4567_89ab_cdef);
        for _ in 0..20_000 {
            let left = stream.any();
            let right = stream.any();
            check_comparisons(left, right);
            check_comparisons(left, stream.near(left));
            // The same value on both sides, which is the case an implementation that compares
            // patterns and one that compares values can still differ on: a negative zero against a
            // positive one, and a not a number against itself.
            check_comparisons(left, left);
            check_comparisons(left, left ^ SIGN);
            // Two values at one exponent, where only the fractions decide it.
            let stored = 1 + u128::from(stream.next() % (TOP as u64 - 1));
            check_comparisons(stream.at(stored), stream.at(stored));
        }
        let values = corners();
        for left in &values {
            for right in &values {
                check_comparisons(*left, *right);
            }
        }
    }

    /// The quads libgcc makes of an integer, for fourteen integers by name and six rounds of random
    /// ones at two widths, through all four routines going up. `which` is the routine, in the order
    /// `__floatsitf`, `__floatunsitf`, `__floatditf`, `__floatunditf`, and the integer is carried as
    /// the bits of it, which each routine reads at its own width and signedness.
    #[rustfmt::skip]
    const LIBGCC_UP: &[(u64, u8, u128)] = &[
        (0x0000000000000000, 0, 0x00000000000000000000000000000000),
        (0x0000000000000000, 1, 0x00000000000000000000000000000000),
        (0x0000000000000000, 2, 0x00000000000000000000000000000000),
        (0x0000000000000000, 3, 0x00000000000000000000000000000000),
        (0x0000000000000001, 0, 0x3fff0000000000000000000000000000),
        (0x0000000000000001, 1, 0x3fff0000000000000000000000000000),
        (0x0000000000000001, 2, 0x3fff0000000000000000000000000000),
        (0x0000000000000001, 3, 0x3fff0000000000000000000000000000),
        (0xffffffffffffffff, 0, 0xbfff0000000000000000000000000000),
        (0xffffffffffffffff, 1, 0x401efffffffe00000000000000000000),
        (0xffffffffffffffff, 2, 0xbfff0000000000000000000000000000),
        (0xffffffffffffffff, 3, 0x403efffffffffffffffe000000000000),
        (0x0000000000000007, 0, 0x4001c000000000000000000000000000),
        (0x0000000000000007, 1, 0x4001c000000000000000000000000000),
        (0x0000000000000007, 2, 0x4001c000000000000000000000000000),
        (0x0000000000000007, 3, 0x4001c000000000000000000000000000),
        (0x00000000fffffff9, 0, 0xc001c000000000000000000000000000),
        (0x00000000fffffff9, 1, 0x401efffffff200000000000000000000),
        (0x00000000fffffff9, 2, 0x401efffffff200000000000000000000),
        (0x00000000fffffff9, 3, 0x401efffffff200000000000000000000),
        (0x000000007fffffff, 0, 0x401dfffffffc00000000000000000000),
        (0x000000007fffffff, 1, 0x401dfffffffc00000000000000000000),
        (0x000000007fffffff, 2, 0x401dfffffffc00000000000000000000),
        (0x000000007fffffff, 3, 0x401dfffffffc00000000000000000000),
        (0x0000000080000000, 0, 0xc01e0000000000000000000000000000),
        (0x0000000080000000, 1, 0x401e0000000000000000000000000000),
        (0x0000000080000000, 2, 0x401e0000000000000000000000000000),
        (0x0000000080000000, 3, 0x401e0000000000000000000000000000),
        (0x00000000ffffffff, 0, 0xbfff0000000000000000000000000000),
        (0x00000000ffffffff, 1, 0x401efffffffe00000000000000000000),
        (0x00000000ffffffff, 2, 0x401efffffffe00000000000000000000),
        (0x00000000ffffffff, 3, 0x401efffffffe00000000000000000000),
        (0x7fffffffffffffff, 0, 0xbfff0000000000000000000000000000),
        (0x7fffffffffffffff, 1, 0x401efffffffe00000000000000000000),
        (0x7fffffffffffffff, 2, 0x403dfffffffffffffffc000000000000),
        (0x7fffffffffffffff, 3, 0x403dfffffffffffffffc000000000000),
        (0x8000000000000000, 0, 0x00000000000000000000000000000000),
        (0x8000000000000000, 1, 0x00000000000000000000000000000000),
        (0x8000000000000000, 2, 0xc03e0000000000000000000000000000),
        (0x8000000000000000, 3, 0x403e0000000000000000000000000000),
        (0x0123456789abcdef, 0, 0xc01dd950c84400000000000000000000),
        (0x0123456789abcdef, 1, 0x401e13579bde00000000000000000000),
        (0x0123456789abcdef, 2, 0x403723456789abcdef00000000000000),
        (0x0123456789abcdef, 3, 0x403723456789abcdef00000000000000),
        (0xfedcba9876543210, 0, 0x401dd950c84000000000000000000000),
        (0xfedcba9876543210, 1, 0x401dd950c84000000000000000000000),
        (0xfedcba9876543210, 2, 0xc03723456789abcdf000000000000000),
        (0xfedcba9876543210, 3, 0x403efdb97530eca86420000000000000),
        (0x0000000100000000, 0, 0x00000000000000000000000000000000),
        (0x0000000100000000, 1, 0x00000000000000000000000000000000),
        (0x0000000100000000, 2, 0x401f0000000000000000000000000000),
        (0x0000000100000000, 3, 0x401f0000000000000000000000000000),
        (0x001fffffffffffff, 0, 0xbfff0000000000000000000000000000),
        (0x001fffffffffffff, 1, 0x401efffffffe00000000000000000000),
        (0x001fffffffffffff, 2, 0x4033fffffffffffff000000000000000),
        (0x001fffffffffffff, 3, 0x4033fffffffffffff000000000000000),
        (0x856d9c28a9741a02, 0, 0xc01d5a2f97f800000000000000000000),
        (0x856d9c28a9741a02, 1, 0x401e52e8340400000000000000000000),
        (0x856d9c28a9741a02, 2, 0xc03dea498f5d5a2f97f8000000000000),
        (0x856d9c28a9741a02, 3, 0x403e0adb385152e83404000000000000),
        (0x0000000042b6ce14, 0, 0x401d0adb385000000000000000000000),
        (0x0000000042b6ce14, 1, 0x401d0adb385000000000000000000000),
        (0x0000000042b6ce14, 2, 0x401d0adb385000000000000000000000),
        (0x0000000042b6ce14, 3, 0x401d0adb385000000000000000000000),
        (0x86ad14d4430c32b6, 0, 0x401d0c30cad800000000000000000000),
        (0x86ad14d4430c32b6, 1, 0x401d0c30cad800000000000000000000),
        (0x86ad14d4430c32b6, 2, 0xc03de54bacaef3cf3528000000000000),
        (0x86ad14d4430c32b6, 3, 0x403e0d5a29a88618656c000000000000),
        (0x0000000043568a6a, 0, 0x401d0d5a29a800000000000000000000),
        (0x0000000043568a6a, 1, 0x401d0d5a29a800000000000000000000),
        (0x0000000043568a6a, 2, 0x401d0d5a29a800000000000000000000),
        (0x0000000043568a6a, 3, 0x401d0d5a29a800000000000000000000),
        (0xc366ae2c20764753, 0, 0x401c03b23a9800000000000000000000),
        (0xc366ae2c20764753, 1, 0x401c03b23a9800000000000000000000),
        (0xc366ae2c20764753, 2, 0xc03ce4ca8e9efc4dc568000000000000),
        (0xc366ae2c20764753, 3, 0x403e86cd5c5840ec8ea6000000000000),
        (0x0000000061b35716, 0, 0x401d86cd5c5800000000000000000000),
        (0x0000000061b35716, 1, 0x401d86cd5c5800000000000000000000),
        (0x0000000061b35716, 2, 0x401d86cd5c5800000000000000000000),
        (0x0000000061b35716, 3, 0x401d86cd5c5800000000000000000000),
        (0xce6336ec93771f1d, 0, 0xc01db223838c00000000000000000000),
        (0xce6336ec93771f1d, 1, 0x401e26ee3e3a00000000000000000000),
        (0xce6336ec93771f1d, 2, 0xc03c8ce6489b64470718000000000000),
        (0xce6336ec93771f1d, 3, 0x403e9cc66dd926ee3e3a000000000000),
        (0x0000000067319b76, 0, 0x401d9cc66dd800000000000000000000),
        (0x0000000067319b76, 1, 0x401d9cc66dd800000000000000000000),
        (0x0000000067319b76, 2, 0x401d9cc66dd800000000000000000000),
        (0x0000000067319b76, 3, 0x401d9cc66dd800000000000000000000),
        (0x1a79312058b39663, 0, 0x401d62ce598c00000000000000000000),
        (0x1a79312058b39663, 1, 0x401d62ce598c00000000000000000000),
        (0x1a79312058b39663, 2, 0x403ba79312058b396630000000000000),
        (0x1a79312058b39663, 3, 0x403ba79312058b396630000000000000),
        (0x000000000d3c9890, 0, 0x401aa793120000000000000000000000),
        (0x000000000d3c9890, 1, 0x401aa793120000000000000000000000),
        (0x000000000d3c9890, 2, 0x401aa793120000000000000000000000),
        (0x000000000d3c9890, 3, 0x401aa793120000000000000000000000),
        (0x3ca10c145535098f, 0, 0x401d54d4263c00000000000000000000),
        (0x3ca10c145535098f, 1, 0x401d54d4263c00000000000000000000),
        (0x3ca10c145535098f, 2, 0x403ce50860a2a9a84c78000000000000),
        (0x3ca10c145535098f, 3, 0x403ce50860a2a9a84c78000000000000),
        (0x000000001e50860a, 0, 0x401be50860a000000000000000000000),
        (0x000000001e50860a, 1, 0x401be50860a000000000000000000000),
        (0x000000001e50860a, 2, 0x401be50860a000000000000000000000),
        (0x000000001e50860a, 3, 0x401be50860a000000000000000000000),
    ];

    /// The integers libgcc truncates a quad to, for the values where C defines all four answers,
    /// which is a magnitude below 2^31 and a value at or above zero.
    #[rustfmt::skip]
    const LIBGCC_DOWN: &[(u128, i32, u32, i64, u64)] = &[
        (0x00000000000000000000000000000001, 0, 0, 0, 0),
        (0x3ffe0000000000000000000000000000, 0, 0, 0, 0),
        (0x3fff0000000000000000000000000000, 1, 1, 1, 1),
        (0x3fff8000000000000000000000000000, 1, 1, 1, 1),
        (0x401dfffffffffc000000000000000000, 2147483647, 2147483647, 2147483647, 2147483647),
        (0x401dfffffffffbffffffffffffffffff, 2147483647, 2147483647, 2147483647, 2147483647),
        (0x401b592f2cfca7a74a65d13977285728, 361951951, 361951951, 361951951, 361951951),
        (0x4015c8f426bf199df806bab4c3dba0ee, 7486729, 7486729, 7486729, 7486729),
        (0x400d28cd5306e837aaa36b90c5520827, 18995, 18995, 18995, 18995),
        (0x4007179b6f92b8bcac7169758f6e32cd, 279, 279, 279, 279),
        (0x401608c2bd956c136cc0eba0b421220b, 8675678, 8675678, 8675678, 8675678),
        (0x4009d6415c837c4a3bafc2f2c1082832, 1881, 1881, 1881, 1881),
        (0x400522d29041888bea384c1f86a6495a, 72, 72, 72, 72),
        (0x400e100bfa4cc56e2c4637d568ecc764, 34821, 34821, 34821, 34821),
    ];

    /// The same for a negative value, where C defines the two signed routines and leaves the two
    /// unsigned ones undefined, so only the two are held to libgcc here.
    #[rustfmt::skip]
    const LIBGCC_DOWN_NEGATIVE: &[(u128, i32, i64)] = &[
        (0x80000000000000000000000000000001, 0, 0),
        (0xbffe0000000000000000000000000000, 0, 0),
        (0xbfff0000000000000000000000000000, -1, -1),
        (0xbfff8000000000000000000000000000, -1, -1),
        (0xc01e0000000000000000000000000000, -2147483648, -2147483648),
        (0xc01dffffffffffffffffffffffffffff, -2147483647, -2147483647),
        (0xc01694c9bbce95e583958bb6aabcd18e, -13264093, -13264093),
        (0xc0187fe4197e2cfa4510fb83171eae23, -50317362, -50317362),
        (0xc00d8e2867f03688edd0ee15242974e5, -25482, -25482),
        (0xc0081a0dff329d32d494d93b92ddf488, -564, -564),
        (0xc002b278017ab9fbda9a0ae15c795248, -13, -13),
        (0xc00d5620a2d90189cbadf1249679f1ca, -21896, -21896),
        (0xc00d1b12adf7be88b1ce5514e578f3f5, -18116, -18116),
        (0xc01692077a15636c747d3e17788012aa, -13173693, -13173693),
    ];

    /// And for a magnitude between 2^31 and 2^63, where the two sixty four bit routines are the ones
    /// with an answer in them.
    #[rustfmt::skip]
    const LIBGCC_DOWN_WIDE: &[(u128, i64, u64)] = &[
        (0x401e0000000000000000000000000000, 2147483648, 2147483648),
        (0x403dfffffffffffff000000000000000, 9223372036854774784, 9223372036854774784),
        (0x403dffffffffffffffffffffffffffff, 9223372036854775807, 9223372036854775807),
        (0x402df0dd51f7663f2e36a4c3b42ce733, 136577082579343, 136577082579343),
        (0x403bb9a2cd51ff599cfca377b691aae7, 1988951479313340879, 1988951479313340879),
        (0x4030880c2de0d152d96976e15f450470, 862121734808229, 862121734808229),
        (0x403687d2d486855c525d88b23e895f56, 55144263062826537, 55144263062826537),
        (0x4026843291b570d8ea2e100a6fa32c39, 833647860408, 833647860408),
        (0x401fa15df14988e918115ce22aa401b8, 7002255689, 7002255689),
        (0x40391ada2137c9348bc5e903f6ed6ba6, 318463118548455983, 318463118548455983),
        (0x402f2ad794591d80176f6ee191f0cfbb, 328580371914112, 328580371914112),
    ];

    fn up_ours(which: u8, value: u64) -> u128 {
        match which {
            0 => quad_from_integer(
                if (value as i32) < 0 { SIGN } else { 0 },
                u64::from((value as i32).unsigned_abs()),
            ),
            1 => quad_from_integer(0, u64::from(value as u32)),
            2 => quad_from_integer(
                if (value as i64) < 0 { SIGN } else { 0 },
                (value as i64).unsigned_abs(),
            ),
            _ => quad_from_integer(0, value),
        }
    }

    #[test]
    fn the_quads_libgcc_makes_of_an_integer_come_out_bit_for_bit() {
        for (value, which, wanted) in LIBGCC_UP {
            assert_eq!(up_ours(*which, *value), *wanted, "routine {which} on 0x{value:016x}");
        }
    }

    #[test]
    fn the_integers_libgcc_truncates_a_quad_to_come_out_the_same() {
        for (bits, signed_32, unsigned_32, signed_64, unsigned_64) in LIBGCC_DOWN {
            assert_eq!(
                truncate(*bits, SIGNED_32).map_or(0, |answer| answer as i32),
                *signed_32,
                "int of {bits:032x}"
            );
            assert_eq!(
                truncate_unsigned(*bits, UNSIGNED_32).map_or(0, |answer| answer as u32),
                *unsigned_32,
                "unsigned of {bits:032x}"
            );
            assert_eq!(
                truncate(*bits, SIGNED_64).map_or(0, |answer| answer as i64),
                *signed_64,
                "long long of {bits:032x}"
            );
            assert_eq!(
                truncate_unsigned(*bits, UNSIGNED_64).map_or(0, |answer| answer as u64),
                *unsigned_64,
                "unsigned long long of {bits:032x}"
            );
        }
        for (bits, signed_32, signed_64) in LIBGCC_DOWN_NEGATIVE {
            assert_eq!(
                truncate(*bits, SIGNED_32).map_or(0, |answer| answer as i32),
                *signed_32,
                "int of {bits:032x}"
            );
            assert_eq!(
                truncate(*bits, SIGNED_64).map_or(0, |answer| answer as i64),
                *signed_64,
                "long long of {bits:032x}"
            );
        }
        for (bits, signed_64, unsigned_64) in LIBGCC_DOWN_WIDE {
            assert_eq!(
                truncate(*bits, SIGNED_64).map_or(0, |answer| answer as i64),
                *signed_64,
                "long long of {bits:032x}"
            );
            assert_eq!(
                truncate_unsigned(*bits, UNSIGNED_64).map_or(0, |answer| answer as u64),
                *unsigned_64,
                "unsigned long long of {bits:032x}"
            );
        }
    }

    #[test]
    fn every_integer_converts_up_exactly_and_comes_back() {
        // The property that is this format's own: a hundred and thirteen bits hold every sixty four
        // bit integer, so a conversion up never rounds and the value that comes back down is the one
        // that went up, for every input rather than for the small ones.
        let mut stream = Stream(0x5851_f42d_4c95_7f2d);
        for _ in 0..20_000 {
            let value = stream.next();
            let signed = value as i64;
            let unsigned = quad_from_integer(0, value);
            assert_eq!(truncate_unsigned(unsigned, UNSIGNED_64), Some(u128::from(value)));
            let magnitude = signed.unsigned_abs();
            let quad = quad_from_integer(if signed < 0 { SIGN } else { 0 }, magnitude);
            assert_eq!(truncate(quad, SIGNED_64), Some(i128::from(signed)));
            // And the same value one word narrower, which is where the thirty two bit routines live.
            let narrow = (value >> 32) as u32;
            assert_eq!(
                truncate_unsigned(quad_from_integer(0, u64::from(narrow)), UNSIGNED_32),
                Some(u128::from(narrow))
            );
            let narrow = narrow as i32;
            let quad = quad_from_integer(
                if narrow < 0 { SIGN } else { 0 },
                u64::from(narrow.unsigned_abs()),
            );
            assert_eq!(truncate(quad, SIGNED_32), Some(i128::from(narrow)));
        }
    }

    #[test]
    fn a_quad_is_the_exact_integer_and_not_a_rounding_of_it() {
        // Said directly rather than through the round trip: the quad an integer becomes has that
        // integer as its exact value, which is what `exact_of` reads back out of the pattern.
        let mut stream = Stream(0x1405_7b7e_f767_814f);
        for _ in 0..20_000 {
            let value = stream.next();
            let (significand, scale) = exact_of(quad_from_integer(0, value));
            // A significand is a hundred and thirteen bits and the integer is at most sixty four, so
            // the scale is negative and the integer sits in the top of the significand. Exact means
            // nothing was dropped on the way, which is every bit below the point being zero.
            let below = (-scale) as u32;
            assert_eq!(significand & ((1 << below) - 1), 0, "0x{value:016x} lost bits");
            assert_eq!(significand >> below, u128::from(value), "0x{value:016x}");
        }
    }

    #[test]
    fn each_type_is_held_to_its_own_bound() {
        // The edges by name, which is where an off by one lives. A signed type holds one more going
        // down than going up, and every one of these bounds is a quad exactly at this width, so the
        // comparison is between two integers and the answers below are arithmetic rather than a
        // convention.
        let largest_int = quad_from_integer(0, 2_147_483_647);
        assert_eq!(truncate(largest_int, SIGNED_32), Some(2_147_483_647));
        let past_int = quad_from_integer(0, 2_147_483_648);
        assert_eq!(truncate(past_int, SIGNED_32), None);
        let most_negative_int = quad_from_integer(SIGN, 2_147_483_648);
        assert_eq!(truncate(most_negative_int, SIGNED_32), Some(-2_147_483_648));
        let past_it = quad_from_integer(SIGN, 2_147_483_649);
        assert_eq!(truncate(past_it, SIGNED_32), None);
        let largest_unsigned = quad_from_integer(0, 4_294_967_295);
        assert_eq!(truncate_unsigned(largest_unsigned, UNSIGNED_32), Some(4_294_967_295));
        assert_eq!(truncate_unsigned(quad_from_integer(0, 4_294_967_296), UNSIGNED_32), None);
        let largest_long = quad_from_integer(0, i64::MAX as u64);
        assert_eq!(truncate(largest_long, SIGNED_64), Some(i128::from(i64::MAX)));
        assert_eq!(truncate(quad_from_integer(0, 1 << 63), SIGNED_64), None);
        assert_eq!(
            truncate(quad_from_integer(SIGN, 1 << 63), SIGNED_64),
            Some(i128::from(i64::MIN))
        );
        assert_eq!(
            truncate_unsigned(quad_from_integer(0, u64::MAX), UNSIGNED_64),
            Some(u128::from(u64::MAX))
        );
        // Two to the sixty four, which is the first value past the widest type and is not an integer
        // any of the four hold. It cannot be asked for through `quad_from_integer`, so it is built
        // the way every other value in this file is, out of a significand and a scale.
        let past_everything = round_from(0, Wide::narrow(1), 64, false);
        assert_eq!(truncate(past_everything, SIGNED_64), None);
        assert_eq!(truncate_unsigned(past_everything, UNSIGNED_64), None);
        // And a value between two integers, where the answer is the truncation and the fraction is
        // dropped rather than rounded, in both directions of sign.
        let three_halves = round_from(0, Wide::narrow(3), -1, false);
        assert_eq!(truncate(three_halves, SIGNED_32), Some(1));
        assert_eq!(truncate(three_halves | SIGN, SIGNED_32), Some(-1));
        assert_eq!(truncate_unsigned(three_halves, UNSIGNED_32), Some(1));
        assert_eq!(truncate_unsigned(three_halves | SIGN, UNSIGNED_32), None);
    }

    #[test]
    fn a_value_with_no_integer_in_it_answers_the_convention() {
        // The four cases C leaves undefined, which these answer with a zero that section 12.8 records
        // as a convention both implementations keep rather than as a promise to a program.
        for bits in
            [infinity(0), infinity(SIGN), EMPTY_NAN, EMPTY_NAN | SIGN, (TOP << FRACTION) | 1]
        {
            assert_eq!(truncate(bits, SIGNED_32).map_or(0, |answer| answer as i32), 0);
            assert_eq!(truncate(bits, SIGNED_64).map_or(0, |answer| answer as i64), 0);
            assert_eq!(truncate_unsigned(bits, UNSIGNED_32).map_or(0, |answer| answer as u32), 0);
            assert_eq!(truncate_unsigned(bits, UNSIGNED_64).map_or(0, |answer| answer as u64), 0);
        }
        // A subnormal and a value below one, where the answer is zero because that is the value and
        // not because there is nothing to hand back.
        for bits in [1u128, SIGN | 1, round_from(0, Wide::narrow(1), -1, false)] {
            assert_eq!(truncate(bits, SIGNED_64), Some(0));
            assert_eq!(truncate_unsigned(bits, UNSIGNED_64), Some(0));
        }
        // A negative value with an integer part, which the two unsigned routines refuse and the two
        // signed ones answer.
        let minus_one = quad_from_integer(SIGN, 1);
        assert_eq!(truncate(minus_one, SIGNED_32), Some(-1));
        assert_eq!(truncate_unsigned(minus_one, UNSIGNED_32), None);
        assert_eq!(truncate_unsigned(minus_one, UNSIGNED_64), None);
    }

    #[test]
    fn the_width_written_by_hand_does_what_the_language_does_at_half_of_it() {
        // `Wide` is the one piece of this file with no counterpart in the formats below, so it is
        // held to `u128` over the range where the two agree.
        let mut stream = Stream(0xdead_beef_0bad_f00d);
        for _ in 0..20_000 {
            let left = u128::from(stream.next());
            let right = u128::from(stream.next());
            assert_eq!(Wide::narrow(left).plus(Wide::narrow(right)).low, left + right);
            assert_eq!(Wide::narrow(left).minus(Wide::narrow(right)).low, left.wrapping_sub(right));
            assert_eq!(wide_product(left, right).low, left * right);
            assert_eq!(wide_product(left, right).high, 0);
            assert_eq!(Wide::narrow(left).bits(), 128 - left.leading_zeros());
            let by = (stream.next() % 64) as u32;
            assert_eq!(Wide::narrow(left).shifted_up(by).low, left << by);
            assert_eq!(Wide::narrow(left).shifted_down(by).low, left >> by);
            assert_eq!(Wide::narrow(left).low_bits(by), Wide::narrow(left & ((1 << by) - 1)));
            // And across the boundary between the halves, which is the part `u128` cannot check.
            let wide = Wide { high: left, low: right };
            assert_eq!(wide.shifted_up(128), Wide { high: right, low: 0 });
            assert_eq!(wide.shifted_down(128), Wide { high: 0, low: left });
            assert_eq!(wide.bits(), 256 - left.leading_zeros());
            assert_eq!(wide.plus(Wide::ZERO), wide);
            assert_eq!(wide.minus(wide), Wide::ZERO);
        }
        // The bit either side of the boundary, which is where a shift, a carry and a borrow all have
        // to cross it.
        let top = Wide::narrow(1 << 127);
        assert_eq!(top.shifted_up(1), Wide { high: 1, low: 0 });
        assert_eq!(Wide { high: 1, low: 0 }.shifted_down(1), top);
        assert_eq!(top.plus(top), Wide { high: 1, low: 0 });
        assert_eq!(Wide { high: 1, low: 0 }.minus(Wide::narrow(1)), Wide::narrow(u128::MAX));
        assert_eq!(Wide::narrow(u128::MAX).plus(Wide::narrow(1)), Wide { high: 1, low: 0 });
        assert_eq!(wide_product(u128::MAX, 1), Wide::narrow(u128::MAX));
        assert_eq!(wide_product(1 << 112, 1 << 112), Wide { high: 1 << 96, low: 0 });
    }
}
