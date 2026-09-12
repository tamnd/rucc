//! Single precision arithmetic in integers, which is the reference for the five entry points in
//! `runtime/builtins/float.c`: the four operations a target with no floating point unit calls, and
//! the negation.
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
//! It comes back quieted, and the left one where both operands are one. That is libgcc's
//! convention, it is what this machine's own arithmetic does with the operands in the same order,
//! and it is what the C does, so the two sides can be compared bit for bit instead of through a
//! canonicalization that would hide a real disagreement. An operation with no answer at all, an
//! infinity less an infinity, a zero times an infinity, or a zero over a zero, gives the quiet not
//! a number with an empty payload.

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

/// `float __subsf3(float, float)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __subsf3(left: f32, right: f32) -> f32 {
    add(left.to_bits(), right.to_bits() ^ SIGN)
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
            1 => add(left.to_bits(), right.to_bits() ^ SIGN),
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
}
