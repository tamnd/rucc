//! What a C type is made of, before any of it has been interned.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.1.
//!
//! Everything here is `Copy` and small, because [`TypeKind`] is the interning key and a key
//! that owns a heap allocation cannot be hashed cheaply or compared cheaply. The two parts of
//! a type that are genuinely variable length, a function's parameter list and a record's
//! members, live in side tables and are referred to by index.

use rucc_base::Symbol;

use crate::TypeId;

/// The qualifiers a type can carry.
///
/// A bitmask in the interning key rather than a chain of wrapper nodes, so `const int` is one
/// entry in the table beside `int` rather than a node pointing at it. That makes stripping
/// qualifiers a field read instead of a walk, which matters because almost every semantic rule
/// in C is stated on the unqualified type.
///
/// `_Atomic` is deliberately not here. C lets it be written in the same position as a
/// qualifier, but `_Atomic(T)` is a different type from `T` with its own size and alignment,
/// so it is a type constructor, [`TypeKind::Atomic`], and the parser is what maps the
/// qualifier spelling onto it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord)]
pub struct Qualifiers(u8);

impl Qualifiers {
    /// No qualifiers.
    pub const NONE: Qualifiers = Qualifiers(0);
    /// `const`.
    pub const CONST: Qualifiers = Qualifiers(1);
    /// `volatile`.
    pub const VOLATILE: Qualifiers = Qualifiers(2);
    /// `restrict`.
    pub const RESTRICT: Qualifiers = Qualifiers(4);

    /// Whether every qualifier in `other` is present here.
    #[inline]
    #[must_use]
    pub const fn has(self, other: Qualifiers) -> bool {
        self.0 & other.0 == other.0
    }

    /// This set with `other` added.
    #[inline]
    #[must_use]
    pub const fn with(self, other: Qualifiers) -> Qualifiers {
        Qualifiers(self.0 | other.0)
    }

    /// This set with `other` removed.
    #[inline]
    #[must_use]
    pub const fn without(self, other: Qualifiers) -> Qualifiers {
        Qualifiers(self.0 & !other.0)
    }

    /// Whether there are no qualifiers at all.
    #[inline]
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// The standard integer types, the character types kept apart from them, and `__int128`.
///
/// `Char` is its own kind rather than an alias for one of the other two. The standard makes
/// plain `char` a third type distinct from both `signed char` and `unsigned char` even though
/// it has the same range as one of them, and a compiler that folds it into whichever one the
/// target picked gets `char *` and `signed char *` wrongly deemed compatible.
///
/// `__int128` is here rather than modelled as a `_BitInt(128)`, because the two are different
/// types with different layouts: `__int128` is sixteen bytes aligned to sixteen on every
/// target we have, and `_BitInt(128)` is aligned to its granule, which is eight on x86-64. It
/// is available everywhere for us, since all three architectures are 64-bit, and GCC has it
/// on every 64-bit target. It is deliberately not an extended integer type in the sense the
/// standard means, which is what keeps `intmax_t` sixty four bits wide the way GCC has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IntKind {
    /// `char`, whose signedness is a target property.
    Char,
    /// `signed char`.
    SChar,
    /// `unsigned char`.
    UChar,
    /// `short`.
    Short,
    /// `unsigned short`.
    UShort,
    /// `int`.
    Int,
    /// `unsigned int`.
    UInt,
    /// `long`, the width that separates LP64 from Windows LLP64.
    Long,
    /// `unsigned long`.
    ULong,
    /// `long long`.
    LongLong,
    /// `unsigned long long`.
    ULongLong,
    /// `__int128`.
    Int128,
    /// `unsigned __int128`.
    UInt128,
}

impl IntKind {
    /// Every integer kind, in rank order, with `__int128` last.
    ///
    /// The order is what the internal index agrees with, and it is also the order the standard
    /// walks when it picks the type of an integer constant, so a table walk over the candidate
    /// list for a suffix is a walk over a slice of this. `__int128` is at the end because that
    /// is where GCC reaches for it: after every standard type has been tried and none of them
    /// was wide enough.
    pub const ALL: [IntKind; 13] = [
        IntKind::Char,
        IntKind::SChar,
        IntKind::UChar,
        IntKind::Short,
        IntKind::UShort,
        IntKind::Int,
        IntKind::UInt,
        IntKind::Long,
        IntKind::ULong,
        IntKind::LongLong,
        IntKind::ULongLong,
        IntKind::Int128,
        IntKind::UInt128,
    ];

    /// A dense index, so that one of these can select a slot in a fixed size array.
    pub(crate) const fn index(self) -> usize {
        match self {
            IntKind::Char => 0,
            IntKind::SChar => 1,
            IntKind::UChar => 2,
            IntKind::Short => 3,
            IntKind::UShort => 4,
            IntKind::Int => 5,
            IntKind::UInt => 6,
            IntKind::Long => 7,
            IntKind::ULong => 8,
            IntKind::LongLong => 9,
            IntKind::ULongLong => 10,
            IntKind::Int128 => 11,
            IntKind::UInt128 => 12,
        }
    }

