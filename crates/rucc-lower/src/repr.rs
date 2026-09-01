//! What a C type is, once the IR is the one asking.
//!
//! Design: `spec/08-ir.md` section 8.2.
//!
//! The IR type system is much smaller than C's, and the gap between them is this file. Integers
//! are signless there and signed or unsigned here, so the signedness comes out and goes on the
//! instruction instead. Pointers are opaque there, so every pointer type and every array and
//! every function is `ptr`. Aggregates are not values there at all, so a `struct` has no type
//! here: it has a size and an alignment and it lives in memory.
//!
//! Nothing in this file makes a decision C has not already made. The width of an `int` and the
//! format of a `long double` are the target's answers and are read from it.

use rucc_ir::{Float, Type};
use rucc_target::TargetInfo;
use rucc_types::{
    ArrayLen, FloatKind, LayoutError, Qualifiers, TypeId, TypeKind, Types, float_format,
    integer_info, layout,
};

/// The IR type a C type's values have, and [`None`] for a type that has none.
///
/// A `struct`, a `union`, an array, a function and `void` each have none, and each for a
/// different reason: the first three live in memory and are reached by address, a function is
/// not an object at all, and `void` is the absence of a value rather than a value of no width.
/// The caller is the one that knows which of those is a mistake where it stands.
#[must_use]
pub(crate) fn value_type(types: &Types, target: &TargetInfo, id: TypeId) -> Option<Type> {
    match types.kind(types.canonical(id)) {
        // `bool` is one bit of value in one byte of storage, and it is the bit that is the
        // type: a `bool` in a register holds nothing but zero or one, and the byte it is
        // stored in is a question for the load and the store, which round a width up.
        TypeKind::Bool => Some(Type::I1),
        TypeKind::Int(_) | TypeKind::BitInt { .. } | TypeKind::Enum(_) => {
            let info = integer_info(types, id, target)?;
            Some(Type::int(info.width))
        }
        TypeKind::Float(kind) => Some(Type::float(format_of(kind, target)?)),
        TypeKind::Pointer(_) => Some(Type::PTR),
        TypeKind::Atomic(inner) => value_type(types, target, inner),
        TypeKind::Vector { elem, len } => {
            let lane = value_type(types, target, elem)?;
            Some(Type::vector(lane, len))
        }
        _ => None,
    }
}

/// The floating point format of a real floating type, and [`None`] for one the IR has no format
/// for, which today is only `__bf16`.
#[must_use]
pub(crate) fn format_of(kind: FloatKind, target: &TargetInfo) -> Option<Float> {
    ir_format(float_format(kind, target))
}

/// The IR's name for a floating point format, and [`None`] for one it has no type for, which
/// today is only `__bf16`.
#[must_use]
pub(crate) fn ir_format(format: rucc_base::float::Format) -> Option<Float> {
    use rucc_base::float::Format;

    match format {
        Format::Half => Some(Float::F16),
        Format::Single => Some(Float::F32),
        Format::Double => Some(Float::F64),
        Format::X87Extended => Some(Float::F80),
        Format::Quad => Some(Float::F128),
        Format::BFloat16 => None,
    }
}

/// The target's format for a real floating type, and [`None`] for a type that is not one.
///
/// The same question [`format_of`] answers, asked of a C type rather than of a kind, which is
/// what the walk has in hand when it needs the constant one for `x++`.
#[must_use]
pub(crate) fn float_format_of(
    types: &Types,
    target: &TargetInfo,
    id: TypeId,
) -> Option<rucc_base::float::Format> {
    match types.kind(types.canonical(id)) {
        TypeKind::Float(kind) => Some(float_format(kind, target)),
        TypeKind::Atomic(inner) => float_format_of(types, target, inner),
        _ => None,
    }
}

/// Whether an object of this type is never written, and so belongs in read-only memory.
///
/// The qualifier on an array is on its element type, because that is where C puts it: `const
/// char s[4]` is an array of four `const char` and the array itself is unqualified. So the
/// question is asked of what is at the bottom of the arrays rather than of the top.
///
/// A `volatile` object is not read-only however it is qualified. `const volatile` is a thing
/// that changes and that this program may not change, which is memory-mapped hardware, and
/// putting it where a write faults is not what was asked for.
#[must_use]
pub(crate) fn is_read_only(types: &Types, id: TypeId) -> bool {
    let mut id = types.canonical(id);
    loop {
        let quals = types.quals(id);
        if quals.has(Qualifiers::VOLATILE) {
            return false;
        }
        if quals.has(Qualifiers::CONST) {
            return true;
        }
        match types.kind(id) {
            TypeKind::Array { elem, .. } => id = types.canonical(elem),
            _ => return false,
        }
    }
}

/// Whether the type's size is not known until the program runs, which is a variable length
/// array or something built out of one.
///
/// `int a[n]` is one and so is `int a[3][n]`, because the outer array's size is three times a
/// number nobody has yet. What is not one is `int (*p)[n]`: a pointer to a variably modified
/// type is an ordinary pointer, and only the thing it points at has a size that varies.
#[must_use]
pub(crate) fn is_variable_length(types: &Types, id: TypeId) -> bool {
    match types.kind(types.canonical(id)) {
        TypeKind::Array { elem, len } => {
            matches!(len, ArrayLen::Variable(_) | ArrayLen::Star) || is_variable_length(types, elem)
        }
        _ => false,
    }
}

