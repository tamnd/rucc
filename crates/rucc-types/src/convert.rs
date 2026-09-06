//! The integer promotions and the usual arithmetic conversions.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.2.
//!
//! These are 6.3.1.1 and 6.3.1.8, the two rules that decide what type an arithmetic expression
//! has, and they are where a compiler quietly goes wrong in ways that only show up as an
//! overflow or a sign extension in generated code. So the answers here were read out of gcc
//! 13.3 and clang 18 rather than out of the standard, with `_Generic` naming the type of every
//! interesting pair, and the standard was used to explain what was measured.
//!
//! C23 changed three things and they are all here. `bool` is a real type and promotes to `int`.
//! An enumeration may have a fixed underlying type, and then it promotes through that rather
//! than through a type the implementation picked. And `_BitInt` does not promote at all, so
//! `_BitInt(8) + _BitInt(8)` is `_BitInt(8)` where `char + char` is `int`, which is the point
//! of the type: it is the one integer type in C that does what it says.

use rucc_target::TargetInfo;

use crate::classify::{element, is_vector, lanes};
use crate::kind::{FloatKind, IntKind, TypeKind};
use crate::layout::{float_format, float_width, int_width, integer_info, layout};
use crate::types::{TypeId, Types};

/// The integer promotions, 6.3.1.1.
///
/// Anything narrower than `int` becomes `int`, or `unsigned int` when `int` cannot hold every
/// value it had. Everything else, floating types and pointers included, is its own answer, so
/// this can be called on any operand without asking what it is first.
///
/// The qualifiers and `_Atomic` come off, because by the time a value is being promoted the
/// lvalue conversion of 6.3.2.1 has already happened and neither of them is part of the value.
///
/// `_BitInt` is deliberately not promoted. That is C23 6.3.1.1p2, and it is what both compilers
/// do: `+x` on a `_BitInt(8)` is still a `_BitInt(8)`.
pub fn promote(types: &mut Types, id: TypeId, target: &TargetInfo) -> TypeId {
    let id = value_type(types, id);
    match types.kind(id) {
        TypeKind::Bool => types.int(IntKind::Int),
        TypeKind::Int(kind) => promoted_int(types, kind, int_width(kind, target), target),
        TypeKind::Enum(id) => {
            // An enumeration promotes through whatever it is represented in. Until that has
            // been decided the declaration is incomplete, which is a diagnostic somewhere with
            // a span; `int` keeps the rest of the expression checkable in the meantime.
            let underlying = types.enum_info(id).underlying;
            match underlying {
                Some(underlying) => promote(types, underlying, target),
                None => types.int(IntKind::Int),
            }
        }
        _ => id,
    }
}

/// The integer promotions applied to a bit-field of the given width.
///
/// A bit-field is narrower than the type it was declared with, and it is the width rather than
/// the type that decides. `unsigned b:3` promotes to `int`, because every three bit value fits
/// in one, and `unsigned b:32` promotes to `unsigned int`, because they no longer do.
///
/// A bit-field wider than an `int` is an integer of exactly that many bits, which is the type
/// `_BitInt` already is. The C17 wording says `unsigned int` there, which would turn a forty bit
/// field into a thirty two bit value, and C23 says the declared type instead. Neither is what
/// either compiler does: gcc gives such a field a type whose precision is the width, so a forty
/// bit field shifted left by thirty two is zero rather than a value with bits above the fortieth,
/// and clang agrees. That is a `_BitInt(40)` here, and saying it that way is what makes the usual
/// arithmetic conversions below give the right answer for a pair of them without knowing they
/// came from bit-fields.
///
/// A field as wide as the type it was declared with is that type, since there is no precision to
/// lose and `unsigned long long b:64` reading as a `_BitInt(64)` would be a different type for no
/// reason.
pub fn promote_bit_field(types: &mut Types, id: TypeId, width: u32, target: &TargetInfo) -> TypeId {
    let id = value_type(types, id);
    let (signed, declared) = match types.kind(id) {
        TypeKind::Bool => (false, 1),
        TypeKind::Int(kind) => (kind.is_signed(target.char_is_signed), int_width(kind, target)),
        TypeKind::BitInt { signed, width } => (signed, width),
        // An enumeration bit-field promotes through what it is represented in, and anything
        // else is not something a bit-field may be declared with.
        _ => return promote(types, id, target),
    };
    let int = int_width(IntKind::Int, target);
    if width < int || (signed && width == int) {
        return types.int(IntKind::Int);
    }
    if !signed && width == int {
        return types.int(IntKind::UInt);
    }
    if width >= declared {
        return promote(types, id, target);
    }
    types.bit_int(signed, width)
}

