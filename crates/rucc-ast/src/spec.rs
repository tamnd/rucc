//! Declaration specifiers: what a declaration says before the declarator.
//!
//! Design: `spec/06-lexer-and-parser.md` sections 6.5 and 6.6.
//!
//! A declaration is a pile of specifiers followed by a list of declarators, and the specifiers
//! may be written in any order: `unsigned static const long int` is a legal, if unpleasant,
//! spelling of `static const unsigned long`. So they are accumulated into a record rather than
//! kept as a sequence, with one exception. The keywords that name a built-in type are kept as
//! the multiset that was written, in [`Builtin`], and turned into a type by [`Builtin::resolve`]
//! rather than at the moment they are read, because `long` on its own and `long` before `int`
//! and `long` after `long` are the same keyword doing three different jobs.
//!
//! [`DeclSpecs`] lives in a side table and is not size-capped, unlike the arena nodes.

use rucc_base::Symbol;
use rucc_diag::Span;

use crate::ast::{AttrList, EnumeratorList, MemberList};
use crate::decl::TypeNameId;
use crate::expr::ExprId;

/// A set of declaration specifiers, in the side table.
pub type DeclSpecsId = rucc_base::Idx<DeclSpecs>;

/// Everything a declaration says before its first declarator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclSpecs {
    /// The storage class, of which there may be at most one.
    pub storage: Option<StorageClass>,
    /// Whether `_Thread_local` was written, which is separate because it is a storage class
    /// specifier that may be combined with another.
    pub thread_local: bool,
    /// Whether `constexpr` was written, which is separate for the same reason: C23 6.7.1 lets
    /// it stand beside `auto`, `register` or `static`, and `static constexpr int x = 1;` is a
    /// declaration people write.
    pub constexpr: bool,
    /// What type was named.
    pub ty: TypeSpec,
    /// The qualifiers, which may be written before or after the type.
    pub quals: Quals,
    /// `inline` and `_Noreturn`.
    pub func: FuncSpecs,
    /// `alignas`, of which there may be several, of which the strictest wins. Only the first is
    /// kept here; the rest are in the side table alongside it.
    pub align: Option<AlignSpec>,
    /// Attributes that appertain to the declaration as a whole.
    pub attrs: AttrList,
    /// From the first specifier to the last.
    pub span: Span,
}

impl DeclSpecs {
    /// A specifier list with nothing in it, which is what the parser starts from.
    #[must_use]
    pub const fn empty(span: Span) -> DeclSpecs {
        DeclSpecs {
            storage: None,
            thread_local: false,
            constexpr: false,
            ty: TypeSpec::None,
            quals: Quals::NONE,
            func: FuncSpecs::NONE,
            align: None,
            attrs: AttrList::EMPTY,
            span,
        }
    }

    /// Whether this declares type names rather than objects.
    #[must_use]
    pub const fn is_typedef(&self) -> bool {
        matches!(self.storage, Some(StorageClass::Typedef))
    }

    /// Which spelling asked for a type deduced from an initializer, if either did.
    #[must_use]
    pub const fn deduces(&self) -> Option<Deduction> {
        match self.ty {
            TypeSpec::Auto(which) => Some(which),
            _ => None,
        }
    }
}

/// A storage class specifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageClass {
    /// `typedef`, which the grammar treats as a storage class and which declares no object.
    Typedef,
    /// `extern`.
    Extern,
    /// `static`.
    Static,
    /// `auto`, the old one that means nothing, not the C23 type specifier.
    Auto,
    /// `register`.
    Register,
}

impl StorageClass {
    /// The keyword, for the printer and for diagnostics.
    #[must_use]
    pub const fn spelling(self) -> &'static str {
        match self {
            StorageClass::Typedef => "typedef",
            StorageClass::Extern => "extern",
            StorageClass::Static => "static",
            StorageClass::Auto => "auto",
            StorageClass::Register => "register",
        }
    }
}

/// The type qualifiers, as a set.
///
/// `_Atomic` is here because it can be written in qualifier position, where it qualifies
/// whatever the declarator arrives at. `_Atomic(T)`, with parentheses, is a different thing and
/// is [`TypeSpec::Atomic`], because it constructs a type that may not have the same alignment
/// as `T`. Both spellings mean the same in the end and the difference matters to the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Quals(u8);

