//! What a classifier is asked about, and what it answers.
//!
//! Design: `spec/cross-compile/06-abis.md` sections 6.2 and 6.7.
//!
//! Everything in this module is deliberately not C. A psABI does not read a C type, it reads a
//! size, an alignment, and where the scalars inside are and whether each one is an integer or a
//! floating point value. Flattening a C type down to that is the compiler's job, because that
//! is where the C type system lives, and every rule after it is the target's.
//!
//! Keeping the boundary there is what lets the descriptions in [`crate::abis`] be data. A rule
//! written over "the members of the struct" would have to know about unions, arrays, bit-fields
//! and anonymous members. A rule written over a flat list of scalars at offsets does not, and
//! every psABI on `spec/cross-compile/06-abis.md` section 6.1's list turns out to be expressible over the flat
//! list.

/// A floating point format.
///
/// The width and the format are separate facts, which is the trap in `spec/cross-compile/06-abis.md` section
/// 6.2 item 1. An x87 `long double` is eighty bits of value stored in twelve bytes on i386 and
/// sixteen on x86-64, and a `long double` on AArch64 Linux is a different format entirely at the
/// same sixteen bytes. A rule written over the width alone gets both wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Format {
    /// IEEE binary16, which C spells `_Float16`.
    Half,
    /// The brain float: an IEEE binary32 with the low sixteen bits of the significand cut off,
    /// which C spells `__bf16`. A `float`'s range with less than half its precision.
    BFloat16,
    /// IEEE binary32, which C spells `float`.
    Single,
    /// IEEE binary64, which C spells `double`.
    Double,
    /// The x87 eighty bit format, which is `long double` on x86. The one format here that stores
    /// the leading significand bit rather than leaving it implied.
    X87Extended,
    /// IEEE binary128, which C spells `_Float128` and which is `long double` on AArch64 Linux,
    /// on s390x and on RISC-V.
    Quad,
    /// IBM double-double, a pair of `double`s whose sum is the value, which is `long double` on
    /// legacy 64-bit PowerPC. Not a binary floating point format in the IEEE sense at all.
    DoubleDouble,
}

impl Format {
    /// The short name it is written under, which is its width in bits except for the two that
    /// the width does not tell apart from something else.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Half => "f16",
            Self::BFloat16 => "bf16",
            Self::Single => "f32",
            Self::Double => "f64",
            Self::X87Extended => "f80",
            Self::Quad => "f128",
            Self::DoubleDouble => "ppc-f128",
        }
    }
}

/// What a scalar is, once the ABI is the one asking.
///
/// Signedness is not here. Every ABI in `spec/cross-compile/06-abis.md` section 6.1 passes a value of a given
/// width the same way whichever end of the range it sits at, and the widening a narrow argument
/// gets on the way into a register is a property of the call rather than of the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An integer, an enumeration, a `bool` or a pointer.
    Integer,
    /// A floating point value in this format.
    Float(Format),
}

/// One scalar, with the three facts a psABI reads about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scalar {
    /// Whether it is an integer or a floating point value.
    pub kind: Kind,
    /// How many bytes it takes in memory, which is the target's answer and not the format's.
    pub size: u64,
    /// What it is aligned to, in bytes.
    ///
    /// One for a bit-field, which may start anywhere and is an integer wherever it starts. That
    /// distinction earns its keep on SysV, where an ordinary member away from its natural
    /// alignment sends the whole aggregate to memory and a bit-field straddling an eightbyte
    /// does not.
    pub align: u64,
}

impl Scalar {
    /// An integer of this size, aligned to itself.
    #[must_use]
    pub const fn integer(size: u64) -> Self {
        Self { kind: Kind::Integer, size, align: size }
    }

    /// A floating point value in this format, of this size, aligned to itself.
    #[must_use]
    pub const fn float(format: Format, size: u64) -> Self {
        Self { kind: Kind::Float(format), size, align: size }
    }

    /// Whether it is a floating point value.
    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self.kind, Kind::Float(_))
    }
}

/// One scalar inside an aggregate, at the offset the layout gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    /// Where it starts, in bytes from the start of the aggregate.
    pub offset: u64,
    /// What it is.
    pub scalar: Scalar,
}