/// The usual arithmetic conversions, 6.3.1.8: the one type both operands are converted to.
///
/// [`None`] when either operand is not an arithmetic type, which is not a failure of this
/// function but the shape of a diagnostic its caller is about to write.
///
/// The floating rules come first and the integer rules only run when neither side is floating,
/// which is why `unsigned long long + float` is `float` and loses precision rather than being
/// the other way round.
pub fn usual_arithmetic(
    types: &mut Types,
    left: TypeId,
    right: TypeId,
    target: &TargetInfo,
) -> Option<TypeId> {
    let left = value_type(types, left);
    let right = value_type(types, right);
    if let Some(common) = floating(types, left, right, target) {
        return Some(common);
    }
    let left = promote(types, left, target);
    let right = promote(types, right, target);
    if left == right {
        // Not only a shortcut: it is also the answer for the types this function does not model
        // as integers, which is every one of them once the two sides agree.
        return integer_shape(types, left, target).map(|_| left);
    }
    let left = integer_shape(types, left, target)?;
    let right = integer_shape(types, right, target)?;
    Some(common_integer(types, left, right, target))
}

/// Whether one vector may be assigned to another that is not compatible with it.
///
/// GNU C is deliberately loose here, and it has to be, because the vector extension gives no way
/// to spell a conversion: `(V)x` on two vectors reinterprets the bytes and there is no cast that
/// means anything else, so a rule as strict as the one for records would leave a program with no
/// way to write what it meant. gcc's rule, in `vector_types_convertible_p`, is that two vectors
/// of the same total size convert when their lanes are both integers, or are both floating and
/// the same width. The lane count may differ, so a sixteen lane vector of `short` may be assigned
/// to an eight lane vector of `int`.
///
/// The place a program hits this without asking for it is a comparison. The mask a comparison
/// answers with has signed lanes whatever the operands had, so `V x = (v > 0);` where `V` has
/// unsigned lanes is this rule and nothing more exotic, and three of the torture cases are
/// exactly that line.
///
/// gcc notes the first one it converts this way and suggests `-flax-vector-conversions`. Nothing
/// is said here, because the note is about a flag that turns off a stricter check we do not make.
#[must_use]
pub fn vectors_convertible(types: &Types, a: TypeId, b: TypeId, target: &TargetInfo) -> bool {
    if !is_vector(types, a) || !is_vector(types, b) {
        return false;
    }
    let (Some(x), Some(y)) = (element(types, a), element(types, b)) else {
        return false;
    };
    let sizes = |left, right| match (layout(types, left, target), layout(types, right, target)) {
        (Ok(left), Ok(right)) => Some((left.size, right.size)),
        _ => None,
    };
    let Some((whole, other)) = sizes(a, b) else {
        return false;
    };
    if whole != other {
        return false;
    }
    match (integer_info(types, x, target), integer_info(types, y, target)) {
        (Some(_), Some(_)) => true,
        (None, None) => sizes(x, y).is_some_and(|(left, right)| left == right),
        _ => false,
    }
}

/// The type a comparison of two vectors answers with, and [`None`] where `id` is not a vector.
///
/// GNU C says `a < b` over vectors is a vector of signed integers with the lane count the
/// operands had and a lane as wide as theirs, holding all ones where the comparison held and
/// zero where it did not. The width has to match rather than merely be an `int`, because the
/// mask is written to be used: `(a < b) & c` reaches every bit of `c` only when the two line up
/// lane for lane, and that is what the result of a vector comparison is for.
///
/// A vector of signed integers is its own mask type, which is what gcc answers and is worth
/// keeping, since `v4si` comparing to `v4si` reading back as some other spelling of the same
/// thing is a diagnostic nobody can act on. Anything else, an unsigned lane or a floating one,
/// gets the signed integer type of that width.
///
/// [`None`] also where the target has no standard integer as wide as the lane, which is the
/// eighty bit `long double` and nothing else.
pub fn mask_of(types: &mut Types, id: TypeId, target: &TargetInfo) -> Option<TypeId> {
    let elem = element(types, id)?;
    let len = lanes(types, id)?;
    let bits = match integer_info(types, elem, target) {
        Some(info) if info.signed => {
            let lane = types.unqualified(elem);
            return Some(types.vector(lane, len));
        }
        Some(info) => info.width,
        None => match types.kind(types.canonical(elem)) {
            TypeKind::Float(kind) => float_width(kind, target),
            _ => return None,
        },
    };
    let standard = [
        IntKind::SChar,
        IntKind::Short,
        IntKind::Int,
        IntKind::Long,
        IntKind::LongLong,
        IntKind::Int128,
    ];
    let kind = standard.into_iter().find(|&kind| int_width(kind, target) == bits)?;
    let lane = types.int(kind);
    Some(types.vector(lane, len))
}

/// The type of a value of type `id`, which is `id` without the parts an lvalue conversion
/// removes.
fn value_type(types: &mut Types, id: TypeId) -> TypeId {
    let id = types.canonical(id);
    let id = match types.kind(id) {
        TypeKind::Atomic(inner) => types.canonical(inner),
        _ => id,
    };
    types.unqualified(id)
}

/// The promotion of a standard integer type of the given width.
fn promoted_int(types: &mut Types, kind: IntKind, width: u32, target: &TargetInfo) -> TypeId {
    if kind.rank() >= IntKind::Int.rank() {
        return types.int(kind);
    }
    let int = int_width(IntKind::Int, target);
    let signed = kind.is_signed(target.char_is_signed);
    if width < int || (signed && width == int) {
        return types.int(IntKind::Int);
    }
    types.int(IntKind::UInt)
}