impl Quals {
    /// No qualifiers.
    pub const NONE: Quals = Quals(0);
    /// `const`.
    pub const CONST: Quals = Quals(1);
    /// `volatile`.
    pub const VOLATILE: Quals = Quals(2);
    /// `restrict`, which is only a keyword from C99.
    pub const RESTRICT: Quals = Quals(4);
    /// `_Atomic` without parentheses.
    pub const ATOMIC: Quals = Quals(8);

    /// Whether every qualifier in `other` is set here.
    #[inline]
    #[must_use]
    pub const fn has(self, other: Quals) -> bool {
        self.0 & other.0 == other.0
    }

    /// This set with `other` added.
    #[inline]
    #[must_use]
    pub const fn with(self, other: Quals) -> Quals {
        Quals(self.0 | other.0)
    }

    /// Whether nothing is qualified.
    #[inline]
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// The function specifiers, as a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FuncSpecs(u8);

impl FuncSpecs {
    /// Neither.
    pub const NONE: FuncSpecs = FuncSpecs(0);
    /// `inline`.
    pub const INLINE: FuncSpecs = FuncSpecs(1);
    /// `_Noreturn`, which C23 deprecated in favour of the attribute and which every real
    /// header still uses.
    pub const NORETURN: FuncSpecs = FuncSpecs(2);

    /// Whether every specifier in `other` is set here.
    #[inline]
    #[must_use]
    pub const fn has(self, other: FuncSpecs) -> bool {
        self.0 & other.0 == other.0
    }

    /// This set with `other` added.
    #[inline]
    #[must_use]
    pub const fn with(self, other: FuncSpecs) -> FuncSpecs {
        FuncSpecs(self.0 | other.0)
    }

    /// Whether neither was written.
    #[inline]
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// An `alignas` specifier, which takes either a type or a constant expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignSpec {
    /// `alignas(T)`, which means the alignment of `T`.
    Type(TypeNameId),
    /// `alignas(N)`.
    Expr(ExprId),
}

/// What type a declaration named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeSpec {
    /// Nothing was written, which is `int` before C23 with a warning and an error in it.
    None,
    /// One or more of the keywords that name a built-in type.
    Builtin(Builtin),
    /// `struct` or `union`, with or without a tag, with or without a body.
    Record {
        /// Which of the two.
        kind: RecordKind,
        /// The tag, absent for an anonymous one.
        tag: Option<Symbol>,
        /// The members, absent when this is a reference to a tag rather than a definition. An
        /// empty list is a definition of an empty structure, which is a GNU extension, so the
        /// difference between `struct S;` and `struct S {};` has to survive.
        fields: Option<MemberList>,
        /// Attributes on the tag itself, which GCC allows both before and after the body.
        attrs: AttrList,
    },
    /// `enum`, with C23's optional underlying type.
    Enum {
        /// The tag, absent for an anonymous one.
        tag: Option<Symbol>,
        /// The enumerators, absent when this is a reference rather than a definition.
        enumerators: Option<EnumeratorList>,
        /// The `: T` that C23 added, which fixes the representation instead of leaving it to
        /// the implementation.
        underlying: Option<TypeNameId>,
        /// Attributes on the tag itself.
        attrs: AttrList,
    },
    /// An identifier the parser's scope stack said was a typedef name.
    Typedef(Symbol),
    /// `typeof` or `typeof_unqual`, or their `__typeof__` spellings.
    Typeof {
        /// Whether the qualifiers come off, which is what `typeof_unqual` is for.
        unqual: bool,
        /// The operand, which is an expression or a type name and in the expression case is
        /// never evaluated.
        operand: TypeofArg,
    },
    /// `_Atomic(T)`, the type constructor rather than the qualifier.
    Atomic(TypeNameId),
    /// A type deduced from an initializer, which is C23's `auto` and GNU's `__auto_type`.
    Auto(Deduction),
    /// `__builtin_va_list`, whose type is the target's rather than anything the source said.
    ///
    /// gcc declares it as a typedef name that is always in scope. It is a keyword here instead,
    /// which is the same thing seen from the parser's side and one fewer name that a program can
    /// shadow by accident, and it means the parser does not have to be handed a scope with
    /// something already in it before it reads the first token.
    VaList,
}

