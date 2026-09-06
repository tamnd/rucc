//! Instruction flags, atomic orderings, and the read-modify-write operations.
//!
//! Design: `spec/08-ir.md` section 8.4.
//!
//! A flag is a licence the frontend grants the optimizer, and every one of them is tied to
//! something the C standard leaves undefined. `-fwrapv` is implemented by not setting
//! [`Flags::NSW`], and that is the whole of it.
//!
//! **There is no poison.** An `add nsw` that overflows does not produce a value that taints
//! everything downstream. It produces an unspecified but stable value, meaning two reads of it
//! agree, and `nsw` licenses only the specific rewrites the rule set proves sound under the
//! assumption that the overflow does not happen. The cost is real, and it is that arithmetic
//! cannot be speculated across control flow as aggressively. The benefit is that every rewrite
//! is locally justifiable, which is what keeps the rule set verifiable, and that a wrong answer
//! cannot travel from somewhere the user cannot see to somewhere they can.
//!
//! The fast-math flags sit on individual instructions rather than in a global mode, so
//! `-ffast-math` is a decision the frontend makes per expression. That is what keeps link time
//! optimization across a unit built with it and a unit built without it correct.

use std::fmt;

use crate::Opcode;

/// The flags on one instruction.
///
/// A bitset rather than a struct of `bool`s, because it rides along in the instruction table
/// and two bytes there is two bytes per instruction in every function in the program.
#[derive(Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Flags(u16);

impl Flags {
    /// No flags, which is what `-O0` and `-fwrapv` and a plain unsigned addition all produce.
    pub const NONE: Self = Self(0);

    /// No signed wrap. Signed overflow is undefined, so the optimizer may assume it does not
    /// happen. `-fwrapv` stops the frontend setting this and nothing else changes.
    pub const NSW: Self = Self(1 << 0);
    /// No unsigned wrap. Set only where the frontend knows it from the source, since C's
    /// unsigned arithmetic wraps by definition and most unsigned arithmetic does not get this.
    pub const NUW: Self = Self(1 << 1);
    /// The shift or division is exact, so no bits are discarded and no remainder is dropped.
    pub const EXACT: Self = Self(1 << 2);

    /// No NaN operands or results.
    pub const NNAN: Self = Self(1 << 3);
    /// No infinite operands or results.
    pub const NINF: Self = Self(1 << 4);
    /// The sign of a zero does not matter.
    pub const NSZ: Self = Self(1 << 5);
    /// A division may become a multiplication by the reciprocal.
    pub const ARCP: Self = Self(1 << 6);
    /// A multiplication and an addition may be contracted into one rounding.
    pub const CONTRACT: Self = Self(1 << 7);
    /// The operation may be reassociated, which is the one that changes results the most.
    pub const REASSOC: Self = Self(1 << 8);

    /// The access is `volatile`, so it happens exactly once and is never moved or merged.
    pub const VOLATILE: Self = Self(1 << 9);
    /// The result does not alias anything else reachable, which is what `restrict` gives.
    pub const NOALIAS: Self = Self(1 << 10);

    /// Every fast-math flag, which is what `-ffast-math` sets on an expression.
    pub const FAST: Self = Self(
        Self::NNAN.0
            | Self::NINF.0
            | Self::NSZ.0
            | Self::ARCP.0
            | Self::CONTRACT.0
            | Self::REASSOC.0,
    );

    /// The underlying bits, for the printer and for hashing an instruction.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Whether nothing is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every flag in `other` is set here.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Both sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The flags in both sets.
    ///
    /// This is what a rewrite does when it replaces two instructions with one: a licence
    /// granted on one of them and not the other is not a licence over the result.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// This set without the flags in `other`.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// The flags that mean anything on that opcode.
    ///
    /// Anything outside this is a verifier failure rather than something ignored, because a
    /// flag on an instruction that does not read it is a flag somebody meant to put somewhere
    /// else.
    #[must_use]
    pub const fn legal_on(opcode: Opcode) -> Self {
        match opcode {
            Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Shl => Self::NSW.union(Self::NUW),
            Opcode::SDiv | Opcode::UDiv | Opcode::LShr | Opcode::AShr => Self::EXACT,
            Opcode::FAdd
            | Opcode::FSub
            | Opcode::FMul
            | Opcode::FDiv
            | Opcode::FRem
            | Opcode::FNeg
            | Opcode::Fma
            | Opcode::FCmp => Self::FAST,
            Opcode::Load | Opcode::Store | Opcode::Memcpy | Opcode::Memmove | Opcode::Memset => {
                Self::VOLATILE
            }
            Opcode::InlineAsm => Self::VOLATILE,
            Opcode::Alloca | Opcode::PtrAdd => Self::NOALIAS,
            _ => Self::NONE,
        }
    }

