//! Arithmetic on [`Float`], correctly rounded, in integer operations only.
//!
//! The constant evaluator folds `1.0 / 3.0` at translation time and the program it compiles
//! computes the same thing at run time, and the two have to agree in the last bit. Asking the
//! host to do the arithmetic gets that wrong in three separate ways: the host may not have the
//! format at all, its `long double` is not the target's, and a compiler that folds one way on one
//! machine and another way on another is a compiler whose output depends on where it ran.
//!
//! So the operations are here, on integers, rounded to nearest with ties to even. Each one
//! computes the exact answer to more bits than the format has and rounds once, which is what
//! makes it correctly rounded: the answer is the representable number nearest the exact result.
//! Every operation below either fits that exact result in a [`u128`] or keeps a sticky bit saying
//! that something nonzero was dropped below the bits it kept, which is all the rounding needs to
//! know about what it cannot see.
//!
//! # Naming
//!
//! An operation returns a number and a [`Status`], because the caller has to be able to warn that
//! a constant overflowed or that a fold was inexact, so these cannot be the `std::ops` traits and
//! are named for what they return rather than for what they do. [`Float`] deliberately implements
//! no arithmetic trait at all: an operator with a discarded status is exactly the kind of quiet
//! wrongness this module exists to prevent.
//!
//! # What is not here
//!
//! A rounding mode other than to nearest. C's `#pragma STDC FENV_ACCESS` and the dynamic rounding
//! modes change what the running program does rather than what a translation time constant means,
//! and a constant is folded to nearest whatever the mode is.
//!
//! A nan payload out of an operation. [`Float`] carries one, since `__builtin_nan` can spell one
//! and a static initializer written with it has to keep it, but every nan produced here is the
//! default quiet one. Propagating a payload would mean deciding which of two operands wins, which
//! IEEE 754 leaves to the implementation and which no C program can see.

use std::cmp::Ordering;

use crate::float::{Category, Float, Format, Status, round};

/// How many bits are kept below the significand while two numbers are lined up for an addition.
///
/// Two of them are the guard and round bits an addition needs in order to round correctly, and
/// the third is where the sticky bit lands, so anything shifted past all three is nonzero or it
/// is nothing, which is the one fact the rounding needs about it.
const GUARD: u32 = 3;

impl Float {
    /// A quiet nan, which is what an operation with no answer gives.
    ///
    /// The default one, whose payload is nothing. [`Float::nan_with`] is where a payload comes
    /// from, and there is only one thing in C that can spell one.
    #[must_use]
    pub const fn nan(format: Format) -> Float {
        Float {
            format,
            category: Category::Nan,
            sign: false,
            exponent: 0,
            significand: Float::quiet_bit(format) | Float::leading_bit(format),
        }
    }

    /// Whether the number is a nan.
    #[must_use]
    pub const fn is_nan(self) -> bool {
        matches!(self.category, Category::Nan)
    }

    /// The number with its sign flipped, which is exact and which a zero and a nan both have.
    #[must_use]
    pub const fn negated(self) -> Float {
        Float { sign: !self.sign, ..self }
    }

    /// The number without its sign, which is exact.
    #[must_use]
    pub const fn abs(self) -> Float {
        Float { sign: false, ..self }
    }

    /// The number with the sign given, which is exact and which is what `copysign` answers. Every
    /// value has a sign to be set, a zero and a nan as much as a number.
    #[must_use]
    pub const fn with_sign(self, sign: bool) -> Float {
        Float { sign, ..self }
    }

    /// `self + other`, rounded to nearest with ties to even.
    ///
    /// A nan operand gives a nan and nothing else. Two infinities of opposite sign give a nan and
    /// [`Status::INVALID`], because the answer depends on how they got there. Two zeros give a
    /// negative zero only when both of them are negative, which is the round to nearest rule and
    /// the reason `x + 0.0` is not a way to drop a sign.
    ///
    /// # Panics
    ///
    /// If the two numbers are not in the same format. The usual arithmetic conversions have
    /// already made them so, and converting here would be a conversion nobody asked for.
    #[must_use]
    pub fn sum(self, other: Float) -> (Float, Status) {
        self.total(other, false)
    }

    /// `self - other`, rounded to nearest with ties to even.
    ///
    /// This is the sum of `self` and the negation of `other`, which is exactly what it is in IEEE
    /// 754, so a subtraction that cancels completely gives a positive zero and an infinity minus
    /// itself gives a nan.
    ///
    /// # Panics
    ///
    /// If the two numbers are not in the same format.
    #[must_use]
    pub fn difference(self, other: Float) -> (Float, Status) {
        self.total(other, true)
    }

