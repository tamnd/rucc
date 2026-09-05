//! How many transformations a pass is allowed before it stops transforming.
//!
//! Section 9.10 of `spec/09-optimizer.md` requires this of every pass, and the reason is
//! bisection. When a program compiles wrongly at `-O2` and correctly at `-O0`, the question is
//! which of the thousands of rewrites the optimizer performed is the wrong one. With fuel it is
//! a binary search: give the suspect pass n transformations, run the program, and halve. The
//! answer arrives in about twenty compilations of a file rather than by reading a diff of two
//! assembly listings.
//!
//! The counter is per pass and per compilation rather than per function, because the site being
//! searched for is one site in one file and numbering it per function would need the function to
//! be identified first, which is the thing not yet known.

use std::fmt;

/// What a pass has left.
///
/// A pass asks [`Fuel::take`] immediately before each transformation and does nothing when the
/// answer is no. A pass that asks after transforming, or that transforms without asking, is
/// what the fuel test in [`crate::pipeline`] exists to catch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fuel {
    /// How many transformations are still allowed, or `None` for as many as are wanted.
    left: Option<u32>,
    /// How many have been performed.
    spent: u32,
}

impl Fuel {
    /// Fuel that never runs out, which is what every pass gets unless `-fpass-fuel` says
    /// otherwise.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self { left: None, spent: 0 }
    }

    /// Fuel for exactly this many transformations.
    #[must_use]
    pub const fn of(count: u32) -> Self {
        Self { left: Some(count), spent: 0 }
    }

    /// Whether one more transformation is allowed, counting it when it is.
    ///
    /// The counting happens here rather than at the transformation because a pass that has to
    /// remember to do two things does one of them.
    pub fn take(&mut self) -> bool {
        match &mut self.left {
            Some(0) => false,
            Some(left) => {
                *left -= 1;
                self.spent += 1;
                true
            }
            None => {
                self.spent += 1;
                true
            }
        }
    }

    /// How many transformations have been taken.
    #[must_use]
    pub const fn spent(self) -> u32 {
        self.spent
    }

    /// Whether the pass has run out, which is only ever true when a limit was set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        matches!(self.left, Some(0))
    }
}

impl Default for Fuel {
    fn default() -> Self {
        Self::unlimited()
    }
}

impl fmt::Display for Fuel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.left {
            None => write!(f, "{} of unlimited", self.spent),
            Some(left) => write!(f, "{} of {}", self.spent, self.spent + left),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Fuel;

    #[test]
    fn unlimited_fuel_is_never_refused_and_still_counts() {
        let mut fuel = Fuel::unlimited();
        for _ in 0..1000 {
            assert!(fuel.take());
        }
        assert_eq!(fuel.spent(), 1000);
        assert!(!fuel.is_empty());
    }

    #[test]
    fn a_limit_of_three_allows_three_and_then_stops_allowing_any() {
        let mut fuel = Fuel::of(3);
        assert!(fuel.take());
        assert!(fuel.take());
        assert!(fuel.take());
        assert!(!fuel.take());
        assert!(!fuel.take());
        assert_eq!(fuel.spent(), 3);
        assert!(fuel.is_empty());
    }

    #[test]
    fn a_limit_of_zero_allows_nothing_at_all() {
        let mut fuel = Fuel::of(0);
        assert!(!fuel.take());
        assert_eq!(fuel.spent(), 0);
        assert!(fuel.is_empty());
    }

    #[test]
    fn the_display_says_how_much_of_how_much() {
        let mut fuel = Fuel::of(4);
        assert!(fuel.take());
        assert_eq!(fuel.to_string(), "1 of 4");
        let mut open = Fuel::unlimited();
        assert!(open.take());
        assert_eq!(open.to_string(), "1 of unlimited");
    }
}