    /// Every flag that is set, with its name, in the order the printer writes them.
    pub fn iter(self) -> impl Iterator<Item = (Self, &'static str)> {
        NAMED.iter().copied().filter(move |&(flag, _)| self.contains(flag))
    }

    /// The flag with that name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        NAMED.iter().find(|&&(_, named)| named == name).map(|&(flag, _)| flag)
    }
}

impl std::ops::BitOr for Flags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

impl std::ops::BitOrAssign for Flags {
    fn bitor_assign(&mut self, other: Self) {
        *self = self.union(other);
    }
}

impl fmt::Display for Flags {
    /// The suffix form the textual IR uses, `add.nsw`, with a leading dot on each flag and
    /// nothing at all when the set is empty.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (_, name) in self.iter() {
            write!(f, ".{name}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Flags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("Flags::NONE");
        }
        fmt::Display::fmt(self, f)
    }
}

/// Each flag with its name, in printing order.
static NAMED: &[(Flags, &str)] = &[
    (Flags::NSW, "nsw"),
    (Flags::NUW, "nuw"),
    (Flags::EXACT, "exact"),
    (Flags::NNAN, "nnan"),
    (Flags::NINF, "ninf"),
    (Flags::NSZ, "nsz"),
    (Flags::ARCP, "arcp"),
    (Flags::CONTRACT, "contract"),
    (Flags::REASSOC, "reassoc"),
    (Flags::VOLATILE, "volatile"),
    (Flags::NOALIAS, "noalias"),
];

/// How strongly an atomic operation is ordered against everything around it.
///
/// These are C11's, minus `consume`, which every compiler in existence widens to `acquire`
/// because nobody can implement it as specified and the standard committee has said so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum MemOrder {
    /// Not atomic at all, which is what an ordinary load or store is.
    #[default]
    NotAtomic,
    /// Atomic, with no ordering against anything else.
    Relaxed,
    /// Nothing after this in program order moves before it.
    Acquire,
    /// Nothing before this in program order moves after it.
    Release,
    /// Both, for a read-modify-write.
    AcqRel,
    /// Both, and a single total order over every sequentially consistent operation.
    SeqCst,
}

impl MemOrder {
    /// The textual form.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotAtomic => "not_atomic",
            Self::Relaxed => "relaxed",
            Self::Acquire => "acquire",
            Self::Release => "release",
            Self::AcqRel => "acq_rel",
            Self::SeqCst => "seq_cst",
        }
    }

    /// The ordering with that name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|order| order.name() == name)
    }

    /// Every ordering, weakest first.
    pub fn all() -> impl Iterator<Item = Self> {
        [Self::NotAtomic, Self::Relaxed, Self::Acquire, Self::Release, Self::AcqRel, Self::SeqCst]
            .into_iter()
    }

    /// Whether this ordering can be asked of a load.
    ///
    /// A load cannot release, because there is nothing it published.
    #[must_use]
    pub const fn is_valid_for_load(self) -> bool {
        matches!(self, Self::Relaxed | Self::Acquire | Self::SeqCst)
    }

    /// Whether this ordering can be asked of a store.
    ///
    /// A store cannot acquire, because it read nothing to synchronise with.
    #[must_use]
    pub const fn is_valid_for_store(self) -> bool {
        matches!(self, Self::Relaxed | Self::Release | Self::SeqCst)
    }

    /// Whether this ordering can be asked of a read-modify-write, which is any of them.
    #[must_use]
    pub const fn is_valid_for_rmw(self) -> bool {
        !matches!(self, Self::NotAtomic)
    }
}

impl fmt::Display for MemOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Which operation an `atomic_rmw` performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RmwOp {
    /// Replace, returning the old value.
    Xchg,
    /// Integer addition.
    Add,
    /// Integer subtraction.
    Sub,
    /// Bitwise and.
    And,
    /// Bitwise and, then complement, which is the one hardware sometimes has natively.
    Nand,
    /// Bitwise or.
    Or,
    /// Bitwise exclusive or.
    Xor,
    /// Signed maximum.
    SMax,
    /// Signed minimum.
    SMin,
    /// Unsigned maximum.
    UMax,
    /// Unsigned minimum.
    UMin,
    /// Floating point addition.
    FAdd,
    /// Floating point subtraction.
    FSub,
}

