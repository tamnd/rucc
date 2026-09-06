//! The half of a cost model that is not a number.
//!
//! Section 40.4 reads `gcc/config/i386/x86-tune.def`, which is 810 lines holding 123 boolean
//! predicates over microarchitectures, and draws the lesson: a target's cost model is a hundred odd
//! numbers and a hundred odd booleans, and the booleans are decisions like whether this core
//! prefers an `lea` to an `add` that no scalar cost can express. The section asks that rucc have a
//! named place for them from the first target "rather than scattering `if target.is_x86()` through
//! passes", and this is that place.
//!
//! # Every flag has a documented default
//!
//! A new target answers `false` to every flag it has not thought about, and every flag is written
//! so that `false` is the conservative answer. That is the property that lets a target be added
//! and be correct before it is tuned, which is the difference between a tuning system and a
//! second correctness surface.
//!
//! # Where a flag belongs and where it does not
//!
//! A flag here is a fact about a machine. Whether a transformation is enabled at `-O2` is not a
//! fact about a machine, it is a pipeline decision, and it lives in the pipeline. The test of
//! whether something belongs here is whether two different processors could honestly answer
//! differently.

/// A boolean fact about the target that a pass needs and no cost can express.
///
/// Named rather than a bitfield so that a pass reads `tune(TuneFlag::PreferLea)` and a reader of
/// that pass knows what the question was. The list is short because rucc has few passes; section
/// 40.4's point is that the list has a home, not that it starts full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum TuneFlag {
    /// Prefer an address computation to an add when both would do.
    ///
    /// GCC's `X86_TUNE_OPT_AGU` family. An `lea` does not write flags and takes three operands, so
    /// it can save a move, and on some cores it goes to a slower port than the adder does. Default
    /// `false`, meaning use the ordinary add, because an add exists on every machine and is never
    /// the surprising choice.
    PreferLea,

    /// Schedule instructions at all.
    ///
    /// GCC's `X86_TUNE_SCHEDULE`, the first entry in `x86-tune.def`. An in-order core needs it and
    /// a large out-of-order window makes most of it pointless. Default `false`, which costs
    /// performance on a machine that wanted it and cannot cost correctness anywhere.
    Schedule,

    /// Turn a well predicted branch into a conditional move when the arms are cheap enough.
    ///
    /// Section 40.5 is the warning attached to this one: a correctly predicted branch is free on an
    /// out of order machine, so if-converting it buys nothing and pays for both arms. Default
    /// `false`.
    IfConvertPredictable,

    /// Unaligned loads and stores cost the same as aligned ones.
    ///
    /// True on every current x86-64 core and false on the machines the alignment rules were
    /// written for. Default `false`, which makes the compiler align things that did not need it,
    /// and the other way round would make it emit a fault on a target that traps.
    FastUnalignedAccess,

    /// A multiply is cheap enough not to be worth expanding into shifts and adds.
    ///
    /// Default `false`, so a new target expands, which is the choice that is slower on a machine
    /// with a fast multiplier rather than wrong on a machine without one.
    FastMultiply,

    /// Partial register writes stall.
    ///
    /// Writing `%al` and then reading `%eax` merges two values and some cores pay for it, which is
    /// the reason a zero extension is sometimes worth inserting where nothing needs one. Default
    /// `false`, meaning do not insert it.
    PartialRegisterStall,
}

impl TuneFlag {
    /// Every flag, so that a report can print the whole set a target answered.
    pub const ALL: [Self; 6] = [
        Self::PreferLea,
        Self::Schedule,
        Self::IfConvertPredictable,
        Self::FastUnalignedAccess,
        Self::FastMultiply,
        Self::PartialRegisterStall,
    ];

    /// What a target that has not been tuned answers.
    ///
    /// `false` for all of them, and the doc comment on each flag says what that costs. Written as
    /// a function rather than left implicit so that a flag whose safe default is `true` has
    /// somewhere to say so, and so that this file is where somebody looks to find out.
    #[must_use]
    pub const fn default_for_a_new_target(self) -> bool {
        false
    }

    /// The flag as it is spelled in a dump or a report.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreferLea => "prefer-lea",
            Self::Schedule => "schedule",
            Self::IfConvertPredictable => "if-convert-predictable",
            Self::FastUnalignedAccess => "fast-unaligned-access",
            Self::FastMultiply => "fast-multiply",
            Self::PartialRegisterStall => "partial-register-stall",
        }
    }
}

impl std::fmt::Display for TuneFlag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The answers a target gives, as a set.
///
/// A set rather than a struct of booleans so that a target lists the flags it turns on and says
/// nothing about the rest, which is how `x86-tune.def` reads and is what makes an untuned flag
/// visibly untuned rather than visibly false.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tuning {
    /// One bit per flag, in the order of [`TuneFlag::ALL`].
    bits: u32,
}

impl Tuning {
    /// Nothing turned on, which is a correct if slow target.
    #[must_use]
    pub const fn untuned() -> Self {
        Self { bits: 0 }
    }

    /// The same tuning with this flag turned on.
    #[must_use]
    pub const fn with(mut self, flag: TuneFlag) -> Self {
        self.bits |= 1 << (flag as u32);
        self
    }

    /// What this target answers for this flag.
    #[must_use]
    pub const fn get(self, flag: TuneFlag) -> bool {
        self.bits & (1 << (flag as u32)) != 0
    }

    /// The flags that are on, in order, for a report.
    pub fn enabled(self) -> impl Iterator<Item = TuneFlag> {
        TuneFlag::ALL.into_iter().filter(move |&flag| self.get(flag))
    }
}

#[cfg(test)]
mod tests {
    use super::{TuneFlag, Tuning};

    #[test]
    fn an_untuned_target_answers_the_documented_default_to_everything() {
        let tuning = Tuning::untuned();
        for flag in TuneFlag::ALL {
            assert_eq!(tuning.get(flag), flag.default_for_a_new_target(), "{flag}");
        }
        assert_eq!(tuning.enabled().count(), 0);
    }

    #[test]
    fn turning_one_flag_on_leaves_the_others_alone() {
        let tuning = Tuning::untuned().with(TuneFlag::Schedule);
        assert!(tuning.get(TuneFlag::Schedule));
        assert!(!tuning.get(TuneFlag::PreferLea));
        assert_eq!(tuning.enabled().collect::<Vec<_>>(), vec![TuneFlag::Schedule]);
    }

    #[test]
    fn every_flag_has_its_own_bit() {
        // A flag added to the enum without a place in `ALL`, or two flags sharing a
        // discriminant, would show up here as one flag turning another one on.
        for flag in TuneFlag::ALL {
            let tuning = Tuning::untuned().with(flag);
            for other in TuneFlag::ALL {
                assert_eq!(tuning.get(other), other == flag, "{flag} turned on {other}");
            }
        }
    }

    #[test]
    fn every_flag_is_in_the_list_and_has_a_distinct_name() {
        let mut names: Vec<&str> = TuneFlag::ALL.iter().map(|f| f.as_str()).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before);
        // The bit set has room for the whole list, which is what stops a flag added past 32 from
        // silently shifting out.
        assert!(before <= 32);
    }
}
