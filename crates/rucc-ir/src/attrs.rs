//! What is true of a whole function rather than of one instruction in it.
//!
//! Design: `spec/08-ir.md` section 8.4.
//!
//! A flag in [`crate::Flags`] is a licence over one instruction. An attribute here is a fact
//! about a function, and the difference matters because the two are used at different times: the
//! optimizer reads a flag when it is about to rewrite the instruction carrying it, and it reads
//! an attribute when it is looking at a call and has no other way to find out what the callee
//! does.
//!
//! That is the test for whether something belongs here. `noreturn` is an attribute because the
//! block after a call to `exit` is unreachable and the only way to know that is to ask about
//! `exit`. `nsw` is not, because the instruction it licenses is right there.
//!
//! Nearly every one of these comes from something a person wrote. `_Noreturn` and the GNU
//! attributes it shares a spelling with, `inline`, `__attribute__((const))` and `((pure))` and
//! `((cold))` and `((naked))`, and the command line for the rest. None of them is inferred yet;
//! inferring them from a function body is an interprocedural analysis and belongs to the
//! optimizer, which will set them on the same fields.

use std::fmt;

/// Everything true of a whole function.
///
/// Two parts, because most of these are either so or not so and one of them has three answers.
/// A default set is a function nobody has promised anything about, which is what a function
/// fresh from [`crate::Func::new`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Attrs {
    /// The ones that are either set or not.
    pub set: AttrSet,
    /// How far the code generator may fuse a multiply and an addition.
    pub fp_contract: FpContract,
}

impl Attrs {
    /// Nothing promised.
    pub const NONE: Self = Self { set: AttrSet::NONE, fp_contract: FpContract::Off };

    /// Whether nothing has been promised, which is when the printer writes nothing at all.
    #[must_use]
    pub const fn is_default(self) -> bool {
        self.set.is_empty() && matches!(self.fp_contract, FpContract::Off)
    }

    /// Two attributes that are set and contradict each other, if there are any.
    ///
    /// The verifier asks, because a function that is both `always_inline` and `noinline` is one
    /// where two parts of the frontend disagreed, and the answer the optimizer picks would be
    /// whichever branch it happens to test first.
    #[must_use]
    pub fn conflict(self) -> Option<(&'static str, &'static str)> {
        CONFLICTS
            .iter()
            .find(|&&(one, other, _, _)| self.set.contains(one) && self.set.contains(other))
            .map(|&(_, _, one, other)| (one, other))
    }
}

impl fmt::Display for Attrs {
    /// The form the textual IR uses, `attrs(nounwind, fp_contract=on)`, and nothing at all when
    /// nothing has been promised.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_default() {
            return Ok(());
        }
        f.write_str("attrs(")?;
        let mut first = true;
        for (_, name) in self.set.iter() {
            if !first {
                f.write_str(", ")?;
            }
            first = false;
            f.write_str(name)?;
        }
        if self.fp_contract != FpContract::Off {
            if !first {
                f.write_str(", ")?;
            }
            write!(f, "fp_contract={}", self.fp_contract.name())?;
        }
        f.write_str(")")
    }
}

/// The attributes that are either set or not.
///
/// A bitset for the same reason [`crate::Flags`] is one, though the pressure is lower here since
/// there is one of these per function rather than one per instruction.
#[derive(Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct AttrSet(u32);

impl AttrSet {
    /// Nothing set.
    pub const NONE: Self = Self(0);

    /// No exception unwinds out of this function. Every C function compiled without
    /// `-fexceptions` is one, and it is what lets a call be moved and lets the caller skip the
    /// landing pad.
    pub const NOUNWIND: Self = Self(1 << 0);
    /// Control never comes back. `_Noreturn`, and `__attribute__((noreturn))` for the same
    /// thing under the older spelling. The instruction after a call to one is unreachable.
    pub const NORETURN: Self = Self(1 << 1);
    /// Control may come back twice from one call. `setjmp` and `vfork`, under
    /// `__attribute__((returns_twice))`. Every value live across a call to one has to be in
    /// memory, so this switches off a large amount of the optimizer for the caller.
    pub const RETURNS_TWICE: Self = Self(1 << 2);
    /// Control comes back, eventually. This is what says an empty infinite loop is not in here,
    /// and without it a call cannot be deleted even when nothing uses its result.
    pub const WILLRETURN: Self = Self(1 << 3);

    /// Rarely called, from `__attribute__((cold))`. Its code goes in the cold section and the
    /// path leading to a call to it is the unlikely one.
    pub const COLD: Self = Self(1 << 4);
    /// Often called, from `__attribute__((hot))`.
    pub const HOT: Self = Self(1 << 5);