impl RmwOp {
    /// The textual form.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Xchg => "xchg",
            Self::Add => "add",
            Self::Sub => "sub",
            Self::And => "and",
            Self::Nand => "nand",
            Self::Or => "or",
            Self::Xor => "xor",
            Self::SMax => "smax",
            Self::SMin => "smin",
            Self::UMax => "umax",
            Self::UMin => "umin",
            Self::FAdd => "fadd",
            Self::FSub => "fsub",
        }
    }

    /// The operation with that name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|op| op.name() == name)
    }

    /// Every operation.
    pub fn all() -> impl Iterator<Item = Self> {
        [
            Self::Xchg,
            Self::Add,
            Self::Sub,
            Self::And,
            Self::Nand,
            Self::Or,
            Self::Xor,
            Self::SMax,
            Self::SMin,
            Self::UMax,
            Self::UMin,
            Self::FAdd,
            Self::FSub,
        ]
        .into_iter()
    }

    /// Whether this operates on a floating point value rather than an integer.
    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self, Self::FAdd | Self::FSub)
    }
}

impl fmt::Display for RmwOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What kind of storage a memory safety instance is, which is `class` of
/// `spec/safe-memory/04-safety-model.md` section 4.1.
///
/// It is on `meta_begin` because judgement J4 writes it when the instance is created, and the
/// one place it is read afterwards is J6: `free` is permitted on an allocated instance and on
/// no other kind, which is what makes freeing a stack address a report rather than a crash in
/// the allocator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StorageClass {
    /// A global or a static local, which lives as long as the program does.
    Static,
    /// A local, which lives as long as its block does.
    Automatic,
    /// Storage an allocator handed out, and the only kind `free` may be given.
    Allocated,
    /// A mapping, from `mmap` or its equivalent.
    Mapped,
    /// A device register window, where a read is not a read of anything the program wrote.
    Mmio,
    /// Storage a device owns, which is what a DMA buffer is while the transfer runs.
    Device,
    /// A function, which is what the address of one points at.
    Function,
    /// A string or compound literal, which the implementation may have merged with another.
    Literal,
}

impl StorageClass {
    /// The textual form.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Automatic => "automatic",
            Self::Allocated => "allocated",
            Self::Mapped => "mapped",
            Self::Mmio => "mmio",
            Self::Device => "device",
            Self::Function => "function",
            Self::Literal => "literal",
        }
    }

    /// The class with that name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|class| class.name() == name)
    }

    /// Every class, in the order document 04 lists them.
    pub fn all() -> impl Iterator<Item = Self> {
        [
            Self::Static,
            Self::Automatic,
            Self::Allocated,
            Self::Mapped,
            Self::Mmio,
            Self::Device,
            Self::Function,
            Self::Literal,
        ]
        .into_iter()
    }
}

impl fmt::Display for StorageClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Who a range of memory belongs to while it is out of the monitor's authority.
///
/// Judgement J7 of `spec/safe-memory/04-safety-model.md`, which is the one that has no analogue
/// in any existing tool. A range handed to a device is a range the program must not touch until
/// it comes back, and saying which of the three it went to is what lets the report name what the
/// program broke rather than only that it broke something.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Owner {
    /// A device, which is what the DMA ownership contract hands a buffer to.
    Device,
    /// Code compiled without the instrumentation, per document 10.
    Uninstrumented,
    /// The kernel, across a system call that writes into the range.
    Kernel,
}