    /// `self * other`, rounded to nearest with ties to even.
    ///
    /// A zero times an infinity gives a nan and [`Status::INVALID`]. The sign is the two signs
    /// multiplied, which a zero and a nan have as much as any other number does.
    ///
    /// # Panics
    ///
    /// If the two numbers are not in the same format.
    #[must_use]
    pub fn product(self, other: Float) -> (Float, Status) {
        let format = self.agreed_format(other);
        let sign = self.sign != other.sign;
        if let Some(nan) = Float::propagated_nan(self, other) {
            return nan;
        }
        match (self.category, other.category) {
            (Category::Infinite, Category::Zero) | (Category::Zero, Category::Infinite) => {
                (Float::nan(format), Status::INVALID)
            }
            (Category::Infinite, _) | (_, Category::Infinite) => {
                (Float::infinity(format, sign), Status::NONE)
            }
            (Category::Zero, _) | (_, Category::Zero) => (Float::zero(format, sign), Status::NONE),
            _ => {
                let (left, left_exponent) = self.parts();
                let (right, right_exponent) = other.parts();
                let (high, low) = wide_multiply(left, right);
                let exponent = left_exponent + right_exponent;
                if high == 0 {
                    return round(low, exponent, false, sign, format);
                }
                // Two significands of at most a hundred and thirteen bits make a product of at
                // most two hundred and twenty six, so the count below is between one and ninety
                // eight and every shift here has somewhere to go.
                let drop = 128 - high.leading_zeros();
                let sticky = low & ((1u128 << drop) - 1) != 0;
                let significand = (high << (128 - drop)) | (low >> drop);
                round(significand, exponent + drop as i32, sticky, sign, format)
            }
        }
    }

    /// `self / other`, rounded to nearest with ties to even.
    ///
    /// A finite number divided by zero gives an infinity and [`Status::DIVIDE_BY_ZERO`]. Zero
    /// divided by zero and an infinity divided by an infinity both give a nan and
    /// [`Status::INVALID`], which is the difference between a division that has no answer and one
    /// whose answer is only too large to be a number.
    ///
    /// # Panics
    ///
    /// If the two numbers are not in the same format.
    #[must_use]
    pub fn quotient(self, other: Float) -> (Float, Status) {
        let format = self.agreed_format(other);
        let sign = self.sign != other.sign;
        if let Some(nan) = Float::propagated_nan(self, other) {
            return nan;
        }
        match (self.category, other.category) {
            (Category::Infinite, Category::Infinite) | (Category::Zero, Category::Zero) => {
                (Float::nan(format), Status::INVALID)
            }
            (Category::Infinite, _) => (Float::infinity(format, sign), Status::NONE),
            (_, Category::Infinite) | (Category::Zero, _) => {
                (Float::zero(format, sign), Status::NONE)
            }
            (_, Category::Zero) => (Float::infinity(format, sign), Status::DIVIDE_BY_ZERO),
            _ => {
                // Both significands are shifted up until their leading bit is the top bit of a
                // `u128`, which puts their quotient between a half and two and so puts its
                // leading bit in a known place. The quotient is then taken to two bits more than
                // the format has, and whatever is left over is the sticky bit.
                let (left, left_exponent) = self.parts();
                let (right, right_exponent) = other.parts();
                let (left_shift, right_shift) = (left.leading_zeros(), right.leading_zeros());
                let extra = format.precision() + 2;
                let numerator = left << left_shift;
                let (quotient, remainder) = long_divide(numerator, right << right_shift, extra);
                let exponent = (left_exponent - left_shift as i32)
                    - (right_exponent - right_shift as i32)
                    - extra as i32;
                round(quotient, exponent, remainder != 0, sign, format)
            }
        }
    }

    /// How the two compare, or [`None`] if either is a nan and they do not compare at all.
    ///
    /// This is the comparison C's relational operators do, so a positive zero and a negative zero
    /// are equal and the unordered case is the one that makes `x < y` and `!(x >= y)` different
    /// questions.
    ///
    /// # Panics
    ///
    /// If the two numbers are not in the same format.
    #[must_use]
    pub fn compare(self, other: Float) -> Option<Ordering> {
        self.agreed_format(other);
        if self.is_nan() || other.is_nan() {
            return None;
        }
        if self.is_zero() && other.is_zero() {
            return Some(Ordering::Equal);
        }
        if self.sign != other.sign {
            return Some(if self.sign { Ordering::Less } else { Ordering::Greater });
        }
        let magnitudes = self.compare_magnitude(other);
        Some(if self.sign { magnitudes.reverse() } else { magnitudes })
    }

    /// The nearest number to this one in another format, rounded to nearest with ties to even.
    ///
    /// Widening is exact for every pair of formats here except a `__bf16` widened to a
    /// `_Float16`, which has more precision and less range. Narrowing is what a cast does, and it
    /// reports what it had to do to make the number fit.
    #[must_use]
    pub fn to_format(self, format: Format) -> (Float, Status) {
        match self.category {
            Category::Nan => (Float { sign: self.sign, ..Float::nan(format) }, Status::NONE),
            Category::Infinite => (Float::infinity(format, self.sign), Status::NONE),
            Category::Zero => (Float::zero(format, self.sign), Status::NONE),
            Category::Finite => {
                let (significand, exponent) = self.parts();
                round(significand, exponent, false, self.sign, format)
            }
        }
    }

