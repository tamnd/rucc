//! A cost is a pair, and the second half of it is a preference the first half cannot express.
//!
//! Section 40.2 takes this from `comp_cost` in `gcc/tree-ssa-loop-ivopts.cc`, which is a runtime
//! cost and a complexity compared lexicographically. The case it was invented for is choosing an
//! addressing mode: two modes can take the same time and one of them is a base plus a scaled index
//! plus a displacement while the other is a bare register, and the bare register is the one to
//! pick. Nothing about that preference is expressible as a number of cycles, because it is not
//! about cycles. It is about not building something complicated for no gain.
//!
//! That case generalises, which is why the pair is the cost type for the whole compiler rather
//! than a thing ivopts keeps to itself. Section 40.9 wants the same tiebreak for preferring an
//! induction variable the programmer wrote to one the compiler invented, at equal cycles, because
//! it keeps the debug information meaningful.

use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Sub};

use crate::Cycles;

/// How complicated the thing is, in no unit at all.
///
/// Deliberately unitless, and GCC's comment says so too: "in no concrete units, complexity field
/// should be larger for more complex expressions and addressing modes". It is counted rather than
/// measured, one for each structural feature, and the only meaningful operation on it is
/// comparison against another complexity produced by the same counting.
///
/// Section 40.9 records the refinement worth having: complexity is counted relative to what the
/// target can do. A scaled index is not a complication on a machine whose only index mode is
/// scaled, and counting it as one there makes every address on that target look complicated and
/// the tiebreak stop discriminating.
pub type Complexity = u32;

/// What something costs: time first, then how complicated it is.
///
/// Ordered lexicographically. Equal times are broken by lower complexity, and a difference in time
/// settles it whatever the complexities are, which is the right way round: complexity is a
/// tiebreak and never a reason to choose something slower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cost {
    /// The time.
    pub cycles: Cycles,
    /// The tiebreak.
    pub complexity: Complexity,
    /// Whether some arithmetic on the way here hit the ceiling instead of giving an answer.
    ///
    /// Section 40.13 lists overflow as a way this goes wrong and asks that the saturating case be
    /// a counter rather than silent. The flag rides along on the value instead, which is the same
    /// thing said better: a counter tells you it happened somewhere in the compilation, and this
    /// tells you it happened to the number a particular decision was made on. It is not part of
    /// the ordering, because a saturated cost is still the largest cost and should still lose.
    ///
    /// [`Cost::INFINITE`] does not set it. Saying a thing is impossible is not an overflow, and
    /// the two would be indistinguishable if the flag did not tell them apart.
    saturated: bool,
}

impl Cost {
    /// Free, and as simple as it gets. GCC's `no_cost`.
    pub const ZERO: Self = Self { cycles: Cycles::ZERO, complexity: 0, saturated: false };

    /// Not possible. GCC's `infinite_cost`.
    ///
    /// The complexity is zero rather than also infinite, because complexity only ever decides
    /// between two things of equal time and nothing is going to be chosen against this.
    pub const INFINITE: Self = Self { cycles: Cycles::INFINITE, complexity: 0, saturated: false };

    /// A cost of a time and a complexity.
    #[must_use]
    pub const fn new(cycles: Cycles, complexity: Complexity) -> Self {
        Self { cycles, complexity, saturated: false }
    }

    /// A cost that is only a time, which is most of them.
    #[must_use]
    pub const fn cycles(cycles: Cycles) -> Self {
        Self::new(cycles, 0)
    }

    /// Whether this says the thing is impossible rather than expensive.
    #[must_use]
    pub const fn is_infinite(self) -> bool {
        self.cycles.is_infinite()
    }

    /// Whether the arithmetic that produced this hit the ceiling.
    ///
    /// A pass that gets `true` here has a number it should not act on, and the honest response is
    /// to treat the comparison as undecided and record a note. In practice it means a loop nest
    /// whose guessed trip counts multiplied out past what the type holds, which section 40.11
    /// already clamps for a different reason.
    #[must_use]
    pub const fn saturated(self) -> bool {
        self.saturated
    }

    /// One more structural feature, which makes it that much more complicated and no slower.
    #[must_use]
    pub const fn complicated(mut self) -> Self {
        self.complexity += 1;
        self
    }
}

impl Add for Cost {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            cycles: self.cycles + other.cycles,
            complexity: self.complexity.saturating_add(other.complexity),
            saturated: self.saturated
                || other.saturated
                || self.cycles.adding_saturates(other.cycles),
        }
    }
}