    /// Whether this type is signed, given what the target says about plain `char`.
    ///
    /// The argument is there because `char` is the one integer type whose signedness is not
    /// in the standard. It is signed on x86-64 and unsigned on AArch64 Linux, and a compiler
    /// that assumes either one is the source of a whole genre of bug report.
    #[must_use]
    pub const fn is_signed(self, char_is_signed: bool) -> bool {
        match self {
            IntKind::Char => char_is_signed,
            IntKind::SChar
            | IntKind::Short
            | IntKind::Int
            | IntKind::Long
            | IntKind::LongLong
            | IntKind::Int128 => true,
            IntKind::UChar
            | IntKind::UShort
            | IntKind::UInt
            | IntKind::ULong
            | IntKind::ULongLong
            | IntKind::UInt128 => false,
        }
    }

    /// The integer conversion rank, as an ordering rather than as a number from the standard.
    ///
    /// The standard gives no values, only a set of relations, and every one of them is a
    /// comparison between two ranks. Signed and unsigned of the same width share a rank, which
    /// is what makes the usual arithmetic conversions between them pick the unsigned type
    /// rather than the wider one.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            IntKind::Char | IntKind::SChar | IntKind::UChar => 1,
            IntKind::Short | IntKind::UShort => 2,
            IntKind::Int | IntKind::UInt => 3,
            IntKind::Long | IntKind::ULong => 4,
            IntKind::LongLong | IntKind::ULongLong => 5,
            // Above `long long`, which is what makes `__int128 + unsigned long long` an
            // `__int128` rather than an unsigned type. Both compilers agree.
            IntKind::Int128 | IntKind::UInt128 => 6,
        }
    }

    /// The same width with the other signedness.
    ///
    /// `char` maps to `unsigned char` and back to `signed char`, which is the mapping the
    /// usual arithmetic conversions need and is not a round trip. That asymmetry is the type
    /// system telling the truth: there is no way back to plain `char` from either of the
    /// other two.
    #[must_use]
    pub const fn flip_sign(self) -> IntKind {
        match self {
            IntKind::Char | IntKind::SChar => IntKind::UChar,
            IntKind::UChar => IntKind::SChar,
            IntKind::Short => IntKind::UShort,
            IntKind::UShort => IntKind::Short,
            IntKind::Int => IntKind::UInt,
            IntKind::UInt => IntKind::Int,
            IntKind::Long => IntKind::ULong,
            IntKind::ULong => IntKind::Long,
            IntKind::LongLong => IntKind::ULongLong,
            IntKind::ULongLong => IntKind::LongLong,
            IntKind::Int128 => IntKind::UInt128,
            IntKind::UInt128 => IntKind::Int128,
        }
    }

    /// How the type is spelled in a diagnostic.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            IntKind::Char => "char",
            IntKind::SChar => "signed char",
            IntKind::UChar => "unsigned char",
            IntKind::Short => "short",
            IntKind::UShort => "unsigned short",
            IntKind::Int => "int",
            IntKind::UInt => "unsigned int",
            IntKind::Long => "long",
            IntKind::ULong => "unsigned long",
            IntKind::LongLong => "long long",
            IntKind::ULongLong => "unsigned long long",
            IntKind::Int128 => "__int128",
            IntKind::UInt128 => "unsigned __int128",
        }
    }
}

/// The real floating types.
///
/// The decimal floating types from C23 are deferred past 1.0 by `spec/19-open-questions.md`
/// and are deliberately absent rather than present and unimplemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FloatKind {
    /// `float`, always the binary32 format.
    Float,
    /// `double`, always the binary64 format.
    Double,
    /// `long double`, whose format is a target property and is not always distinct from
    /// `double`. It is 80 bits of x87 on SysV x86-64, quad precision on AArch64 Linux, and
    /// the same as `double` on Apple and Windows.
    LongDouble,
}

impl FloatKind {
    /// Every real floating type, in rank order.
    pub const ALL: [FloatKind; 3] = [FloatKind::Float, FloatKind::Double, FloatKind::LongDouble];

    /// A dense index, so that one of these can select a slot in a fixed size array.
    pub(crate) const fn index(self) -> usize {
        match self {
            FloatKind::Float => 0,
            FloatKind::Double => 1,
            FloatKind::LongDouble => 2,
        }
    }

    /// The conversion rank, which for floating types is just the ordering.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            FloatKind::Float => 1,
            FloatKind::Double => 2,
            FloatKind::LongDouble => 3,
        }
    }

    /// How the type is spelled in a diagnostic.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            FloatKind::Float => "float",
            FloatKind::Double => "double",
            FloatKind::LongDouble => "long double",
        }
    }
}

/// How many elements an array has, which is four different answers in C.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ArrayLen {
    /// `int a[4]`. The count of elements, not the size in bytes.
    Fixed(u64),
    /// `int a[]`, an incomplete array type. It has an element type and no size, and it is
    /// completed by an initializer or by a later declaration.
    Unknown,
    /// `int a[*]`, a variably modified type in a prototype, where the size exists but is not
    /// available to the declaration that mentions it.
    Star,
    /// `int a[n]`, a variable length array. The size expression stays in the AST, and the
    /// type carries only the identity of the one that made it, because two variable length
    /// arrays written with the same element type are still distinct types.
    Variable(VlaId),
}