    /// The nearest number in `format` to a signed integer.
    #[must_use]
    pub fn from_signed(value: i128, format: Format) -> (Float, Status) {
        if value == 0 {
            return (Float::zero(format, false), Status::NONE);
        }
        round(value.unsigned_abs(), 0, false, value < 0, format)
    }

    /// The nearest number in `format` to an unsigned integer.
    #[must_use]
    pub fn from_unsigned(value: u128, format: Format) -> (Float, Status) {
        if value == 0 {
            return (Float::zero(format, false), Status::NONE);
        }
        round(value, 0, false, false, format)
    }

    /// The number truncated toward zero into an integer of `width` bits.
    ///
    /// What comes back is what an integer constant is stored as, which is the value sign extended
    /// out of the type it has, so an unsigned conversion of a hundred and twenty eight bits comes
    /// back with its top bit in the sign of the [`i128`].
    ///
    /// Converting a number that does not fit is undefined behaviour in C rather than a value, so
    /// what comes back is the nearest end of the range together with [`Status::INVALID`], which
    /// is what the caller warns about. A nan comes back as zero, for the same reason and with the
    /// same flag. Dropping a fraction is [`Status::INEXACT`] and nothing worse, since that is the
    /// conversion doing what it is for.
    ///
    /// # Panics
    ///
    /// If `width` is zero or wider than a hundred and twenty eight bits.
    #[must_use]
    pub fn to_integer(self, width: u32, signed: bool) -> (i128, Status) {
        assert!(width > 0 && width <= 128, "an integer type of {width} bits");
        let limit = self.limit(width, signed);
        match self.category {
            Category::Nan => (0, Status::INVALID),
            Category::Infinite => (self.signed_value(limit), Status::INVALID),
            Category::Zero => (0, Status::NONE),
            Category::Finite => {
                let (significand, exponent) = self.parts();
                let (magnitude, inexact) = if exponent >= 0 {
                    if exponent > significand.leading_zeros() as i32 {
                        return (self.signed_value(limit), Status::INVALID);
                    }
                    (significand << exponent, false)
                } else if -exponent >= 128 {
                    (0, true)
                } else {
                    let dropped = -exponent as u32;
                    (significand >> dropped, significand & ((1u128 << dropped) - 1) != 0)
                };
                if magnitude > limit {
                    return (self.signed_value(limit), Status::INVALID);
                }
                let status = if inexact { Status::INEXACT } else { Status::NONE };
                (self.signed_value(magnitude), status)
            }
        }
    }

    /// The largest magnitude an integer of this type can hold with this number's sign.
    fn limit(self, width: u32, signed: bool) -> u128 {
        match (signed, self.sign) {
            (true, true) => 1u128 << (width - 1),
            (true, false) => (1u128 << (width - 1)) - 1,
            // An unsigned type has nowhere for a negative number to go, but truncating one whose
            // magnitude is below one lands on zero, which is in range and is not an error.
            (false, true) => 0,
            (false, false) => u128::MAX >> (128 - width),
        }
    }

    /// A magnitude given this number's sign, as an integer constant is stored.
    fn signed_value(self, magnitude: u128) -> i128 {
        if self.sign { (magnitude as i128).wrapping_neg() } else { magnitude as i128 }
    }

    /// The significand and the power of two it is scaled by, so that the value of a finite number
    /// is the first of these shifted by the second.
    fn parts(self) -> (u128, i32) {
        (self.significand, self.exponent - self.format.precision() as i32 + 1)
    }

    /// The format both numbers are in.
    ///
    /// # Panics
    ///
    /// If they are not in the same one. Every operation here is on two numbers of one type,
    /// because the usual arithmetic conversions ran first, and converting one here instead would
    /// silently round an operand on the way in.
    fn agreed_format(self, other: Float) -> Format {
        assert_eq!(self.format, other.format, "an operation on two floating formats at once");
        self.format
    }

    /// The nan an operation gives when an operand is one, if either is.
    fn propagated_nan(left: Float, right: Float) -> Option<(Float, Status)> {
        (left.is_nan() || right.is_nan()).then(|| (Float::nan(left.format), Status::NONE))
    }

