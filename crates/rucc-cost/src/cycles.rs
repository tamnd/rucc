//! The two units a cost is measured in, and the reason they are two types.
//!
//! Section 40.1 of `spec/optimizer/40-cost-models.md` records the failure this module exists to
//! prevent: a cost in one unit compared against a threshold in another. It is an easy mistake
//! because both are small integers and neither carries its unit at the point of comparison, and it
//! is a hard mistake to find because the result is a heuristic that is quietly wrong rather than a
//! compiler that stops. So time and space are separate types here, neither converts to the other,
//! and a pass that wants to trade one against the other has to say in which direction.

use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Sub};

/// Time, as multiples of one simple ALU operation.
///
/// Fixed point with two decimal places, per [`Cycles::SCALE`]. Integers are not enough: an `lea`
/// on x86-64 costs less than an add on some microarchitectures and more on others, and a table
/// that can only say one or two has to round the answer before any pass has seen it. Floating
/// point would do, and is not used, because two compilers built from the same source must make the
/// same decisions and a cost that is compared for equality is a cost that has to be exact.
///
/// # Infinity
///
/// [`Cycles::INFINITE`] means the thing is not possible, which is a different statement from very
/// expensive and is the same type so that the comparison operators work on both. Section 40.2
/// takes this from GCC's `infinite_cost` in `gcc/tree-ssa-loop-ivopts.cc`, and the reason to have
/// it once rather than in each pass is that `i64::MAX / 2` written in eight places is eight
/// chances to overflow it.
///
/// Arithmetic saturates at infinity rather than wrapping. Section 40.13 asks that the saturating
/// case be visible rather than silent, and [`crate::Cost`] is where that is recorded, because a
/// saturation matters when it decides something and a bare `Cycles` has not decided anything yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Cycles(i64);

impl Cycles {
    /// How many units one simple ALU operation is worth, which fixes where the point sits.
    ///
    /// A hundred, so that a table can be read and written in the units people quote latencies in.
    /// GCC's `COSTS_N_INSNS` uses four, which buys quarter granularity and makes every number in
    /// every target file a multiple of four that has to be divided in your head.
    pub const SCALE: i64 = 100;

    /// Free. Not the same as [`Cycles::ONE`] on a machine that issues several operations a cycle,
    /// and not the same as unknown either.
    pub const ZERO: Self = Self(0);

    /// One simple ALU operation, the unit everything else is quoted against.
    pub const ONE: Self = Self(Self::SCALE);

    /// This is not possible. Saturating, and larger than every finite cost.
    pub const INFINITE: Self = Self(i64::MAX);

    /// A whole number of simple operations.
    #[must_use]
    pub const fn insns(n: i64) -> Self {
        Self(n.saturating_mul(Self::SCALE))
    }

    /// A number of hundredths, for a table entry that is not a whole number of operations.
    #[must_use]
    pub const fn hundredths(n: i64) -> Self {
        Self(n)
    }

    /// Whether this is [`Cycles::INFINITE`], meaning impossible rather than expensive.
    ///
    /// Worth asking before printing a cost and before dividing by one, and worth not asking
    /// anywhere else, since the ordering already puts it above everything finite.
    #[must_use]
    pub const fn is_infinite(self) -> bool {
        self.0 == i64::MAX
    }

    /// The raw hundredths, for a report that wants to do its own arithmetic.
    #[must_use]
    pub const fn raw(self) -> i64 {
        self.0
    }

    /// Whether adding these two would saturate, which is the check [`crate::Cost`] makes.
    #[must_use]
    pub(crate) const fn adding_saturates(self, other: Self) -> bool {
        !self.is_infinite() && !other.is_infinite() && self.0.checked_add(other.0).is_none()
    }

    /// Whether scaling this would saturate.
    #[must_use]
    pub(crate) const fn scaling_saturates(self, by: i64) -> bool {
        !self.is_infinite() && self.0.checked_mul(by).is_none()
    }
}

impl Add for Cycles {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

impl AddAssign for Cycles {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl Sub for Cycles {
    type Output = Self;