/// Which of the two spellings asked for a deduced type.
///
/// They deduce the same type and are not the same specifier: gcc names the one that was written
/// in everything it says about a declaration, and C23's is in scope inside its own initializer
/// while GNU's is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deduction {
    /// C23's `auto`, which is the storage class keyword with no other type specifier next to it.
    Auto,
    /// GNU's `__auto_type`, which C23's is modelled on and which every dialect has.
    AutoType,
}

impl Deduction {
    /// How it was written, for the messages that name it.
    #[must_use]
    pub const fn spelling(self) -> &'static str {
        match self {
            Deduction::Auto => "auto",
            Deduction::AutoType => "__auto_type",
        }
    }
}

/// Which of the two record kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    /// `struct`.
    Struct,
    /// `union`.
    Union,
}

impl RecordKind {
    /// The keyword, for the printer and for diagnostics.
    #[must_use]
    pub const fn spelling(self) -> &'static str {
        match self {
            RecordKind::Struct => "struct",
            RecordKind::Union => "union",
        }
    }
}

/// What a `typeof` was applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeofArg {
    /// An expression, which is not evaluated.
    Expr(ExprId),
    /// A type name.
    Type(TypeNameId),
}

/// The keywords naming a built-in type, as the multiset that was written.
///
/// Kept rather than resolved, because the parser reads one keyword at a time and cannot tell
/// what `long` will turn out to mean until the specifier list ends. [`Builtin::add`] catches a
/// keyword written twice, at the place it is written, and [`Builtin::resolve`] catches a
/// combination that names no type once there are no more keywords coming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Builtin {
    /// Which keywords were written.
    pub set: BuiltinSet,
    /// How many times `long` was written, since it is the one that may repeat.
    pub longs: u8,
    /// The width of the `_BitInt`, for the one keyword here that takes one.
    pub width: Option<ExprId>,
}

/// The set of built-in type keywords, without the count of `long`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BuiltinSet(u32);

impl BuiltinSet {
    /// Nothing.
    pub const NONE: BuiltinSet = BuiltinSet(0);
    /// `void`.
    pub const VOID: BuiltinSet = BuiltinSet(1 << 0);
    /// `bool`, in either spelling.
    pub const BOOL: BuiltinSet = BuiltinSet(1 << 1);
    /// `char`.
    pub const CHAR: BuiltinSet = BuiltinSet(1 << 2);
    /// `short`.
    pub const SHORT: BuiltinSet = BuiltinSet(1 << 3);
    /// `int`.
    pub const INT: BuiltinSet = BuiltinSet(1 << 4);
    /// `long`, however many times it was written.
    pub const LONG: BuiltinSet = BuiltinSet(1 << 5);
    /// `signed`.
    pub const SIGNED: BuiltinSet = BuiltinSet(1 << 6);
    /// `unsigned`.
    pub const UNSIGNED: BuiltinSet = BuiltinSet(1 << 7);
    /// `float`.
    pub const FLOAT: BuiltinSet = BuiltinSet(1 << 8);
    /// `double`.
    pub const DOUBLE: BuiltinSet = BuiltinSet(1 << 9);
    /// `_Complex`.
    pub const COMPLEX: BuiltinSet = BuiltinSet(1 << 10);
    /// `_Imaginary`.
    pub const IMAGINARY: BuiltinSet = BuiltinSet(1 << 11);
    /// `__int128`.
    pub const INT128: BuiltinSet = BuiltinSet(1 << 12);
    /// `_Float16`.
    pub const FLOAT16: BuiltinSet = BuiltinSet(1 << 13);
    /// `_Float32`.
    pub const FLOAT32: BuiltinSet = BuiltinSet(1 << 14);
    /// `_Float64`.
    pub const FLOAT64: BuiltinSet = BuiltinSet(1 << 15);
    /// `_Float128`, whose `__float128` spelling is the same type.
    pub const FLOAT128: BuiltinSet = BuiltinSet(1 << 16);
    /// `_Float32x`.
    pub const FLOAT32X: BuiltinSet = BuiltinSet(1 << 17);
    /// `_Float64x`.
    pub const FLOAT64X: BuiltinSet = BuiltinSet(1 << 18);
    /// `_Float128x`.
    pub const FLOAT128X: BuiltinSet = BuiltinSet(1 << 19);
    /// `__float80`.
    pub const FLOAT80: BuiltinSet = BuiltinSet(1 << 20);
    /// `_Decimal32`.
    pub const DECIMAL32: BuiltinSet = BuiltinSet(1 << 21);
    /// `_Decimal64`.
    pub const DECIMAL64: BuiltinSet = BuiltinSet(1 << 22);
    /// `_Decimal128`.
    pub const DECIMAL128: BuiltinSet = BuiltinSet(1 << 23);
    /// `_BitInt`, whose width is kept next to the set rather than in it.
    pub const BIT_INT: BuiltinSet = BuiltinSet(1 << 24);