    /// How the magnitudes of two numbers in the same format compare, nans aside.
    ///
    /// Comparing the exponent before the significand works across the subnormals as well as the
    /// normals, because a subnormal has the smallest exponent there is and a leading zero where a
    /// normal number has its leading one.
    fn compare_magnitude(self, other: Float) -> Ordering {
        match (self.category, other.category) {
            (Category::Zero, Category::Zero) | (Category::Infinite, Category::Infinite) => {
                Ordering::Equal
            }
            (Category::Zero, _) | (_, Category::Infinite) => Ordering::Less,
            (Category::Infinite, _) | (_, Category::Zero) => Ordering::Greater,
            _ => (self.exponent, self.significand).cmp(&(other.exponent, other.significand)),
        }
    }

    /// The sum of two numbers, or their difference, which is the sum of one of them and the other
    /// negated and is not a separate operation anywhere below this line.
    fn total(self, other: Float, subtract: bool) -> (Float, Status) {
        let format = self.agreed_format(other);
        let other = if subtract { other.negated() } else { other };
        if let Some(nan) = Float::propagated_nan(self, other) {
            return nan;
        }
        match (self.category, other.category) {
            (Category::Infinite, Category::Infinite) => {
                if self.sign == other.sign {
                    (self, Status::NONE)
                } else {
                    (Float::nan(format), Status::INVALID)
                }
            }
            (Category::Infinite, _) => (self, Status::NONE),
            (_, Category::Infinite) => (other, Status::NONE),
            // Round to nearest makes the sum of two zeros positive unless both of them were
            // negative, which is the one rule here that is about the sign rather than the value.
            (Category::Zero, Category::Zero) => {
                (Float::zero(format, self.sign && other.sign), Status::NONE)
            }
            (Category::Zero, _) => (other, Status::NONE),
            (_, Category::Zero) => (self, Status::NONE),
            _ => {
                let (big, small) = if self.compare_magnitude(other) == Ordering::Less {
                    (other, self)
                } else {
                    (self, other)
                };
                let (left, exponent) = big.parts();
                let (right, small_exponent) = small.parts();
                let distance = (exponent - small_exponent) as u32;
                let left = left << GUARD;
                let (mut right, sticky) = if distance <= GUARD {
                    (right << (GUARD - distance), false)
                } else if distance - GUARD >= 128 {
                    (0, true)
                } else {
                    let dropped = distance - GUARD;
                    (right >> dropped, right & ((1u128 << dropped) - 1) != 0)
                };
                let exponent = exponent - GUARD as i32;
                if big.sign == small.sign {
                    return round(left + right, exponent, sticky, big.sign, format);
                }
                // What was dropped belongs to the number being taken away, so the answer is a
                // little below what the bits that are left say it is. Taking one more off, with
                // the sticky bit set, says exactly that: the answer is between the two, which is
                // all the rounding needs. It cannot go below zero, because the two are ordered by
                // magnitude and a dropped bit means the smaller one is smaller by more than the
                // last bit of the larger.
                right += u128::from(sticky);
                if left == right {
                    return (Float::zero(format, false), Status::NONE);
                }
                round(left - right, exponent, sticky, big.sign, format)
            }
        }
    }
}

/// The full two hundred and fifty six bit product of two numbers, high half first.
///
/// The halves of each operand multiply into products that fit, and the middle column is the one
/// that has to be carried by hand. There is no `u256` and no widening multiply in the language,
/// so this is what multiplying two significands looks like.
fn wide_multiply(left: u128, right: u128) -> (u128, u128) {
    const LOW: u128 = u64::MAX as u128;
    let (left_low, left_high) = (left & LOW, left >> 64);
    let (right_low, right_high) = (right & LOW, right >> 64);
    let low = left_low * right_low;
    let first = left_low * right_high;
    let second = left_high * right_low;
    let middle = (low >> 64) + (first & LOW) + (second & LOW);
    let high = left_high * right_high + (first >> 64) + (second >> 64) + (middle >> 64);
    (high, (middle << 64) | (low & LOW))
}