/// The common type when either side is a floating type, and [`None`] when neither is.
///
/// The real type is the one with the higher rank, or the floating one when the other side is an
/// integer, and the result is complex when either operand was. That last part is why
/// `_Complex float + double` is `_Complex double`: the real types combine first and the
/// complexity is carried across afterwards.
fn floating(types: &mut Types, left: TypeId, right: TypeId, target: &TargetInfo) -> Option<TypeId> {
    let left = float_part(types, left);
    let right = float_part(types, right);
    let (kind, complex) = match (left, right) {
        (None, None) => return None,
        (Some((kind, complex)), None) | (None, Some((kind, complex))) => (kind, complex),
        (Some((a, a_complex)), Some((b, b_complex))) => {
            let kind = if float_rank(a, target) >= float_rank(b, target) { a } else { b };
            (kind, a_complex || b_complex)
        }
    };
    Some(if complex { types.complex(kind) } else { types.float(kind) })
}

/// The conversion rank of a real floating type, as something two of which can be compared.
///
/// There is no ordering on the kinds themselves to use here, because since C23 the answer
/// depends on the target: `long double` outranks `_Float64x` on x86-64, where both of them are
/// the x87 format, and loses to it on Apple, where `long double` is a `double`. So the question
/// is asked of the format instead. Precision first and then range, which never disagree among
/// the binary formats, and [`FloatKind::tie_break`] settles two types that are the same format,
/// which is what makes `double + _Float64` a `_Float64`.
fn float_rank(kind: FloatKind, target: &TargetInfo) -> (u32, i32, u8) {
    let format = float_format(kind, target);
    (format.precision(), format.max_exponent(), kind.tie_break())
}

/// The real floating type inside `id`, and whether it was complex.
fn float_part(types: &Types, id: TypeId) -> Option<(FloatKind, bool)> {
    match types.kind(id) {
        TypeKind::Float(kind) => Some((kind, false)),
        TypeKind::Complex(kind) => Some((kind, true)),
        _ => None,
    }
}

/// What an integer type is, once it no longer matters how it was spelled.
#[derive(Clone, Copy)]
struct IntShape {
    signed: bool,
    width: u32,
    /// The standard type it is, and [`None`] for a `_BitInt`.
    standard: Option<IntKind>,
}

impl IntShape {
    /// The integer conversion rank, as something two of which can be compared.
    ///
    /// Width first, which is what makes a `_BitInt(40)` outrank an `int` and lose to a `long`.
    /// A standard type outranks a `_BitInt` of the same width, which is C23 6.3.1.1p1 and is
    /// why `_BitInt(32) + int` is `int`. The standard rank breaks the last tie, which is the
    /// one that matters on every 64-bit target: `long` and `long long` are both sixty four bits
    /// and `long long` outranks `long`.
    fn rank(self) -> (u32, u8, u8) {
        match self.standard {
            Some(kind) => (self.width, 1, kind.rank()),
            None => (self.width, 0, 0),
        }
    }

    /// Whether every value of `other` is a value of this type.
    fn covers(self, other: IntShape) -> bool {
        if self.signed == other.signed {
            return self.width >= other.width;
        }
        // A signed type loses a bit to the sign, so it takes a strictly wider one to hold every
        // value of an unsigned type. That is what makes `unsigned int + long` a `long` on
        // Linux and an `unsigned long` on Windows, where `long` is only thirty two bits.
        self.signed && self.width > other.width
    }
}

/// The shape of an integer type, and [`None`] when `id` is not one.
fn integer_shape(types: &Types, id: TypeId, target: &TargetInfo) -> Option<IntShape> {
    match types.kind(id) {
        TypeKind::Int(kind) => Some(IntShape {
            signed: kind.is_signed(target.char_is_signed),
            width: int_width(kind, target),
            standard: Some(kind),
        }),
        TypeKind::BitInt { signed, width } => Some(IntShape { signed, width, standard: None }),
        _ => None,
    }
}

/// The common type of two promoted integer types.
fn common_integer(
    types: &mut Types,
    left: IntShape,
    right: IntShape,
    target: &TargetInfo,
) -> TypeId {
    let (higher, lower) = if left.rank() >= right.rank() { (left, right) } else { (right, left) };
    if higher.signed == lower.signed || !higher.signed || higher.covers(lower) {
        // Same signedness, or the higher ranked one is the unsigned one, or it is the signed
        // one and wide enough to hold every value the other side had.
        return build(types, higher, target);
    }
    // The signed type wins on rank and loses on range, so neither operand's type will do and
    // the answer is the unsigned type of the same width. This is the arm that turns
    // `long + unsigned long` into `unsigned long`, and it is the one a program is surprised by.
    build(types, IntShape { signed: false, ..higher }, target)
}

/// The type an [`IntShape`] describes.
fn build(types: &mut Types, shape: IntShape, target: &TargetInfo) -> TypeId {
    match shape.standard {
        Some(kind) if kind.is_signed(target.char_is_signed) == shape.signed => types.int(kind),
        Some(kind) => types.int(kind.flip_sign()),
        None => types.bit_int(shape.signed, shape.width),
    }
}