    /// Everything that names a decimal floating type.
    const DECIMALS: BuiltinSet =
        BuiltinSet(Self::DECIMAL32.0 | Self::DECIMAL64.0 | Self::DECIMAL128.0);
    /// Everything that names one of the `_FloatN` and `_FloatNx` types.
    const EXTENDED: BuiltinSet = BuiltinSet(
        Self::FLOAT16.0
            | Self::FLOAT32.0
            | Self::FLOAT64.0
            | Self::FLOAT128.0
            | Self::FLOAT32X.0
            | Self::FLOAT64X.0
            | Self::FLOAT128X.0
            | Self::FLOAT80.0,
    );
    /// Everything that can be part of a standard integer type.
    const INTEGER: BuiltinSet =
        BuiltinSet(Self::SHORT.0 | Self::INT.0 | Self::LONG.0 | Self::SIGNED.0 | Self::UNSIGNED.0);

    /// Whether every keyword in `other` is set here.
    #[inline]
    #[must_use]
    pub const fn has(self, other: BuiltinSet) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any keyword in `other` is set here.
    #[inline]
    #[must_use]
    pub const fn has_any(self, other: BuiltinSet) -> bool {
        self.0 & other.0 != 0
    }

    /// This set with `other` added.
    #[inline]
    #[must_use]
    pub const fn with(self, other: BuiltinSet) -> BuiltinSet {
        BuiltinSet(self.0 | other.0)
    }

    /// This set with everything in `other` taken out.
    #[inline]
    #[must_use]
    pub const fn without(self, other: BuiltinSet) -> BuiltinSet {
        BuiltinSet(self.0 & !other.0)
    }

    /// Whether nothing was written.
    #[inline]
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// Why a built-in type keyword could not be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinError {
    /// The keyword was already written, and it is not `long`.
    Duplicate,
    /// A third `long`, which no type has.
    TooManyLongs,
}

impl Builtin {
    /// Nothing written yet.
    pub const NONE: Builtin = Builtin { set: BuiltinSet::NONE, longs: 0, width: None };

    /// This with one more keyword.
    ///
    /// `long` is the only keyword that may be written twice, so everything else is a duplicate
    /// the second time and is reported at the keyword rather than at the end of the specifier
    /// list, which is where the second half of the checking happens.
    ///
    /// # Errors
    ///
    /// [`BuiltinError::Duplicate`] for a repeat, and [`BuiltinError::TooManyLongs`] for a third
    /// `long`.
    pub const fn add(self, which: BuiltinSet) -> Result<Builtin, BuiltinError> {
        if which.0 == BuiltinSet::LONG.0 {
            if self.longs >= 2 {
                return Err(BuiltinError::TooManyLongs);
            }
            let set = self.set.with(which);
            return Ok(Builtin { set, longs: self.longs + 1, width: self.width });
        }
        if self.set.has(which) {
            return Err(BuiltinError::Duplicate);
        }
        Ok(Builtin { set: self.set.with(which), longs: self.longs, width: self.width })
    }