/// Whether the type is one whose values a signed operation applies to.
///
/// The question the IR asks constantly, because the signedness that C keeps in the type is kept
/// in the opcode there: `sdiv` and `udiv` are two instructions over one type. A floating type is
/// signed and a pointer is not, which is what makes a comparison of two pointers unsigned.
#[must_use]
pub(crate) fn is_signed(types: &Types, target: &TargetInfo, id: TypeId) -> bool {
    match integer_info(types, id, target) {
        Some(info) => info.signed,
        None => !matches!(types.kind(types.canonical(id)), TypeKind::Pointer(_)),
    }
}

/// The size in bytes of a complete object type, and zero for one with no layout.
///
/// Zero rather than a failure, because a type with no layout in a place that needs one has been
/// reported by the checking already, and the walk carries on so that the rest of the function is
/// still worth reading.
#[must_use]
pub(crate) fn size_of(types: &Types, target: &TargetInfo, id: TypeId) -> u64 {
    match layout(types, id, target) {
        Ok(layout) => layout.size,
        // GNU C gives both of these the size one, which is what `sizeof(void)` and `sizeof(f)`
        // answer with, and what pointer arithmetic over `void *` steps by.
        Err(LayoutError::Incomplete | LayoutError::Function) => 1,
        Err(_) => 0,
    }
}

/// The alignment in bytes of a type, and one for a type with no layout.
#[must_use]
pub(crate) fn align_of(types: &Types, target: &TargetInfo, id: TypeId) -> u32 {
    match layout(types, id, target) {
        Ok(layout) => u32::try_from(layout.align).unwrap_or(1).max(1),
        Err(_) => 1,
    }
}

/// The integer type a pointer is as wide as, which is what an address arrives as when it is not
/// yet an address.
#[must_use]
pub(crate) fn address_type(target: &TargetInfo) -> Type {
    Type::int(target.pointer_width)
}

#[cfg(test)]
mod tests {
    use rucc_types::{ArrayLen, IntKind};

    use super::*;

    fn target() -> TargetInfo {
        TargetInfo::new("x86_64-unknown-linux-gnu".parse().expect("a triple"))
    }

    #[test]
    fn the_scalar_types_have_ir_types_and_the_rest_have_none() {
        let mut types = Types::new();
        let target = target();
        let int = types.int(IntKind::Int);
        let long = types.int(IntKind::Long);
        let pointer = types.pointer(int);
        let array = types.array(int, ArrayLen::Fixed(4));
        assert_eq!(value_type(&types, &target, int), Some(Type::int(32)));
        assert_eq!(value_type(&types, &target, long), Some(Type::int(64)));
        assert_eq!(value_type(&types, &target, types.boolean()), Some(Type::I1));
        assert_eq!(value_type(&types, &target, pointer), Some(Type::PTR));
        assert_eq!(value_type(&types, &target, types.void()), None);
        // An array is reached by address and is not a value, which is what makes the walk go
        // through a place for one rather than through a register.
        assert_eq!(value_type(&types, &target, array), None);
    }

    #[test]
    fn the_qualifier_on_an_array_is_the_one_on_its_elements() {
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let konst = types.qualified(int, Qualifiers::CONST);
        let array = types.array(konst, ArrayLen::Fixed(4));
        assert!(is_read_only(&types, konst));
        assert!(is_read_only(&types, array));
        assert!(!is_read_only(&types, int));
        let plain = types.array(int, ArrayLen::Fixed(4));
        assert!(!is_read_only(&types, plain));

        // `const volatile` is memory-mapped hardware, which is written by something that is
        // not this program and does not belong where a write faults.
        let both = types.qualified(int, Qualifiers::CONST.with(Qualifiers::VOLATILE));
        assert!(!is_read_only(&types, both));
    }

    #[test]
    fn a_variable_length_array_is_one_however_deeply_it_is_buried() {
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let fixed = types.array(int, ArrayLen::Fixed(4));
        let variable = types.array(int, ArrayLen::Star);
        let outer = types.array(variable, ArrayLen::Fixed(3));
        let pointer = types.pointer(variable);
        assert!(!is_variable_length(&types, fixed));
        assert!(is_variable_length(&types, variable));
        assert!(is_variable_length(&types, outer));
        // A pointer to one is an ordinary pointer, and only what it points at varies.
        assert!(!is_variable_length(&types, pointer));
    }

    #[test]
    fn a_size_the_type_does_not_have_is_the_one_gnu_c_gives_it() {
        let mut types = Types::new();
        let target = target();
        let incomplete = types.array(types.int(IntKind::Int), ArrayLen::Unknown);
        assert_eq!(size_of(&types, &target, types.void()), 1);
        assert_eq!(size_of(&types, &target, incomplete), 1);
        assert_eq!(align_of(&types, &target, incomplete), 1);
    }
}
