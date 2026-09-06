//! What is known about a pointer, which is what discharges a check.
//!
//! Section 6.2.3 of `spec/safe-memory/06-instrumentation.md` calls these facts rather than
//! instructions on purpose. A check is code and costs something; a fact is a thing the optimizer
//! may assume, established in one place and exploited in another, and costs nothing at all. That
//! is the same shape `nsw` has, and it is what makes check elimination a dataflow problem instead
//! of a pass with its own opinions.
//!
//! There are four of them and a value carries any combination, including none, which is what
//! every value in a function compiled without `-fsafety` carries. They live in a side table on
//! [`Func`](crate::Func) rather than in the value itself, so a module with no safety in it is the
//! same module it was before this existed.

use std::fmt;

use crate::Value;

/// What is known about one value.
///
/// All four fields absent is the default and means nothing is known, which is not the same as
/// knowing the pointer is bad. A fact is only ever a promise, never a denial, so an optimizer
/// that loses one makes the program slower and never makes it wrong.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The range the pointer is known to lie in, which is `!bounds(lo, ext)`.
    pub bounds: Option<Bounds>,
    /// How many bytes at the pointer are known initialized, which is `!init(n)`.
    pub init: Option<u64>,
    /// The alignment the pointer is known to have, in bytes, which is `!aligned(a)`.
    pub align: Option<u32>,
    /// Whether the storage the pointer points into is known live here, which is `!live`.
    pub live: bool,
}

impl Facts {
    /// Nothing known, which is what every value has until something establishes otherwise.
    pub const NONE: Self = Self { bounds: None, init: None, align: None, live: false };

    /// Whether nothing at all is known, which is when there is nothing to print.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bounds.is_none() && self.init.is_none() && self.align.is_none() && !self.live
    }
}

/// The range a pointer is known to lie in.
///
/// Two values and not two numbers, because the range of a heap allocation is not known until it
/// is made. Where they are constants the optimizer folds them like any other constant, which is
/// the case document 07 section 7.4 collapses into one comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// Where the range starts.
    pub lo: Value,
    /// How many bytes long it is.
    pub ext: Value,
}

impl fmt::Display for Facts {
    /// The list form the textual IR uses, `!live, !aligned(8)`, with the facts in a fixed order
    /// and nothing at all when none of them is known.
    ///
    /// The two values a `!bounds` names are written by their raw index, which is the printer's
    /// numbering only when the function was built in print order. [`Printer`](crate::Printer)
    /// writes them itself for that reason and this is here for a diagnostic to use.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        let mut sep = |f: &mut fmt::Formatter<'_>| {
            let out = if first { Ok(()) } else { f.write_str(", ") };
            first = false;
            out
        };
        if let Some(bounds) = self.bounds {
            sep(f)?;
            write!(f, "!bounds(%{}, %{})", bounds.lo.raw(), bounds.ext.raw())?;
        }
        if self.live {
            sep(f)?;
            f.write_str("!live")?;
        }
        if let Some(n) = self.init {
            sep(f)?;
            write!(f, "!init({n})")?;
        }
        if let Some(align) = self.align {
            sep(f)?;
            write!(f, "!aligned({align})")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_known_prints_as_nothing() {
        assert!(Facts::NONE.is_empty());
        assert_eq!(Facts::NONE.to_string(), "");
        assert_eq!(Facts::default(), Facts::NONE);
    }

    #[test]
    fn the_facts_print_in_the_order_the_specification_lists_them() {
        let facts = Facts {
            bounds: Some(Bounds { lo: Value::from_usize(3), ext: Value::from_usize(4) }),
            init: Some(16),
            align: Some(8),
            live: true,
        };
        assert!(!facts.is_empty());
        assert_eq!(facts.to_string(), "!bounds(%3, %4), !live, !init(16), !aligned(8)");
    }

    #[test]
    fn one_fact_on_its_own_carries_no_separator() {
        assert_eq!(Facts { live: true, ..Facts::NONE }.to_string(), "!live");
        assert_eq!(Facts { align: Some(4), ..Facts::NONE }.to_string(), "!aligned(4)");
    }
}