/// The quotient of `numerator` shifted up by `extra` bits and `divisor`, and what is left over.
///
/// Both arguments have their top bit set, so their quotient is between a half and two and the
/// answer here has either `extra` or `extra` plus one bits. Restoring division a bit at a time,
/// because the alternatives are a longer program and this runs once per constant folded.
fn long_divide(numerator: u128, divisor: u128, extra: u32) -> (u128, u128) {
    let mut remainder = 0u128;
    let mut quotient = 0u128;
    for step in 0..128 + extra {
        let bit = if step < 128 { (numerator >> (127 - step)) & 1 } else { 0 };
        // The remainder is below the divisor, so doubling it can carry out of the top of a `u128`
        // and still be a number the divisor goes into exactly once.
        let carry = remainder >> 127 == 1;
        remainder = (remainder << 1) | bit;
        quotient <<= 1;
        if carry || remainder >= divisor {
            remainder = remainder.wrapping_sub(divisor);
            quotient |= 1;
        }
    }
    (quotient, remainder)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `double` from the host's bits, which is what makes the host an oracle.
    fn double(value: f64) -> Float {
        Float::from_bits(Format::Double, u128::from(value.to_bits()))
    }

    /// The host number a `double` holds.
    fn host(value: Float) -> f64 {
        f64::from_bits(value.to_bits() as u64)
    }

    fn single(value: f32) -> Float {
        Float::from_bits(Format::Single, u128::from(value.to_bits()))
    }

    fn host_single(value: Float) -> f32 {
        f32::from_bits(value.to_bits() as u32)
    }

    /// A fixed sequence, so that a failure names the same numbers on every machine.
    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *state
    }

    /// Every operation on a pair of `double` values, against what the host computes.
    fn agrees(left: f64, right: f64) {
        let (a, b) = (double(left), double(right));
        for (name, mine, theirs) in [
            ("+", a.sum(b).0, left + right),
            ("-", a.difference(b).0, left - right),
            ("*", a.product(b).0, left * right),
            ("/", a.quotient(b).0, left / right),
        ] {
            if theirs.is_nan() {
                assert!(mine.is_nan(), "{left:e} {name} {right:e} gave {}", host(mine));
            } else {
                assert_eq!(
                    host(mine).to_bits(),
                    theirs.to_bits(),
                    "{left:e} {name} {right:e} gave {} not {theirs:e}",
                    host(mine)
                );
            }
        }
    }

    /// The same, for a pair of `float` values.
    fn agrees_single(left: f32, right: f32) {
        let (a, b) = (single(left), single(right));
        for (name, mine, theirs) in [
            ("+", a.sum(b).0, left + right),
            ("-", a.difference(b).0, left - right),
            ("*", a.product(b).0, left * right),
            ("/", a.quotient(b).0, left / right),
        ] {
            if theirs.is_nan() {
                assert!(mine.is_nan(), "{left:e} {name} {right:e}");
            } else {
                assert_eq!(
                    host_single(mine).to_bits(),
                    theirs.to_bits(),
                    "{left:e} {name} {right:e} gave {} not {theirs:e}",
                    host_single(mine)
                );
            }
        }
    }

    #[test]
    fn the_ordinary_sums_are_the_ones_the_host_computes() {
        for (left, right) in [
            (1.0, 1.0),
            (1.0, 2.0),
            (0.1, 0.2),
            (1.0, -1.0),
            (1e308, 1e308),
            (1.0, 1e-308),
            (3.0, 7.0),
            (1.0, 3.0),
            (2.5, 0.5),
            (1e-320, 1e-320),
            (f64::MAX, f64::MIN),
        ] {
            agrees(left, right);
            agrees(right, left);
            agrees(-left, right);
            agrees(left, -right);
        }
    }

    #[test]
    fn a_sweep_of_random_doubles_agrees_with_the_host_in_every_bit() {
        // Random bits cover the infinities, the nans and the subnormals as well as the ordinary
        // numbers, which is the point of taking bits rather than taking values.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..20_000 {
            agrees(f64::from_bits(next(&mut state)), f64::from_bits(next(&mut state)));
        }
    }

    #[test]
    fn a_sweep_of_random_floats_agrees_with_the_host_in_every_bit() {
        let mut state = 0x1234_5678_9abc_def0u64;
        for _ in 0..20_000 {
            let bits = next(&mut state);
            agrees_single(f32::from_bits(bits as u32), f32::from_bits((bits >> 32) as u32));
        }
    }

    #[test]
    fn a_sweep_of_numbers_close_together_agrees_too() {
        // Two numbers of nearly the same size are where a subtraction cancels and where the bits
        // that are left come from the guard bits rather than from either operand.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..20_000 {
            let left = (next(&mut state) >> 11) as f64;
            let scale = f64::from(next(&mut state) as u32 % 8) - 4.0;
            let right = (next(&mut state) >> 11) as f64 * scale.exp2();
            agrees(left, right);
            agrees(left, left);
            agrees(left, -left);
        }
    }

    #[test]
    fn the_operations_with_no_answer_say_so() {
        let (infinity, zero) = (Float::infinity(Format::Double, false), double(0.0));
        let (one, nan) = (double(1.0), Float::nan(Format::Double));

        let (value, status) = infinity.difference(infinity);
        assert!(value.is_nan() && status.has(Status::INVALID));
        let (value, status) = infinity.product(zero);
        assert!(value.is_nan() && status.has(Status::INVALID));
        let (value, status) = zero.quotient(zero);
        assert!(value.is_nan() && status.has(Status::INVALID));
        let (value, status) = infinity.quotient(infinity);
        assert!(value.is_nan() && status.has(Status::INVALID));

        // A division by zero has an answer, which is why it is not the same flag.
        let (value, status) = one.quotient(zero);
        assert!(value.is_infinite() && !value.is_negative());
        assert!(status.has(Status::DIVIDE_BY_ZERO) && !status.has(Status::INVALID));
        assert!(one.negated().quotient(zero).0.is_negative());
        assert!(one.quotient(zero.negated()).0.is_negative());

        // A nan on the way in is a nan on the way out, and nothing is reported for it.
        for (value, status) in
            [nan.sum(one), one.sum(nan), nan.product(one), nan.quotient(one), one.difference(nan)]
        {
            assert!(value.is_nan() && status.is_none());
        }
        assert!(infinity.sum(infinity).0.is_infinite());
        assert!(infinity.sum(one).0.is_infinite());
    }

    #[test]
    fn the_sign_of_a_zero_is_the_one_the_host_gives() {
        let (positive, negative) = (double(0.0), double(-0.0));
        for (mine, theirs) in [
            (positive.sum(positive), 0.0 + 0.0),
            (positive.sum(negative), 0.0 + -0.0),
            (negative.sum(positive), -0.0 + 0.0),
            (negative.sum(negative), -0.0 + -0.0),
            (positive.difference(positive), 0.0 - 0.0),
            (negative.difference(positive), -0.0 - 0.0),
            (double(1.0).difference(double(1.0)), 1.0 - 1.0),
            (double(-1.0).sum(double(1.0)), -1.0 + 1.0),
            (positive.product(double(3.0)), 0.0 * 3.0),
            (negative.product(double(3.0)), -0.0 * 3.0),
            (positive.quotient(double(-3.0)), 0.0 / -3.0),
        ] {
            assert_eq!(host(mine.0).to_bits(), f64::to_bits(theirs), "{theirs}");
        }
    }

    #[test]
    fn an_operation_says_what_it_had_to_do_to_the_answer() {
        let (one, three) = (double(1.0), double(3.0));
        assert!(one.sum(one).1.is_none());
        assert!(one.product(three).1.is_none());
        assert!(one.quotient(double(2.0)).1.is_none());
        assert!(one.quotient(three).1.has(Status::INEXACT));

        let (value, status) = double(f64::MAX).product(double(2.0));
        assert!(value.is_infinite() && status.has(Status::OVERFLOW) && status.has(Status::INEXACT));
        let (value, status) = double(f64::MIN_POSITIVE).quotient(double(1e300));
        assert!(value.is_zero() && status.has(Status::UNDERFLOW) && status.has(Status::INEXACT));
        // A subnormal answer that lost no bits is exact, small as it is.
        let four = Float::from_bits(Format::Double, 4);
        assert!(four.quotient(double(2.0)).1.is_none());
        assert!(four.quotient(double(4.0)).1.is_none());
        // One that lost a bit is inexact and underflowed, both.
        let status = Float::from_bits(Format::Double, 3).quotient(double(2.0)).1;
        assert!(status.has(Status::INEXACT) && status.has(Status::UNDERFLOW));
    }

    #[test]
    fn a_comparison_orders_the_numbers_and_leaves_the_nans_out() {
        let (one, two) = (double(1.0), double(2.0));
        assert_eq!(one.compare(two), Some(Ordering::Less));
        assert_eq!(two.compare(one), Some(Ordering::Greater));
        assert_eq!(one.compare(one), Some(Ordering::Equal));
        assert_eq!(one.negated().compare(two.negated()), Some(Ordering::Greater));
        assert_eq!(one.negated().compare(one), Some(Ordering::Less));
        // The two zeros are the same number as far as a comparison is concerned.
        assert_eq!(double(0.0).compare(double(-0.0)), Some(Ordering::Equal));
        assert_eq!(double(-0.0).compare(double(0.0)), Some(Ordering::Equal));
        assert_eq!(double(-0.0).compare(one), Some(Ordering::Less));
        // An infinity is at the end of the order, and a nan is not in the order at all.
        let infinity = Float::infinity(Format::Double, false);
        assert_eq!(infinity.compare(double(f64::MAX)), Some(Ordering::Greater));
        assert_eq!(infinity.negated().compare(double(f64::MIN)), Some(Ordering::Less));
        assert_eq!(infinity.compare(infinity), Some(Ordering::Equal));
        let nan = Float::nan(Format::Double);
        assert_eq!(nan.compare(one), None);
        assert_eq!(one.compare(nan), None);
        assert_eq!(nan.compare(nan), None);
    }

    #[test]
    fn a_comparison_of_random_numbers_is_the_host_order() {
        let mut state = 0xdead_beef_cafe_f00du64;
        for _ in 0..20_000 {
            let left = f64::from_bits(next(&mut state));
            let right = f64::from_bits(next(&mut state));
            assert_eq!(
                double(left).compare(double(right)),
                left.partial_cmp(&right),
                "{left:e} against {right:e}"
            );
        }
    }

    #[test]
    fn a_conversion_between_formats_rounds_the_way_the_host_does() {
        let mut state = 0x0123_4567_89ab_cdefu64;
        for _ in 0..20_000 {
            let value = f64::from_bits(next(&mut state));
            let narrowed = double(value).to_format(Format::Single);
            let theirs = value as f32;
            if theirs.is_nan() {
                assert!(narrowed.0.is_nan(), "{value:e}");
                continue;
            }
            assert_eq!(host_single(narrowed.0).to_bits(), theirs.to_bits(), "{value:e}");
            // Widening is exact, so the number that comes back is the one that went in.
            let widened = narrowed.0.to_format(Format::Double);
            assert_eq!(host(widened.0).to_bits(), f64::from(theirs).to_bits(), "{value:e}");
            assert!(widened.1.is_none(), "{value:e}");
        }
    }

    #[test]
    fn a_narrowing_conversion_says_what_it_did() {
        let (value, status) = double(0.1).to_format(Format::Single);
        assert_eq!(host_single(value).to_bits(), (0.1f32).to_bits());
        assert!(status.has(Status::INEXACT));
        assert!(double(0.5).to_format(Format::Single).1.is_none());
        let (value, status) = double(1e300).to_format(Format::Single);
        assert!(value.is_infinite() && status.has(Status::OVERFLOW));
        let (value, status) = double(1e-300).to_format(Format::Single);
        assert!(value.is_zero() && status.has(Status::UNDERFLOW));
        // The x87 format has more bits than a `double`, so a number goes up into it exactly and
        // comes back down as the number it started as.
        let (up, status) = double(0.1).to_format(Format::X87Extended);
        assert!(status.is_none());
        assert_eq!(up.to_bits(), 0x3ffb_cccc_cccc_cccc_d000);
        assert_eq!(host(up.to_format(Format::Double).0).to_bits(), (0.1f64).to_bits());
        // Widening keeps the error the number already had rather than removing it: a tenth that
        // went through a `double` is not the tenth an x87 number can hold.
        let tenth = Float::parse("0.1", Format::X87Extended).expect("a tenth").0;
        assert_eq!(tenth.to_bits(), 0x3ffb_cccc_cccc_cccc_cccd);
        assert_ne!(up.to_bits(), tenth.to_bits());
    }

    #[test]
    fn an_integer_becomes_the_nearest_number_to_it() {
        let mut state = 0xfeed_face_dead_c0dcu64;
        for _ in 0..20_000 {
            let value = next(&mut state) as i64;
            let mine = Float::from_signed(i128::from(value), Format::Double).0;
            assert_eq!(host(mine).to_bits(), (value as f64).to_bits(), "{value}");
            let value = next(&mut state);
            let mine = Float::from_unsigned(u128::from(value), Format::Single).0;
            assert_eq!(host_single(mine).to_bits(), (value as f32).to_bits(), "{value}");
        }
        // The ends of the two widest integer types, which are where the rounding shows.
        assert_eq!(host(Float::from_signed(0, Format::Double).0).to_bits(), (0f64).to_bits());
        assert!(Float::from_signed(1 << 52, Format::Double).1.is_none());
        assert!(Float::from_signed((1 << 53) + 1, Format::Double).1.has(Status::INEXACT));
        let (value, status) = Float::from_signed(i128::MIN, Format::Double);
        assert!(value.is_negative() && status.is_none());
        assert_eq!(host(value), -(2f64).powi(127));
        let (value, status) = Float::from_unsigned(u128::MAX, Format::Double);
        assert!(status.has(Status::INEXACT));
        assert_eq!(host(value), (2f64).powi(128));
    }

    #[test]
    fn a_number_becomes_an_integer_by_dropping_its_fraction() {
        for (value, expected) in [
            (1.5, 1),
            (-1.5, -1),
            (0.9, 0),
            (-0.9, 0),
            (2.0, 2),
            (-2.0, -2),
            (1e18, 1_000_000_000_000_000_000),
        ] {
            assert_eq!(double(value).to_integer(64, true).0, expected, "{value}");
        }
        assert!(double(2.0).to_integer(64, true).1.is_none());
        assert!(double(1.5).to_integer(64, true).1.has(Status::INEXACT));
        // Truncation toward zero lands inside an unsigned type, and anything below it does not.
        assert_eq!(double(-0.5).to_integer(32, false), (0, Status::INEXACT));
        let (value, status) = double(-1.0).to_integer(32, false);
        assert!(value == 0 && status.has(Status::INVALID));
    }

    #[test]
    fn a_number_that_will_not_fit_gives_the_end_of_the_range() {
        let (value, status) = double(1e30).to_integer(32, true);
        assert!(value == i128::from(i32::MAX) && status.has(Status::INVALID));
        let (value, status) = double(-1e30).to_integer(32, true);
        assert!(value == i128::from(i32::MIN) && status.has(Status::INVALID));
        let (value, status) = double(1e30).to_integer(32, false);
        assert!(value == i128::from(u32::MAX) && status.has(Status::INVALID));
        let (value, status) = Float::infinity(Format::Double, false).to_integer(64, true);
        assert!(value == i128::from(i64::MAX) && status.has(Status::INVALID));
        let (value, status) = Float::nan(Format::Double).to_integer(64, true);
        assert!(value == 0 && status.has(Status::INVALID));
        // The widest unsigned type has its top bit where the sign of the value holding it is.
        let (value, status) = double(f64::MAX).to_integer(128, false);
        assert!(value == -1 && status.has(Status::INVALID));
        // The widest signed one holds its own smallest number exactly.
        let smallest = double(-(2f64).powi(127));
        assert_eq!(smallest.to_integer(128, true), (i128::MIN, Status::NONE));
    }

    #[test]
    fn a_conversion_to_an_integer_is_the_one_the_host_does() {
        // Rust's own conversion saturates and turns a nan into zero, which is what C leaves
        // undefined and what this fills it in with, so the host answers for this too.
        let mut state = 0xabad_1dea_0000_0001u64;
        for _ in 0..20_000 {
            let value = f64::from_bits(next(&mut state));
            assert_eq!(double(value).to_integer(64, true).0, i128::from(value as i64), "{value:e}");
            assert_eq!(
                double(value).to_integer(32, false).0,
                i128::from(value as u32),
                "{value:e}"
            );
        }
    }

    #[test]
    fn the_wide_formats_compute_what_they_are_supposed_to() {
        let quad = |text: &str| Float::parse(text, Format::Quad).expect("a number").0;
        // A third in binary128 is the exact quotient rounded down, since the digits repeat and
        // the first one dropped is below a half. Worked out by hand rather than measured, because
        // no host here has the format.
        let (third, status) = quad("1").quotient(quad("3"));
        assert_eq!(third.to_bits(), 0x3ffd_5555_5555_5555_5555_5555_5555_5555);
        assert!(status.has(Status::INEXACT));
        // Three of them is one exactly, because the sum is a tie and the tie rounds up.
        let (whole, status) = third.sum(third).0.sum(third);
        assert_eq!(whole.to_bits(), quad("1").to_bits());
        assert!(status.has(Status::INEXACT));

        // The x87 format has sixty four bits of significand, so this is exact where a `double`
        // would have to round it.
        let x87 = |text: &str| Float::parse(text, Format::X87Extended).expect("a number").0;
        let (sum, status) = x87("9007199254740993").sum(x87("1"));
        assert!(status.is_none());
        assert_eq!(sum.to_bits(), x87("9007199254740994").to_bits());

        // Half precision has eleven, so 2049 is a tie between the two numbers either side of it
        // and rounds to the even one below.
        let half = |text: &str| Float::parse(text, Format::Half).expect("a number").0;
        let (value, status) = half("2048").sum(half("1"));
        assert!(status.has(Status::INEXACT));
        assert_eq!(value.to_bits(), half("2048").to_bits());
    }

    #[test]
    fn a_nan_survives_a_trip_through_its_encoding() {
        for format in [
            Format::Half,
            Format::BFloat16,
            Format::Single,
            Format::Double,
            Format::X87Extended,
            Format::Quad,
        ] {
            let nan = Float::nan(format);
            assert!(nan.is_nan() && !nan.is_finite() && !nan.is_infinite(), "{format:?}");
            assert_eq!(Float::from_bits(format, nan.to_bits()), nan, "{format:?}");
            assert_eq!(nan.negated().to_hex(), "-nan", "{format:?}");
            // An infinity is not a nan, in the format that stores the bit above the fraction as
            // well as in the ones that leave it implied.
            let infinity = Float::infinity(format, false);
            assert!(Float::from_bits(format, infinity.to_bits()).is_infinite(), "{format:?}");
        }
        // The host agrees about where the quiet bit is.
        assert_eq!(Float::nan(Format::Double).to_bits(), u128::from(f64::NAN.to_bits()));
        assert!(Float::from_bits(Format::Double, u128::from(f64::NAN.to_bits())).is_nan());
    }

    #[test]
    fn the_helpers_underneath_do_what_they_say() {
        assert_eq!(wide_multiply(0, 12345), (0, 0));
        assert_eq!(wide_multiply(3, 5), (0, 15));
        assert_eq!(wide_multiply(1, u128::MAX), (0, u128::MAX));
        assert_eq!(wide_multiply(u128::MAX, u128::MAX), (u128::MAX - 1, 1));
        assert_eq!(wide_multiply(1 << 127, 1 << 127), (1 << 126, 0));
        // A number divided by itself is one, at whatever scale the extra bits put it.
        assert_eq!(long_divide(1 << 127, 1 << 127, 4), (16, 0));
        assert_eq!(long_divide(3 << 126, 1 << 127, 4), (24, 0));
        assert_eq!(long_divide(1 << 127, 3 << 126, 4), (10, 1 << 127));
    }
}