    /// This with `_BitInt(width)` written into it.
    ///
    /// The width is the one thing a type keyword carries, and it is kept here rather than in a
    /// specifier of its own so that `unsigned _BitInt(8)` and `_BitInt(8) unsigned` are the
    /// same declaration, which is what they are: a sign and a width may be written either way
    /// round, and neither of them names a type on its own.
    ///
    /// # Errors
    ///
    /// [`BuiltinError::Duplicate`] when `_BitInt` was already written.
    pub const fn add_bit_int(self, width: ExprId) -> Result<Builtin, BuiltinError> {
        if self.set.has(BuiltinSet::BIT_INT) {
            return Err(BuiltinError::Duplicate);
        }
        let set = self.set.with(BuiltinSet::BIT_INT);
        Ok(Builtin { set, longs: self.longs, width: Some(width) })
    }

    /// Whether any built-in keyword has been written.
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.set.is_none()
    }

    /// The type this combination of keywords names, and `None` if it names no type.
    ///
    /// The table is the one in 6.7.2 plus the GNU and C23 rows: `_Complex` and `_Imaginary` on
    /// the floating types, `_Complex` on the integer ones which GCC also allows, `__int128`
    /// with a sign, and the `_FloatN` and `_DecimalN` families which stand alone.
    #[must_use]
    pub fn resolve(self) -> Option<Basic> {
        let set = self.set;
        let longs = self.longs;

        let complexity = match (set.has(BuiltinSet::COMPLEX), set.has(BuiltinSet::IMAGINARY)) {
            (true, true) => return None,
            (true, false) => Complexity::Complex,
            (false, true) => Complexity::Imaginary,
            (false, false) => Complexity::Real,
        };
        let set = set.without(BuiltinSet::COMPLEX.with(BuiltinSet::IMAGINARY));
        let basic = |scalar| Some(Basic { scalar, complexity });
        let real = |scalar| {
            if complexity == Complexity::Real { Some(Basic { scalar, complexity }) } else { None }
        };

        if set.has(BuiltinSet::SIGNED) && set.has(BuiltinSet::UNSIGNED) {
            return None;
        }
        let unsigned = set.has(BuiltinSet::UNSIGNED);
        let signs = BuiltinSet::SIGNED.with(BuiltinSet::UNSIGNED);

        // Everything below is "these keywords and nothing else", which is what makes `void int`
        // and `char short` fail here rather than needing a rule each.
        if set.has(BuiltinSet::VOID) {
            return if set == BuiltinSet::VOID && longs == 0 { real(Scalar::Void) } else { None };
        }
        if set.has(BuiltinSet::BOOL) {
            return if set == BuiltinSet::BOOL && longs == 0 { real(Scalar::Bool) } else { None };
        }
        if set.has(BuiltinSet::CHAR) {
            if set.without(signs) != BuiltinSet::CHAR || longs != 0 {
                return None;
            }
            return real(match (set.has(BuiltinSet::SIGNED), unsigned) {
                (true, _) => Scalar::SignedChar,
                (_, true) => Scalar::UnsignedChar,
                _ => Scalar::Char,
            });
        }
        if set.has(BuiltinSet::INT128) {
            if set.without(signs) != BuiltinSet::INT128 || longs != 0 {
                return None;
            }
            return real(if unsigned { Scalar::UnsignedInt128 } else { Scalar::Int128 });
        }
        if set.has(BuiltinSet::BIT_INT) {
            if set.without(signs) != BuiltinSet::BIT_INT || longs != 0 {
                return None;
            }
            // A width of nothing is a `_BitInt` whose parenthesised part did not parse, which
            // the parser has already reported and which names no type here either.
            let width = self.width?;
            return real(Scalar::BitInt { width, unsigned });
        }
        if set.has_any(BuiltinSet::DECIMALS) {
            if longs != 0 || complexity != Complexity::Real {
                return None;
            }
            return match set {
                s if s == BuiltinSet::DECIMAL32 => real(Scalar::Decimal32),
                s if s == BuiltinSet::DECIMAL64 => real(Scalar::Decimal64),
                s if s == BuiltinSet::DECIMAL128 => real(Scalar::Decimal128),
                _ => None,
            };
        }
        if set.has_any(BuiltinSet::EXTENDED) {
            if longs != 0 {
                return None;
            }
            return match set {
                s if s == BuiltinSet::FLOAT16 => basic(Scalar::Float16),
                s if s == BuiltinSet::FLOAT32 => basic(Scalar::Float32),
                s if s == BuiltinSet::FLOAT64 => basic(Scalar::Float64),
                s if s == BuiltinSet::FLOAT128 => basic(Scalar::Float128),
                s if s == BuiltinSet::FLOAT32X => basic(Scalar::Float32x),
                s if s == BuiltinSet::FLOAT64X => basic(Scalar::Float64x),
                s if s == BuiltinSet::FLOAT128X => basic(Scalar::Float128x),
                s if s == BuiltinSet::FLOAT80 => basic(Scalar::Float80),
                _ => None,
            };
        }
        if set.has(BuiltinSet::FLOAT) {
            return if set == BuiltinSet::FLOAT && longs == 0 {
                basic(Scalar::Float)
            } else {
                None
            };
        }
        if set.has(BuiltinSet::DOUBLE) {
            if set.without(BuiltinSet::LONG) != BuiltinSet::DOUBLE {
                return None;
            }
            return match longs {
                0 => basic(Scalar::Double),
                1 => basic(Scalar::LongDouble),
                _ => None,
            };
        }
        if set.is_none() {
            return None;
        }
        // What is left is the standard integer types, where `int` is implied by any of the
        // others and every combination is legal except `short long`.
        if !BuiltinSet::INTEGER.has(set) {
            return None;
        }
        if set.has(BuiltinSet::SHORT) {
            if longs != 0 {
                return None;
            }
            return basic(if unsigned { Scalar::UnsignedShort } else { Scalar::Short });
        }
        match longs {
            0 => basic(if unsigned { Scalar::UnsignedInt } else { Scalar::Int }),
            1 => basic(if unsigned { Scalar::UnsignedLong } else { Scalar::Long }),
            2 => basic(if unsigned { Scalar::UnsignedLongLong } else { Scalar::LongLong }),
            _ => None,
        }
    }
}