impl AddAssign for Cost {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl Sub for Cost {
    type Output = Self;

    /// The saving of one thing over another, in both components.
    fn sub(self, other: Self) -> Self {
        Self {
            cycles: self.cycles - other.cycles,
            complexity: self.complexity.saturating_sub(other.complexity),
            saturated: self.saturated || other.saturated,
        }
    }
}

impl Mul<i64> for Cost {
    type Output = Self;

    /// Scaled by a repeat count, which is how a block's cost becomes a loop's cost.
    ///
    /// The complexity does not scale. Doing the same simple thing a thousand times is a thousand
    /// times the work and is not any more complicated, and scaling it would let a hot loop's
    /// tiebreak outvote a cold block's time.
    fn mul(self, by: i64) -> Self {
        Self {
            cycles: self.cycles * by,
            complexity: self.complexity,
            saturated: self.saturated || self.cycles.scaling_saturates(by),
        }
    }
}

impl Div<i64> for Cost {
    type Output = Self;

    fn div(self, by: i64) -> Self {
        Self { cycles: self.cycles / by, complexity: self.complexity, saturated: self.saturated }
    }
}

impl Ord for Cost {
    /// Lexicographic: time, then complexity.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.cycles.cmp(&other.cycles).then(self.complexity.cmp(&other.complexity))
    }
}

impl PartialOrd for Cost {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Cost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.cycles)?;
        if self.complexity != 0 {
            write!(f, " (complexity {})", self.complexity)?;
        }
        if self.saturated {
            f.write_str(" (saturated)")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Cost;
    use crate::Cycles;

    #[test]
    fn a_faster_thing_wins_however_complicated_it_is() {
        let fast = Cost::new(Cycles::insns(1), 9);
        let slow = Cost::new(Cycles::insns(2), 0);
        assert!(fast < slow);
    }

    #[test]
    fn equal_times_are_broken_by_the_simpler_one() {
        // The ivopts case that section 40.2 says the pair was invented for: a bare base register
        // and a base plus scaled index plus displacement that happen to cost the same.
        let simple = Cost::new(Cycles::insns(1), 0);
        let elaborate = Cost::new(Cycles::insns(1), 3);
        assert!(simple < elaborate);
        assert_eq!(simple.complicated().complicated().complicated(), elaborate);
    }

    #[test]
    fn infinity_loses_to_everything_and_stays_infinite() {
        assert!(Cost::INFINITE > Cost::new(Cycles::insns(1_000), 100));
        assert!((Cost::INFINITE + Cost::cycles(Cycles::ONE)).is_infinite());
        assert!(!Cost::INFINITE.saturated());
    }

    #[test]
    fn saturating_is_recorded_rather_than_silent() {
        let big = Cost::cycles(Cycles::hundredths(i64::MAX - 1));
        let sum = big + big;
        assert!(sum.is_infinite());
        assert!(sum.saturated(), "section 40.13 asks for this to be visible");
        // And it carries, so the flag survives being folded into a larger sum.
        assert!((sum + Cost::ZERO).saturated());
        assert!((sum - Cost::ZERO).saturated());
        assert!((sum * 2).saturated());
        assert!((sum / 2).saturated());
    }

    #[test]
    fn an_honest_infinity_is_not_a_saturation() {
        // Both are the largest cost there is and only one of them is a number that went wrong.
        let impossible = Cost::INFINITE;
        let overflowed = Cost::cycles(Cycles::hundredths(i64::MAX - 1)) * 4;
        assert_eq!(impossible.cycles, overflowed.cycles);
        assert!(!impossible.saturated());
        assert!(overflowed.saturated());
    }

    #[test]
    fn a_repeat_count_scales_the_time_and_not_the_complexity() {
        let block = Cost::new(Cycles::insns(3), 2);
        let loop_body = block * 1_000;
        assert_eq!(loop_body.cycles, Cycles::insns(3_000));
        assert_eq!(loop_body.complexity, 2);
    }

    #[test]
    fn printing_says_what_is_there_and_nothing_else() {
        assert_eq!(Cost::cycles(Cycles::insns(2)).to_string(), "2");
        assert_eq!(Cost::new(Cycles::insns(2), 3).to_string(), "2 (complexity 3)");
        assert_eq!(Cost::INFINITE.to_string(), "infinite");
    }
}