    /// Saturating below at zero as well as above at infinity.
    ///
    /// A negative cost is not a thing any pass here means. Every subtraction in the documents is
    /// a saving, of the form what it cost before minus what it costs now, and a saving that came
    /// out negative is a transformation that did not pay, which zero says as well as minus three
    /// does and without the risk that something later multiplies by it.
    fn sub(self, other: Self) -> Self {
        if self.is_infinite() {
            return Self::INFINITE;
        }
        Self(self.0.saturating_sub(other.0).max(0))
    }
}

impl Mul<i64> for Cycles {
    type Output = Self;

    fn mul(self, by: i64) -> Self {
        if self.is_infinite() {
            return Self::INFINITE;
        }
        Self(self.0.saturating_mul(by))
    }
}

impl Div<i64> for Cycles {
    type Output = Self;

    /// Dividing infinity leaves infinity, and dividing by zero is a bug rather than an infinity.
    ///
    /// A pass that divides a cost by a count it did not check is a pass with an empty set it
    /// thought was non-empty, and turning that into a very large number would hide it until the
    /// number came out of a heuristic somewhere else.
    fn div(self, by: i64) -> Self {
        assert!(by != 0, "a cost divided by zero, which is an empty set somewhere upstream");
        if self.is_infinite() {
            return Self::INFINITE;
        }
        Self(self.0 / by)
    }
}

impl fmt::Display for Cycles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_infinite() {
            return f.write_str("infinite");
        }
        let whole = self.0 / Self::SCALE;
        let part = (self.0 % Self::SCALE).abs();
        if part == 0 { write!(f, "{whole}") } else { write!(f, "{whole}.{part:02}") }
    }
}

/// Space, in bytes of machine code, exact and from the encoder.
///
/// Exact rather than estimated because it can be: the encoder knows how long an instruction is and
/// asking it is cheaper than maintaining a second table that drifts from it. That is also why
/// there is no infinity here. An instruction that cannot be encoded is not an instruction with a
/// very large size, it is an instruction the selector must not have chosen, and it is caught
/// where it is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Bytes(pub u32);

impl Add for Bytes {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

impl AddAssign for Bytes {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl Mul<u32> for Bytes {
    type Output = Self;

    fn mul(self, by: u32) -> Self {
        Self(self.0.saturating_mul(by))
    }
}

impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} bytes", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{Bytes, Cycles};

    #[test]
    fn one_operation_is_the_unit() {
        assert_eq!(Cycles::ONE, Cycles::insns(1));
        assert_eq!(Cycles::insns(3), Cycles::hundredths(300));
        assert_eq!(Cycles::ONE.to_string(), "1");
        assert_eq!(Cycles::hundredths(133).to_string(), "1.33");
        assert_eq!(Cycles::ZERO.to_string(), "0");
    }

    #[test]
    fn infinity_is_above_everything_finite_and_stays_there() {
        assert!(Cycles::INFINITE > Cycles::insns(1_000_000));
        assert_eq!(Cycles::INFINITE + Cycles::ONE, Cycles::INFINITE);
        assert_eq!(Cycles::INFINITE * 3, Cycles::INFINITE);
        assert_eq!(Cycles::INFINITE / 3, Cycles::INFINITE);
        assert_eq!(Cycles::INFINITE - Cycles::ONE, Cycles::INFINITE);
        assert!(Cycles::INFINITE.is_infinite());
        assert!(!Cycles::insns(9999).is_infinite());
    }

    #[test]
    fn arithmetic_saturates_rather_than_wrapping() {
        let big = Cycles::hundredths(i64::MAX - 1);
        assert_eq!(big + big, Cycles::INFINITE);
        assert_eq!(big * 2, Cycles::INFINITE);
    }

    #[test]
    fn a_saving_that_came_out_negative_is_no_saving() {
        assert_eq!(Cycles::insns(2) - Cycles::insns(5), Cycles::ZERO);
        assert_eq!(Cycles::insns(5) - Cycles::insns(2), Cycles::insns(3));
    }

    #[test]
    #[should_panic(expected = "empty set somewhere upstream")]
    fn dividing_by_zero_is_a_bug_and_not_an_infinity() {
        let _ = Cycles::ONE / 0;
    }

    #[test]
    fn bytes_are_a_different_type_and_do_not_mix_with_cycles() {
        // The whole point of the module. If this ever compiles with a `Cycles` on either side,
        // the defence section 40.1 asks for has gone.
        let size = Bytes(4) + Bytes(3);
        assert_eq!(size, Bytes(7));
        assert_eq!((Bytes(2) * 3).to_string(), "6 bytes");
    }
}
