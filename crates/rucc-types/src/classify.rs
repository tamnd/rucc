//! What category a type is in, which is what almost every constraint in C is written over.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.1.
//!
//! The standard states its rules in terms of categories rather than types: an operand of `%`
//! must have integer type, an operand of `!` must have scalar type, a member of a `struct` must
//! have complete object type. Those categories are asked about constantly and they are exactly
//! where a compiler drifts, because each of them has one or two members nobody remembers.
//!
//! The three that get forgotten:
//!
//! An enumeration is an integer type. `enum e x; x % 2` is legal C and a compiler that asks
//! whether the kind is `Int` says it is not.
//!
//! `_Atomic(T)` is in whatever category `T` is in. It is a type here rather than a qualifier,
//! which is the right way round for spelling it and the wrong way round for this question, so
//! everything below looks through it. `_Atomic(int)` is an integer type.
//!
//! `void` is an object type and is never a complete one. Those are two different questions and
//! collapsing them is how `sizeof (void)` ends up either accepted or rejected for the wrong
//! reason, since it is a constraint violation that gcc accepts as an extension worth one byte.
//!
//! Every question here reads [`Types::canonical`], so a typedef name answers as what it names.

use crate::kind::{ArrayLen, Qualifiers, TypeKind};
use crate::types::{TypeId, Types};

/// What a type is, once the sugar and `_Atomic` are off it.
fn bare(types: &Types, id: TypeId) -> TypeKind {
    match types.kind(types.canonical(id)) {
        TypeKind::Atomic(inner) => types.kind(types.canonical(inner)),
        other => other,
    }
}

/// `void`.
#[must_use]
pub fn is_void(types: &Types, id: TypeId) -> bool {
    matches!(bare(types, id), TypeKind::Void)
}

/// An integer type, 6.2.5p17.
///
/// `bool`, the standard and extended integer types, `_BitInt`, and every enumeration. The last
/// is the one that gets forgotten, and forgetting it rejects `enum e x; x % 2`.
#[must_use]
pub fn is_integer(types: &Types, id: TypeId) -> bool {
    matches!(
        bare(types, id),
        TypeKind::Bool | TypeKind::Int(_) | TypeKind::BitInt { .. } | TypeKind::Enum(_)
    )
}

/// A real floating type: `float`, `double`, `long double` and the extended ones.
#[must_use]
pub fn is_real_floating(types: &Types, id: TypeId) -> bool {
    matches!(bare(types, id), TypeKind::Float(_))
}

/// A complex type, `_Complex T`.
#[must_use]
pub fn is_complex(types: &Types, id: TypeId) -> bool {
    matches!(bare(types, id), TypeKind::Complex(_))
}

/// A floating type, which is the real ones and the complex ones together.
#[must_use]
pub fn is_floating(types: &Types, id: TypeId) -> bool {
    matches!(bare(types, id), TypeKind::Float(_) | TypeKind::Complex(_))
}

/// An arithmetic type, 6.2.5p18: the integer types and the floating types.
#[must_use]
pub fn is_arithmetic(types: &Types, id: TypeId) -> bool {
    is_integer(types, id) || is_floating(types, id)
}

/// A real type, 6.2.5p17: the integer types and the real floating types.
///
/// Not the same question as [`is_arithmetic`]. `<` takes real operands, so comparing two
/// `_Complex double` values is a constraint violation while adding them is not.
#[must_use]
pub fn is_real(types: &Types, id: TypeId) -> bool {
    is_integer(types, id) || is_real_floating(types, id)
}

/// A pointer type.
#[must_use]
pub fn is_pointer(types: &Types, id: TypeId) -> bool {
    matches!(bare(types, id), TypeKind::Pointer(_))
}

/// What a pointer points to, or [`None`] where it is not a pointer.
#[must_use]
pub fn pointee(types: &Types, id: TypeId) -> Option<TypeId> {
    match bare(types, id) {
        TypeKind::Pointer(inner) => Some(inner),
        _ => None,
    }
}

/// An array type.
#[must_use]
pub fn is_array(types: &Types, id: TypeId) -> bool {
    matches!(bare(types, id), TypeKind::Array { .. })
}

/// The element type of an array or a vector, or [`None`] where it is neither.
#[must_use]
pub fn element(types: &Types, id: TypeId) -> Option<TypeId> {
    match bare(types, id) {
        TypeKind::Array { elem, .. } | TypeKind::Vector { elem, .. } => Some(elem),
        _ => None,
    }
}

/// A function type.
#[must_use]
pub fn is_function(types: &Types, id: TypeId) -> bool {
    matches!(bare(types, id), TypeKind::Function(_))
}

/// A `struct` or a `union`.
#[must_use]
pub fn is_record(types: &Types, id: TypeId) -> bool {
    matches!(bare(types, id), TypeKind::Record(_))
}

/// A GNU vector type.
#[must_use]
pub fn is_vector(types: &Types, id: TypeId) -> bool {
    matches!(bare(types, id), TypeKind::Vector { .. })
}

/// `_Atomic(T)`, whatever `T` is.
///
/// The one question that does not look through the wrapper, since it is asking about it.
#[must_use]
pub fn is_atomic(types: &Types, id: TypeId) -> bool {
    matches!(types.kind(types.canonical(id)), TypeKind::Atomic(_))
}

/// A scalar type, 6.2.5p21: the arithmetic types and the pointer types.
///
/// This is the category a condition, a `!`, and both operands of `&&` have to be in. A vector
/// is deliberately not one, because GNU vectors are compared and negated elementwise and
/// letting them through here would silently accept the scalar rules for them.
#[must_use]
pub fn is_scalar(types: &Types, id: TypeId) -> bool {
    is_arithmetic(types, id) || is_pointer(types, id)
}