    /// The programmer wrote `inline`, which in C is a hint about linkage and about inlining and
    /// which the inliner treats as a small nudge rather than as an instruction.
    pub const INLINE_HINT: Self = Self(1 << 6);
    /// `__attribute__((always_inline))`, which is not a hint. Failing to inline one of these is
    /// an error, because the header that wrote it usually meant a target-specific builtin that
    /// does not work any other way.
    pub const ALWAYS_INLINE: Self = Self(1 << 7);
    /// `__attribute__((noinline))`.
    pub const NOINLINE: Self = Self(1 << 8);
    /// `__attribute__((optimize("O0")))` and the pragma for it. Nothing in this function is
    /// rewritten, which is what somebody debugging one function of a release build asks for.
    pub const OPTNONE: Self = Self(1 << 9);

    /// Reads no memory and writes none, so its result depends only on its arguments and two
    /// calls with the same arguments are one call. `__attribute__((const))`.
    pub const READNONE: Self = Self(1 << 10);
    /// Writes no memory, though it may read it. `__attribute__((pure))`. Two calls with the
    /// same arguments are one call only if nothing wrote memory in between.
    pub const READONLY: Self = Self(1 << 11);
    /// Touches no memory except through the pointers it was passed. This is what makes a call
    /// stop clobbering everything the caller knew about its own locals.
    pub const ARGMEM_ONLY: Self = Self(1 << 12);

    /// `__attribute__((naked))`. No prologue and no epilogue are emitted, the body is inline
    /// assembly, and the code generator does exactly what it is told.
    pub const NAKED: Self = Self(1 << 13);
    /// Keep it even if nothing refers to it. `__attribute__((used))`, which is how a section of
    /// initializers survives a linker that garbage collects.
    pub const USED: Self = Self(1 << 14);
    /// Emit a stack protector for this frame, from `-fstack-protector` and the attribute.
    pub const STACK_PROTECT: Self = Self(1 << 15);
    /// Emit none, whatever the command line said.
    /// `__attribute__((no_stack_protector))`, which the kernel needs on the functions that run
    /// before the canary exists.
    pub const NO_STACK_PROTECTOR: Self = Self(1 << 16);

    /// The underlying bits, for the printer and for hashing.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Whether nothing is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every attribute in `other` is set here.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Both sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// This set without the attributes in `other`.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Every attribute that is set, with its name, in the order the printer writes them.
    pub fn iter(self) -> impl Iterator<Item = (Self, &'static str)> {
        NAMED.iter().copied().filter(move |&(attr, _)| self.contains(attr))
    }

    /// The attribute with that name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        NAMED.iter().find(|&&(_, named)| named == name).map(|&(attr, _)| attr)
    }
}

impl std::ops::BitOr for AttrSet {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

impl std::ops::BitOrAssign for AttrSet {
    fn bitor_assign(&mut self, other: Self) {
        *self = self.union(other);
    }
}

impl fmt::Debug for AttrSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("AttrSet::NONE");
        }
        let named: Vec<&str> = self.iter().map(|(_, name)| name).collect();
        f.write_str(&named.join(" | "))
    }
}

/// Each attribute with its name, in printing order.
static NAMED: &[(AttrSet, &str)] = &[
    (AttrSet::NOUNWIND, "nounwind"),
    (AttrSet::NORETURN, "noreturn"),
    (AttrSet::RETURNS_TWICE, "returns_twice"),
    (AttrSet::WILLRETURN, "willreturn"),
    (AttrSet::COLD, "cold"),
    (AttrSet::HOT, "hot"),
    (AttrSet::INLINE_HINT, "inline_hint"),
    (AttrSet::ALWAYS_INLINE, "always_inline"),
    (AttrSet::NOINLINE, "noinline"),
    (AttrSet::OPTNONE, "optnone"),
    (AttrSet::READNONE, "readnone"),
    (AttrSet::READONLY, "readonly"),
    (AttrSet::ARGMEM_ONLY, "argmem_only"),
    (AttrSet::NAKED, "naked"),
    (AttrSet::USED, "used"),
    (AttrSet::STACK_PROTECT, "stack_protect"),
    (AttrSet::NO_STACK_PROTECTOR, "no_stack_protector"),
];

/// The pairs that cannot both be set, with their names for the message.
static CONFLICTS: &[(AttrSet, AttrSet, &str, &str)] = &[
    (AttrSet::ALWAYS_INLINE, AttrSet::NOINLINE, "always_inline", "noinline"),
    (AttrSet::ALWAYS_INLINE, AttrSet::OPTNONE, "always_inline", "optnone"),
    (AttrSet::COLD, AttrSet::HOT, "cold", "hot"),
    (AttrSet::READNONE, AttrSet::READONLY, "readnone", "readonly"),
    (AttrSet::NORETURN, AttrSet::WILLRETURN, "noreturn", "willreturn"),
    (AttrSet::STACK_PROTECT, AttrSet::NO_STACK_PROTECTOR, "stack_protect", "no_stack_protector"),
];