/// The identity of one variable length array's size expression.
///
/// An opaque number handed out by whoever is building the type, which in practice is
/// semantic analysis walking a declarator. This crate never looks inside it; it is here so
/// that interning two variable length arrays does not accidentally make them the same type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VlaId(pub u32);

/// Whether a record is a `struct` or a `union`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RecordKind {
    /// `struct`, whose members are laid out one after another.
    Struct,
    /// `union`, whose members all start at offset zero.
    Union,
}

impl RecordKind {
    /// How the keyword is spelled in a diagnostic.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RecordKind::Struct => "struct",
            RecordKind::Union => "union",
        }
    }
}

/// What a type is, with its qualifiers stripped off into [`Type::quals`].
///
/// This is `Copy` and sixteen bytes, which is what lets it be the interning key directly.
/// Function types and record types are the two that carry a variable amount of information,
/// and both of them are an index into a table this crate owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeKind {
    /// `void`.
    Void,
    /// `bool`, which C23 spells without an underscore and which is one byte with two values.
    Bool,
    /// One of the standard integer types.
    Int(IntKind),
    /// One of the real floating types.
    Float(FloatKind),
    /// `_Complex T` for a real floating `T`.
    Complex(FloatKind),
    /// `_BitInt(N)` and `unsigned _BitInt(N)`.
    ///
    /// A distinct kind rather than an integer type with a width, because these do not take
    /// part in the integer promotions and folding them in with the standard types is how
    /// that rule gets forgotten.
    BitInt {
        /// Whether the type is signed. A signed `_BitInt(1)` is legal and holds `0` and `-1`.
        signed: bool,
        /// The declared width in bits, which is what the standard calls `N`.
        width: u32,
    },
    /// A pointer to the given type.
    Pointer(TypeId),
    /// `_Atomic(T)`, which is a type and not a qualifier. See [`Qualifiers`].
    Atomic(TypeId),
    /// An array of the given element type.
    Array {
        /// The element type.
        elem: TypeId,
        /// How many of them there are, which may be unknown.
        len: ArrayLen,
    },
    /// A function type, whose parameter list is in this crate's side table.
    Function(FunctionId),
    /// A GNU vector type, `__attribute__((vector_size(n)))`.
    Vector {
        /// The element type, which must be a scalar.
        elem: TypeId,
        /// How many elements there are.
        len: u32,
    },
    /// A `struct` or `union`, identified by its declaration rather than by its members.
    Record(RecordId),
    /// An `enum`, identified by its declaration.
    Enum(EnumId),
    /// A typedef name, which is sugar over whatever it was declared as.
    ///
    /// Every semantic decision reads [`Types::canonical`](crate::Types::canonical) and never
    /// sees this; every diagnostic reads the type as written and sees nothing else, so the
    /// error says `size_t` rather than `unsigned long`. Compilers that drop the sugar produce
    /// messages nobody can act on, and compilers that decide on the sugar produce wrong
    /// answers, and both are common.
    Typedef {
        /// The name, for printing.
        name: Symbol,
        /// What it was declared as.
        underlying: TypeId,
    },
}

/// A type with its qualifiers, which together are one entry in the type table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Type {
    /// What the type is.
    pub kind: TypeKind,
    /// What it is qualified with.
    pub quals: Qualifiers,
}

impl Type {
    /// An unqualified type of the given kind.
    #[must_use]
    pub const fn new(kind: TypeKind) -> Type {
        Type { kind, quals: Qualifiers::NONE }
    }
}

/// The identity of a function type in [`Types`](crate::Types).
///
/// Deduplicated by content, so two declarations written with the same return type, the same
/// parameters and the same variadic flag share one of these and therefore one [`TypeId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FunctionId(pub(crate) u32);

/// The identity of a `struct` or `union` declaration in [`Types`](crate::Types).
///
/// Not deduplicated by content, because record types in C are nominal. Two `struct` types
/// written with the same members in the same translation unit are different types, and the
/// looser relation that does hold between them is compatibility rather than identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordId(pub(crate) u32);

/// The identity of an `enum` declaration in [`Types`](crate::Types).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnumId(pub(crate) u32);

/// A function type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionType {
    /// What it returns.
    pub ret: TypeId,
    /// The parameter types, after the adjustments a parameter declaration gets: an array
    /// parameter has already decayed to a pointer and a function parameter to a function
    /// pointer, because those adjustments are part of forming the type and not part of
    /// calling it.
    pub params: Vec<TypeId>,
    /// Whether the list ends in `...`.
    pub variadic: bool,
    /// Whether there was a prototype at all.
    ///
    /// `int f()` declares an unprototyped function before C23 and a function taking no
    /// arguments from C23 onwards, and the difference is visible in what calls are checked
    /// and in what the composite type of a redeclaration is. The dialect decides which
    /// meaning `()` gets, and this records the decision rather than repeating it.
    pub prototyped: bool,
}