impl Piece {
    /// One past the last byte it covers.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.offset + if self.scalar.size == 0 { 1 } else { self.scalar.size }
    }
}

/// An aggregate, as much of it as an ABI cares about.
///
/// The pieces are every scalar in it with arrays and nested records flattened out, in offset
/// order. Padding is not a piece: a hole is described by the offsets on either side of it, which
/// is the form every classification rule is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape<'a> {
    /// The size of the whole thing in bytes, padding included.
    pub size: u64,
    /// What it is aligned to, in bytes.
    pub align: u64,
    /// The scalars in it.
    pub pieces: &'a [Piece],
    /// Whether it is a `_Complex` rather than a `struct` or a `union` of the same shape.
    ///
    /// Exactly one rule reads this, and it is on SysV AMD64, where `_Complex long double` comes
    /// back on the x87 stack and `struct { long double a, b; }`, which is the same thirty two
    /// bytes with the same two members in the same places, comes back in memory. Without the
    /// flag there is no way to tell those apart from the shape, and they are passed differently.
    pub complex: bool,
}

impl Shape<'_> {
    /// Whether it holds at least one scalar and every one of them is this format.
    #[must_use]
    pub fn is_all_of(&self, format: Format) -> bool {
        !self.pieces.is_empty()
            && self.pieces.iter().all(|piece| piece.scalar.kind == Kind::Float(format))
    }
}

/// One argument, or one return value, as much of it as an ABI cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg<'a> {
    /// `void`, which is a return type and never an argument.
    Void,
    /// A scalar.
    Scalar(Scalar),
    /// A `struct`, a `union`, an array or a `_Complex`.
    Aggregate(Shape<'a>),
}

/// One register's worth of an aggregate that travels in registers, and which of the object's
/// bytes go in it.
///
/// A slot says what the object's bytes are read as rather than what the program wrote into them.
/// An eightbyte holding two `float`s is a [`Slot::Float`] of [`Format::Double`], because eight
/// bytes of floating point data arrive in one vector register whichever way the program divided
/// them up and the bits are the same either way.
///
/// The offset is carried rather than derived because it cannot be worked out from the run of
/// slots. Two eightbytes are at zero and eight, four `float`s of a homogeneous aggregate are
/// four bytes apart, and `struct { double value; int tag; }` on RISC-V travels in one floating
/// point and one integer register whose bytes are at zero and eight, where the second is not
/// where the first one ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// An integer this many bytes wide, which is one general purpose register.
    ///
    /// The last slot of an aggregate is only as wide as what is left of it, so a twelve byte
    /// structure is eight bytes and then four and nothing reads a byte past the object.
    Integer {
        /// Where its bytes start in the object.
        offset: u64,
        /// How many of them there are.
        size: u32,
    },
    /// A floating point value in this format, which is one vector register.
    Float {
        /// Where its bytes start in the object.
        offset: u64,
        /// What is read out of them.
        format: Format,
    },
}

impl Slot {
    /// Where its bytes start in the object.
    #[must_use]
    pub const fn offset(self) -> u64 {
        match self {
            Self::Integer { offset, .. } | Self::Float { offset, .. } => offset,
        }
    }

    /// Whether it is a vector register rather than a general purpose one.
    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self, Self::Float { .. })
    }
}

/// How one value travels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pass {
    /// Nothing travels, which is `void` and an aggregate of no size.
    Ignore,
    /// The value itself. Every scalar is this.
    Direct,
    /// The object's bytes, in these slots, which is what passing an aggregate in registers means
    /// once the object has been taken apart.
    Pieces(Vec<Slot>),
    /// The address of a copy, in the place the value itself would have gone.
    ///
    /// For an argument the caller makes the copy. For a return value the caller passes the
    /// address of somewhere to put it, which is the hidden first argument.
    Reference,
    /// The object's own bytes in the argument area, with no address anywhere.
    ///
    /// SysV's MEMORY class and AAPCS's aggregate that ran out of registers. Never a return
    /// value: a return value that does not fit in registers is [`Pass::Reference`].
    Memory,
}