/// A built-in type, once the keywords have been read together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Basic {
    /// The type itself.
    pub scalar: Scalar,
    /// Whether `_Complex` or `_Imaginary` was written.
    pub complexity: Complexity,
}

/// Whether a built-in type is real, complex or imaginary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Complexity {
    /// Neither keyword.
    Real,
    /// `_Complex`.
    Complex,
    /// `_Imaginary`, which GCC parses and has never implemented.
    Imaginary,
}

/// A built-in type named by keywords, with the sign folded in.
///
/// `char` is here three times because plain `char` is a third type distinct from both
/// `signed char` and `unsigned char`, however it is represented on the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    /// `void`.
    Void,
    /// `bool`.
    Bool,
    /// `char`.
    Char,
    /// `signed char`.
    SignedChar,
    /// `unsigned char`.
    UnsignedChar,
    /// `short`.
    Short,
    /// `unsigned short`.
    UnsignedShort,
    /// `int`.
    Int,
    /// `unsigned int`.
    UnsignedInt,
    /// `long`.
    Long,
    /// `unsigned long`.
    UnsignedLong,
    /// `long long`.
    LongLong,
    /// `unsigned long long`.
    UnsignedLongLong,
    /// `__int128`.
    Int128,
    /// `unsigned __int128`.
    UnsignedInt128,
    /// `_BitInt(N)`, with the sign written next to it folded in like every other sign here.
    BitInt {
        /// The width, which is a constant expression that nothing has evaluated yet.
        width: ExprId,
        /// Whether `unsigned` was written, which changes the least width there is as well as
        /// the range: a signed one needs a bit for the sign and so is never narrower than two.
        unsigned: bool,
    },
    /// `float`.
    Float,
    /// `double`.
    Double,
    /// `long double`.
    LongDouble,
    /// `_Float16`.
    Float16,
    /// `_Float32`.
    Float32,
    /// `_Float64`.
    Float64,
    /// `_Float128`.
    Float128,
    /// `_Float32x`.
    Float32x,
    /// `_Float64x`.
    Float64x,
    /// `_Float128x`.
    Float128x,
    /// `__float80`.
    Float80,
    /// `_Decimal32`.
    Decimal32,
    /// `_Decimal64`.
    Decimal64,
    /// `_Decimal128`.
    Decimal128,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(keywords: &[BuiltinSet]) -> Option<Basic> {
        let mut b = Builtin::NONE;
        for &k in keywords {
            b = b.add(k).expect("keyword rejected");
        }
        b.resolve()
    }

    fn real(keywords: &[BuiltinSet]) -> Option<Scalar> {
        resolve(keywords).filter(|b| b.complexity == Complexity::Real).map(|b| b.scalar)
    }

    #[test]
    fn the_plain_integer_spellings() {
        assert_eq!(real(&[BuiltinSet::INT]), Some(Scalar::Int));
        assert_eq!(real(&[BuiltinSet::SIGNED]), Some(Scalar::Int));
        assert_eq!(real(&[BuiltinSet::UNSIGNED]), Some(Scalar::UnsignedInt));
        assert_eq!(real(&[BuiltinSet::SIGNED, BuiltinSet::INT]), Some(Scalar::Int));
        assert_eq!(real(&[BuiltinSet::SHORT]), Some(Scalar::Short));
        assert_eq!(real(&[BuiltinSet::SHORT, BuiltinSet::INT]), Some(Scalar::Short));
        assert_eq!(
            real(&[BuiltinSet::UNSIGNED, BuiltinSet::SHORT, BuiltinSet::INT]),
            Some(Scalar::UnsignedShort)
        );
    }

    #[test]
    fn long_counts_rather_than_repeats() {
        assert_eq!(real(&[BuiltinSet::LONG]), Some(Scalar::Long));
        assert_eq!(real(&[BuiltinSet::LONG, BuiltinSet::LONG]), Some(Scalar::LongLong));
        assert_eq!(
            real(&[BuiltinSet::UNSIGNED, BuiltinSet::LONG, BuiltinSet::LONG, BuiltinSet::INT]),
            Some(Scalar::UnsignedLongLong)
        );
        let three = Builtin::NONE
            .add(BuiltinSet::LONG)
            .and_then(|b| b.add(BuiltinSet::LONG))
            .and_then(|b| b.add(BuiltinSet::LONG));
        assert_eq!(three, Err(BuiltinError::TooManyLongs));
    }

    #[test]
    fn a_repeated_keyword_is_caught_where_it_is_written() {
        let twice = Builtin::NONE.add(BuiltinSet::INT).and_then(|b| b.add(BuiltinSet::INT));
        assert_eq!(twice, Err(BuiltinError::Duplicate));
    }

    #[test]
    fn plain_char_is_its_own_type() {
        assert_eq!(real(&[BuiltinSet::CHAR]), Some(Scalar::Char));
        assert_eq!(real(&[BuiltinSet::SIGNED, BuiltinSet::CHAR]), Some(Scalar::SignedChar));
        assert_eq!(real(&[BuiltinSet::UNSIGNED, BuiltinSet::CHAR]), Some(Scalar::UnsignedChar));
        assert_eq!(real(&[BuiltinSet::CHAR, BuiltinSet::INT]), None);
        assert_eq!(real(&[BuiltinSet::CHAR, BuiltinSet::LONG]), None);
    }

    #[test]
    fn long_double_is_a_double_with_one_long() {
        assert_eq!(real(&[BuiltinSet::DOUBLE]), Some(Scalar::Double));
        assert_eq!(real(&[BuiltinSet::LONG, BuiltinSet::DOUBLE]), Some(Scalar::LongDouble));
        assert_eq!(real(&[BuiltinSet::LONG, BuiltinSet::LONG, BuiltinSet::DOUBLE]), None);
        assert_eq!(real(&[BuiltinSet::LONG, BuiltinSet::FLOAT]), None);
        assert_eq!(real(&[BuiltinSet::DOUBLE, BuiltinSet::INT]), None);
    }

    #[test]
    fn complex_is_a_modifier_and_not_a_type() {
        assert_eq!(
            resolve(&[BuiltinSet::COMPLEX, BuiltinSet::DOUBLE]),
            Some(Basic { scalar: Scalar::Double, complexity: Complexity::Complex })
        );
        assert_eq!(
            resolve(&[BuiltinSet::LONG, BuiltinSet::DOUBLE, BuiltinSet::IMAGINARY]),
            Some(Basic { scalar: Scalar::LongDouble, complexity: Complexity::Imaginary })
        );
        // GCC accepts a complex integer type, so this is not an error here either.
        assert_eq!(
            resolve(&[BuiltinSet::COMPLEX, BuiltinSet::INT]),
            Some(Basic { scalar: Scalar::Int, complexity: Complexity::Complex })
        );
        assert_eq!(resolve(&[BuiltinSet::COMPLEX, BuiltinSet::IMAGINARY, BuiltinSet::FLOAT]), None);
        // There is no complex decimal type in any dialect.
        assert_eq!(resolve(&[BuiltinSet::COMPLEX, BuiltinSet::DECIMAL64]), None);
    }

    #[test]
    fn the_extended_types_stand_alone_or_with_complex() {
        assert_eq!(real(&[BuiltinSet::FLOAT128]), Some(Scalar::Float128));
        assert_eq!(
            resolve(&[BuiltinSet::COMPLEX, BuiltinSet::FLOAT128]),
            Some(Basic { scalar: Scalar::Float128, complexity: Complexity::Complex })
        );
        assert_eq!(real(&[BuiltinSet::FLOAT32X]), Some(Scalar::Float32x));
        assert_eq!(real(&[BuiltinSet::FLOAT128X]), Some(Scalar::Float128x));
        assert_eq!(real(&[BuiltinSet::FLOAT16, BuiltinSet::INT]), None);
        assert_eq!(real(&[BuiltinSet::FLOAT32, BuiltinSet::FLOAT64]), None);
    }

    #[test]
    fn the_wide_integers_take_a_sign_and_nothing_else() {
        assert_eq!(real(&[BuiltinSet::INT128]), Some(Scalar::Int128));
        assert_eq!(real(&[BuiltinSet::UNSIGNED, BuiltinSet::INT128]), Some(Scalar::UnsignedInt128));
        assert_eq!(real(&[BuiltinSet::INT128, BuiltinSet::INT]), None);
    }

    #[test]
    fn void_and_bool_take_nothing() {
        assert_eq!(real(&[BuiltinSet::VOID]), Some(Scalar::Void));
        assert_eq!(real(&[BuiltinSet::BOOL]), Some(Scalar::Bool));
        assert_eq!(real(&[BuiltinSet::VOID, BuiltinSet::INT]), None);
        assert_eq!(real(&[BuiltinSet::UNSIGNED, BuiltinSet::BOOL]), None);
    }

    #[test]
    fn two_signs_name_no_type() {
        assert_eq!(real(&[BuiltinSet::SIGNED, BuiltinSet::UNSIGNED]), None);
    }

    #[test]
    fn no_keywords_at_all_names_no_type() {
        assert_eq!(Builtin::NONE.resolve(), None);
        assert!(Builtin::NONE.is_none());
    }

    #[test]
    fn short_and_long_do_not_go_together() {
        assert_eq!(real(&[BuiltinSet::SHORT, BuiltinSet::LONG]), None);
    }

    #[test]
    fn qualifier_sets_add_up() {
        let q = Quals::NONE.with(Quals::CONST).with(Quals::VOLATILE);
        assert!(q.has(Quals::CONST));
        assert!(q.has(Quals::VOLATILE));
        assert!(!q.has(Quals::RESTRICT));
        assert!(Quals::NONE.is_none());
        assert!(!q.is_none());
    }

    #[test]
    fn function_specifier_sets_add_up() {
        let f = FuncSpecs::NONE.with(FuncSpecs::INLINE);
        assert!(f.has(FuncSpecs::INLINE));
        assert!(!f.has(FuncSpecs::NORETURN));
        assert!(FuncSpecs::NONE.is_none());
    }
}