/// An aggregate type, 6.2.5p21: an array or a `struct`.
///
/// A `union` is not one. That is not a quirk of wording: it is why a `union` is initialized
/// from its first member and an aggregate is initialized member by member.
#[must_use]
pub fn is_aggregate(types: &Types, id: TypeId) -> bool {
    match bare(types, id) {
        TypeKind::Array { .. } => true,
        TypeKind::Record(record) => {
            matches!(types.record_info(record).kind, crate::kind::RecordKind::Struct)
        }
        _ => false,
    }
}

/// An object type, 6.2.5p1: anything that is not a function type.
///
/// `void` is one, and so is an incomplete `struct`. Whether the object can be made is
/// [`is_complete`], and the two questions are asked in different places.
#[must_use]
pub fn is_object(types: &Types, id: TypeId) -> bool {
    !is_function(types, id)
}

/// A complete type: one whose size is known, so an object of it can exist.
///
/// `void` is never complete. An array is complete when its length is known, which includes a
/// variable length array, since the length is known when the declaration is reached even though
/// it is not known here. A `struct`, a `union` or an `enum` is complete once its definition has
/// been seen, which is a property of the declaration and not of the type expression.
#[must_use]
pub fn is_complete(types: &Types, id: TypeId) -> bool {
    match bare(types, id) {
        TypeKind::Void => false,
        TypeKind::Array { len: ArrayLen::Unknown, .. } => false,
        TypeKind::Array { elem, .. } => is_complete(types, elem),
        TypeKind::Record(record) => types.record_info(record).layout.is_some(),
        TypeKind::Enum(id) => types.enum_info(id).underlying.is_some(),
        _ => true,
    }
}

/// Whether a value of this type may be modified, 6.3.2.1p1.
///
/// An array is not modifiable, a `const` object is not, an incomplete type is not, and a
/// `struct` with a `const` member anywhere inside it is not, which is the part that takes a
/// walk rather than a look and the part a compiler forgets.
#[must_use]
pub fn is_modifiable(types: &Types, id: TypeId) -> bool {
    if types.quals(id).has(Qualifiers::CONST) || is_array(types, id) || !is_complete(types, id) {
        return false;
    }
    match bare(types, id) {
        TypeKind::Record(record) => {
            types.record_info(record).fields.iter().all(|field| is_modifiable(types, field.ty))
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_target::{TargetInfo, Triple};

    use super::*;
    use crate::kind::{ArrayLen, FloatKind, IntKind, RecordKind};
    use crate::record::{FieldDecl, RecordOptions, layout_record};

    #[test]
    fn an_enumeration_is_an_integer_type() {
        let mut types = Types::new();
        let id = types.declare_enum(None);
        let int = types.int(IntKind::Int);
        types.complete_enum(id, int, false);
        let enumeration = types.enumeration(id);

        // The rule that gets forgotten, and forgetting it rejects `enum e x; x % 2`.
        assert!(is_integer(&types, enumeration));
        assert!(is_arithmetic(&types, enumeration));
        assert!(is_scalar(&types, enumeration));
    }

    #[test]
    fn atomic_is_in_whatever_category_it_wraps() {
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let atomic = types.atomic(int);

        assert!(is_integer(&types, atomic));
        assert!(is_scalar(&types, atomic));
        assert!(is_atomic(&types, atomic));
        assert!(!is_atomic(&types, int));
    }

    #[test]
    fn a_typedef_answers_as_what_it_names() {
        let mut types = Types::new();
        let mut names = Interner::new();
        let int = types.int(IntKind::Int);
        let name = names.intern("size_t");
        let alias = types.typedef(name, int);

        assert!(is_integer(&types, alias));
        assert!(types.is_sugar(alias));
    }

    #[test]
    fn a_complex_type_is_arithmetic_and_is_not_real() {
        let mut types = Types::new();
        let complex = types.complex(FloatKind::Double);

        assert!(is_arithmetic(&types, complex));
        assert!(is_floating(&types, complex));
        // Which is why `<` on two of them is a constraint violation and `+` is not.
        assert!(!is_real(&types, complex));
    }

    #[test]
    fn void_is_an_object_type_and_is_never_complete() {
        let types = Types::new();
        let void = types.void();

        assert!(is_object(&types, void));
        assert!(!is_complete(&types, void));
        assert!(!is_scalar(&types, void));
    }

    #[test]
    fn a_union_is_not_an_aggregate() {
        let mut types = Types::new();
        let union = types.declare_record(RecordKind::Union, None);
        let union = types.record(union);
        let int = types.int(IntKind::Int);
        let array = types.array(int, ArrayLen::Fixed(2));

        // Not a quirk of wording: it is why a union is initialized from its first member.
        assert!(!is_aggregate(&types, union));
        assert!(is_aggregate(&types, array));
    }

    #[test]
    fn an_incomplete_record_is_an_object_type_that_cannot_be_made() {
        let mut types = Types::new();
        let record = types.declare_record(RecordKind::Struct, None);
        let id = types.record(record);

        assert!(is_object(&types, id));
        assert!(!is_complete(&types, id));
        assert!(!is_modifiable(&types, id));
    }

    #[test]
    fn a_const_member_makes_the_whole_structure_unmodifiable() {
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let constant = types.qualified(int, Qualifiers::CONST);
        let record = types.declare_record(RecordKind::Struct, None);
        let target =
            TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a triple"));
        let laid_out = layout_record(
            &types,
            RecordKind::Struct,
            &[FieldDecl::new(None, constant)],
            &RecordOptions::default(),
            &target,
        )
        .expect("a layout");
        types.complete_record(record, laid_out);
        let id = types.record(record);

        // The part that takes a walk rather than a look, and the part a compiler forgets.
        assert!(!is_modifiable(&types, id));
    }
}