impl Owner {
    /// The textual form.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Uninstrumented => "uninstrumented",
            Self::Kernel => "kernel",
        }
    }

    /// The owner with that name, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|owner| owner.name() == name)
    }

    /// Every owner.
    pub fn all() -> impl Iterator<Item = Self> {
        [Self::Device, Self::Uninstrumented, Self::Kernel].into_iter()
    }
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_set_is_two_bytes() {
        assert_eq!(size_of::<Flags>(), 2);
    }

    #[test]
    fn every_flag_has_a_name_and_finds_it_again() {
        for &(flag, name) in NAMED {
            assert_eq!(Flags::from_name(name), Some(flag), "{name}");
            assert_eq!(flag.to_string(), format!(".{name}"));
        }
        assert_eq!(Flags::from_name("poison"), None);
        assert_eq!(Flags::from_name(""), None);
    }

    #[test]
    fn no_two_flags_share_a_bit() {
        let mut seen = 0u16;
        for &(flag, name) in NAMED {
            assert_eq!(flag.bits().count_ones(), 1, "{name} is not one bit");
            assert_eq!(seen & flag.bits(), 0, "{name} shares a bit");
            seen |= flag.bits();
        }
    }

    #[test]
    fn fast_is_exactly_the_six_fast_math_flags() {
        let named: Vec<&str> = Flags::FAST.iter().map(|(_, name)| name).collect();
        assert_eq!(named, ["nnan", "ninf", "nsz", "arcp", "contract", "reassoc"]);
        assert!(!Flags::FAST.contains(Flags::NSW));
        assert!(!Flags::FAST.contains(Flags::VOLATILE));
    }

    #[test]
    fn the_empty_set_prints_as_nothing() {
        assert!(Flags::NONE.is_empty());
        assert_eq!(Flags::NONE.to_string(), "");
        assert_eq!(Flags::NONE.iter().count(), 0);
    }

    #[test]
    fn flags_print_as_the_suffix_the_textual_form_uses() {
        assert_eq!((Flags::NSW | Flags::NUW).to_string(), ".nsw.nuw");
        // Whatever order they were combined in, the printer writes them in one order, which
        // is what a byte for byte round trip needs.
        assert_eq!((Flags::NUW | Flags::NSW).to_string(), ".nsw.nuw");
    }

    #[test]
    fn intersecting_is_what_a_rewrite_keeps() {
        let one = Flags::NSW | Flags::NUW;
        let other = Flags::NSW;
        assert_eq!(one.intersection(other), Flags::NSW);
        assert_eq!(one.without(Flags::NSW), Flags::NUW);
        assert!(one.contains(Flags::NSW));
        assert!(!other.contains(Flags::NUW));
    }

    #[test]
    fn wrapping_flags_go_on_arithmetic_and_nowhere_else() {
        assert!(Flags::legal_on(Opcode::Add).contains(Flags::NSW));
        assert!(Flags::legal_on(Opcode::Shl).contains(Flags::NUW));
        assert!(!Flags::legal_on(Opcode::Add).contains(Flags::EXACT));
        assert!(!Flags::legal_on(Opcode::FAdd).contains(Flags::NSW));
        assert!(!Flags::legal_on(Opcode::Load).contains(Flags::NSW));
        assert!(Flags::legal_on(Opcode::SDiv).contains(Flags::EXACT));
        assert!(Flags::legal_on(Opcode::FMul).contains(Flags::CONTRACT));
        assert!(Flags::legal_on(Opcode::Store).contains(Flags::VOLATILE));
        assert!(Flags::legal_on(Opcode::Jump).is_empty());
    }

    #[test]
    fn every_flag_is_legal_on_something() {
        for &(flag, name) in NAMED {
            assert!(
                Opcode::all().any(|op| Flags::legal_on(op).contains(flag)),
                "{name} is legal nowhere, so nothing can ever set it"
            );
        }
    }

    #[test]
    fn a_load_cannot_release_and_a_store_cannot_acquire() {
        assert!(MemOrder::Acquire.is_valid_for_load());
        assert!(!MemOrder::Release.is_valid_for_load());
        assert!(!MemOrder::AcqRel.is_valid_for_load());
        assert!(MemOrder::Release.is_valid_for_store());
        assert!(!MemOrder::Acquire.is_valid_for_store());
        assert!(MemOrder::SeqCst.is_valid_for_load());
        assert!(MemOrder::SeqCst.is_valid_for_store());
    }

    #[test]
    fn not_atomic_is_valid_for_no_atomic_operation() {
        assert!(!MemOrder::NotAtomic.is_valid_for_load());
        assert!(!MemOrder::NotAtomic.is_valid_for_store());
        assert!(!MemOrder::NotAtomic.is_valid_for_rmw());
        assert_eq!(MemOrder::default(), MemOrder::NotAtomic);
    }

    #[test]
    fn every_ordering_and_operation_finds_its_name_again() {
        for order in MemOrder::all() {
            assert_eq!(MemOrder::from_name(order.name()), Some(order));
        }
        for op in RmwOp::all() {
            assert_eq!(RmwOp::from_name(op.name()), Some(op));
        }
        assert_eq!(MemOrder::from_name("consume"), None);
        assert_eq!(RmwOp::from_name("fmul"), None);
    }

    #[test]
    fn the_floating_read_modify_writes_are_the_two_that_have_one() {
        let floats: Vec<&str> = RmwOp::all().filter(|op| op.is_float()).map(RmwOp::name).collect();
        assert_eq!(floats, ["fadd", "fsub"]);
    }

    #[test]
    fn every_storage_class_and_owner_finds_its_name_again() {
        for class in StorageClass::all() {
            assert_eq!(StorageClass::from_name(class.name()), Some(class));
        }
        for owner in Owner::all() {
            assert_eq!(Owner::from_name(owner.name()), Some(owner));
        }
        // The eight of document 04 and no more. `heap` is what a reader would guess and the
        // model does not have it, since what the allocator hands out is `allocated`.
        assert_eq!(StorageClass::all().count(), 8);
        assert_eq!(StorageClass::from_name("heap"), None);
        assert_eq!(Owner::from_name("hardware"), None);
    }
}