/// How far a multiply and an addition may be fused into one rounding.
///
/// [`crate::Flags::CONTRACT`] is the same question asked about one instruction, and it is the
/// one the optimizer reads. This is the one the code generator reads, because by the time it
/// runs the two operations it might fuse may have arrived from different expressions and the
/// flags that were on them are gone. `off` is the default here rather than the one C says,
/// because the frontend is what knows what the command line asked for and a licence nobody
/// granted should not be assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum FpContract {
    /// Never. Every rounding the source asked for happens.
    #[default]
    Off,
    /// Within one expression, which is what C's `FP_CONTRACT` pragma allows.
    On,
    /// Anywhere in the function, which is what `-ffp-contract=fast` means and what gcc does by
    /// default.
    Fast,
}

impl FpContract {
    /// The textual form.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Fast => "fast",
        }
    }

    /// The setting with that name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|contract| contract.name() == name)
    }

    /// Every setting, least permissive first.
    pub fn all() -> impl Iterator<Item = Self> {
        [Self::Off, Self::On, Self::Fast].into_iter()
    }
}

impl fmt::Display for FpContract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_promised_prints_as_nothing() {
        assert!(Attrs::NONE.is_default());
        assert_eq!(Attrs::default(), Attrs::NONE);
        assert_eq!(Attrs::NONE.to_string(), "");
        assert_eq!(Attrs::NONE.conflict(), None);
    }

    #[test]
    fn the_spec_example_prints_the_way_the_spec_writes_it() {
        let attrs = Attrs { set: AttrSet::NOUNWIND, fp_contract: FpContract::On };
        assert_eq!(attrs.to_string(), "attrs(nounwind, fp_contract=on)");
    }

    #[test]
    fn one_of_each_half_on_its_own() {
        let set = Attrs { set: AttrSet::COLD, ..Attrs::NONE };
        assert_eq!(set.to_string(), "attrs(cold)");
        let keyed = Attrs { fp_contract: FpContract::Fast, ..Attrs::NONE };
        assert_eq!(keyed.to_string(), "attrs(fp_contract=fast)");
    }

    #[test]
    fn attributes_print_in_one_order_whatever_order_they_were_set_in() {
        let one = Attrs { set: AttrSet::NOUNWIND | AttrSet::COLD, ..Attrs::NONE };
        let other = Attrs { set: AttrSet::COLD | AttrSet::NOUNWIND, ..Attrs::NONE };
        assert_eq!(one.to_string(), "attrs(nounwind, cold)");
        assert_eq!(one, other);
    }

    #[test]
    fn every_attribute_has_a_name_and_finds_it_again() {
        for &(attr, name) in NAMED {
            assert_eq!(AttrSet::from_name(name), Some(attr), "{name}");
        }
        assert_eq!(AttrSet::from_name("nsw"), None);
        assert_eq!(AttrSet::from_name(""), None);
    }

    #[test]
    fn no_two_attributes_share_a_bit() {
        let mut seen = 0u32;
        for &(attr, name) in NAMED {
            assert_eq!(attr.bits().count_ones(), 1, "{name} is not one bit");
            assert_eq!(seen & attr.bits(), 0, "{name} shares a bit");
            seen |= attr.bits();
        }
    }

    #[test]
    fn a_function_cannot_be_told_to_inline_and_not_to() {
        let attrs = Attrs { set: AttrSet::ALWAYS_INLINE | AttrSet::NOINLINE, ..Attrs::NONE };
        assert_eq!(attrs.conflict(), Some(("always_inline", "noinline")));
        let fine = Attrs { set: AttrSet::INLINE_HINT | AttrSet::NOINLINE, ..Attrs::NONE };
        assert_eq!(fine.conflict(), None);
    }

    #[test]
    fn both_halves_of_every_conflicting_pair_are_real_attributes() {
        for &(one, other, one_name, other_name) in CONFLICTS {
            assert_eq!(AttrSet::from_name(one_name), Some(one), "{one_name}");
            assert_eq!(AttrSet::from_name(other_name), Some(other), "{other_name}");
        }
    }

    #[test]
    fn a_set_says_what_is_in_it_when_something_prints_it_for_debugging() {
        assert_eq!(format!("{:?}", AttrSet::NONE), "AttrSet::NONE");
        assert_eq!(format!("{:?}", AttrSet::COLD | AttrSet::NAKED), "cold | naked");
    }

    #[test]
    fn combining_and_removing() {
        let mut set = AttrSet::NOUNWIND;
        set |= AttrSet::COLD;
        assert!(set.contains(AttrSet::NOUNWIND));
        assert!(set.contains(AttrSet::COLD));
        assert!(!set.contains(AttrSet::HOT));
        assert_eq!(set.without(AttrSet::COLD), AttrSet::NOUNWIND);
        assert!(AttrSet::NONE.is_empty());
    }

    #[test]
    fn every_contraction_setting_finds_its_name_again() {
        for contract in FpContract::all() {
            assert_eq!(FpContract::from_name(contract.name()), Some(contract));
        }
        assert_eq!(FpContract::from_name("maybe"), None);
        assert_eq!(FpContract::default(), FpContract::Off);
    }
}
