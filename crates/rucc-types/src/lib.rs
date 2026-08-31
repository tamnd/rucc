//! The C type system, interned, and layout computation.
//!
//! Design: `spec/07-types-and-semantics.md`. Layer rank 2, see `spec/18-package-layout.md`.
//!
//! There is one [`Types`] per translation unit and it owns every type in it. A [`TypeId`] is
//! four bytes and two of them are equal exactly when they are the same type, which turns the
//! question the compiler asks more often than any other into an integer comparison.
//!
//! Two ideas shape the rest of it.
//!
//! **Sugar is kept and never decided on.** `typedef int32_t;` gives a node that remembers the
//! name and points at canonical `int`. Every semantic rule reads [`Types::canonical`] and sees
//! `int`; every diagnostic reads the type as it was written and says `int32_t`. Compilers that
//! throw the name away produce messages nobody can act on, and compilers that decide on the
//! name produce wrong answers, and both are common. Sugar is not only at the outermost node,
//! so `int32_t *` and `int32_t[4]` are sugar too and canonicalising rebuilds them.
//!
//! **`_Atomic` is a type, not a qualifier.** `const` and `volatile` and `restrict` are a
//! bitmask in the interning key, because nothing about them changes what an object is. C lets
//! `_Atomic` be written in the same position, but `_Atomic(T)` can have a different alignment
//! from `T`, so it is a type constructor here and the parser is what maps the spelling onto
//! it. Document 01 recorded a compiler that treated it as a qualifier and lost track of it,
//! which is exactly the shortcut that makes atomics silently wrong.
//!
//! Layout comes out of [`TargetInfo`](rucc_target::TargetInfo) and never out of the host.
//! `long` is four bytes on Windows and eight on Linux, and `long double` is eight bytes on
//! Apple and sixteen on SysV x86-64, so a cross compiler that asks its own platform is wrong
//! twice before it has read a line of C.
//!
//! ```
//! use rucc_target::{TargetInfo, Triple};
//! use rucc_types::{IntKind, Types, layout};
//!
//! let mut types = Types::new();
//! let linux = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
//! let windows = TargetInfo::new("x86_64-pc-windows-msvc".parse::<Triple>().unwrap());
//!
//! let long = types.int(IntKind::Long);
//! assert_eq!(layout(&types, long, &linux).unwrap().size, 8);
//! assert_eq!(layout(&types, long, &windows).unwrap().size, 4);
//! ```
//!
//! # Status
//!
//! The type universe, the interner, the canonical and sugar split, the qualifier rules and
//! the layout of everything that is not a record are implemented. Record layout is the next
//! piece: a `struct` or a `union` is declared here and carries a layout slot that whoever
//! walks its members fills in, because member offsets, bit-field packing and the alignment
//! attributes are a body of rules of their own.
//!
//! Not here yet, and named so that the gaps are not mistaken for decisions: the integer
//! promotions and the usual arithmetic conversions, type compatibility and the composite type,
//! and printing a type back as C declaration syntax.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-types/0.2.0")]

mod kind;
mod layout;
mod types;

pub use crate::kind::{
    ArrayLen, EnumId, FloatKind, FunctionId, FunctionType, IntKind, Qualifiers, RecordId,
    RecordKind, Type, TypeKind, VlaId,
};
pub use crate::layout::{Layout, LayoutError, float_width, int_width, layout};
pub use crate::types::{EnumInfo, RecordInfo, TypeId, Types};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M2";

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_target::{TargetInfo, Triple};

    use super::*;

    fn target(triple: &str) -> TargetInfo {
        TargetInfo::new(triple.parse::<Triple>().expect("a triple the compiler supports"))
    }

    fn linux() -> TargetInfo {
        target("x86_64-unknown-linux-gnu")
    }

    #[test]
    fn milestone_is_recorded() {
        assert!(MILESTONE.starts_with('M'));
    }

    #[test]
    fn the_same_type_asked_for_twice_is_the_same_id() {
        let mut types = Types::new();
        let a = types.pointer(types.int(IntKind::Int));
        let b = types.pointer(types.int(IntKind::Int));
        assert_eq!(a, b, "interning is what makes type identity an integer comparison");
        let c = types.pointer(types.int(IntKind::Long));
        assert_ne!(a, c);
    }

    #[test]
    fn a_qualifier_makes_a_different_type_with_the_same_shape() {
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let konst = types.qualified(int, Qualifiers::CONST);
        assert_ne!(int, konst);
        assert_eq!(types.kind(konst), types.kind(int));
        assert!(types.quals(konst).has(Qualifiers::CONST));
        assert_eq!(types.unqualified(konst), int);
    }

    #[test]
    fn qualifiers_accumulate_and_do_not_depend_on_the_order_they_were_written() {
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let a = types.qualified(int, Qualifiers::CONST);
        let a = types.qualified(a, Qualifiers::VOLATILE);
        let b = types.qualified(int, Qualifiers::VOLATILE);
        let b = types.qualified(b, Qualifiers::CONST);
        assert_eq!(a, b, "`const volatile int` and `volatile const int` are one type");
    }

    #[test]
    fn qualifying_an_array_qualifies_its_element() {
        // 6.7.3p10, and not a shortcut. An array type has no qualifiers of its own, so if this
        // put the `const` on the array then `const` on an array parameter would mean nothing.
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let array = types.array(int, ArrayLen::Fixed(4));
        let konst = types.qualified(array, Qualifiers::CONST);
        assert!(types.quals(konst).is_none(), "the array itself is unqualified");
        let TypeKind::Array { elem, len } = types.kind(konst) else {
            panic!("still an array");
        };
        assert_eq!(len, ArrayLen::Fixed(4));
        assert!(types.quals(elem).has(Qualifiers::CONST));
    }

    #[test]
    fn a_typedef_is_a_different_type_that_means_the_same_thing() {
        let mut interner = Interner::new();
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let name = types.typedef(interner.intern("int32_t"), int);
        assert_ne!(name, int, "the sugar survives, so a diagnostic can print it");
        assert_eq!(types.canonical(name), int, "and no rule ever sees it");
        assert!(types.is_sugar(name));
        assert!(!types.is_sugar(int));
    }

    #[test]
    fn sugar_below_the_outermost_node_is_resolved_too() {
        // The bug this is here for: canonicalising only the top node leaves `int32_t *` and
        // `int *` as different types, and then every rule stated on pointers stops firing.
        let mut interner = Interner::new();
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let name = types.typedef(interner.intern("int32_t"), int);
        let sugar_pointer = types.pointer(name);
        let plain_pointer = types.pointer(int);
        assert_ne!(sugar_pointer, plain_pointer);
        assert_eq!(types.canonical(sugar_pointer), plain_pointer);

        let sugar_array = types.array(name, ArrayLen::Fixed(3));
        let plain_array = types.array(int, ArrayLen::Fixed(3));
        assert_eq!(types.canonical(sugar_array), plain_array);
    }

    #[test]
    fn a_typedef_of_a_typedef_canonicalises_all_the_way_down() {
        let mut interner = Interner::new();
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let mut current = int;
        for i in 0..8 {
            current = types.typedef(interner.intern(&format!("t{i}")), current);
        }
        assert_eq!(types.canonical(current), int);
    }

    #[test]
    fn a_qualified_typedef_keeps_the_name_and_canonicalises_to_the_qualified_type() {
        let mut interner = Interner::new();
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let name = types.typedef(interner.intern("int32_t"), int);
        let konst = types.qualified(name, Qualifiers::CONST);
        assert!(matches!(types.kind(konst), TypeKind::Typedef { .. }), "still prints as int32_t");
        let want = types.qualified(int, Qualifiers::CONST);
        assert_eq!(types.canonical(konst), want);
    }

    #[test]
    fn a_typedef_of_an_array_pushes_a_qualifier_to_the_element_when_it_canonicalises() {
        // `typedef int A[4]; const A x;` declares an array of `const int`, which is where the
        // array rule and the sugar rule have to agree with each other.
        let mut interner = Interner::new();
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let array = types.array(int, ArrayLen::Fixed(4));
        let name = types.typedef(interner.intern("A"), array);
        let konst = types.qualified(name, Qualifiers::CONST);
        let konst_int = types.qualified(int, Qualifiers::CONST);
        let want = types.array(konst_int, ArrayLen::Fixed(4));
        assert_eq!(types.canonical(konst), want);
    }

    #[test]
    fn a_function_type_is_deduplicated_by_its_signature() {
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let long = types.int(IntKind::Long);
        let make = |types: &mut Types, params: Vec<TypeId>, variadic| {
            types.function(FunctionType { ret: int, params, variadic, prototyped: true })
        };
        let a = make(&mut types, vec![int, long], false);
        let b = make(&mut types, vec![int, long], false);
        assert_eq!(a, b);
        assert_ne!(a, make(&mut types, vec![int, long], true), "`...` is part of the type");
        assert_ne!(a, make(&mut types, vec![long, int], false));
    }

    #[test]
    fn a_function_type_written_with_a_typedef_canonicalises_through_its_signature() {
        let mut interner = Interner::new();
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let name = types.typedef(interner.intern("int32_t"), int);
        let sugar = types.function(FunctionType {
            ret: name,
            params: vec![name],
            variadic: false,
            prototyped: true,
        });
        let plain = types.function(FunctionType {
            ret: int,
            params: vec![int],
            variadic: false,
            prototyped: true,
        });
        assert_ne!(sugar, plain);
        assert_eq!(types.canonical(sugar), plain);
    }

    #[test]
    fn a_record_is_its_declaration_and_not_its_members() {
        // Two structs written the same way in one translation unit are different types. The
        // looser relation that does hold between them is compatibility, which is a separate
        // question from identity and is answered elsewhere.
        let mut interner = Interner::new();
        let mut types = Types::new();
        let tag = interner.intern("point");
        let first = types.declare_record(RecordKind::Struct, Some(tag));
        let second = types.declare_record(RecordKind::Struct, Some(tag));
        assert_ne!(types.record(first), types.record(second));
        assert_eq!(types.record(first), types.record(first));
    }

    #[test]
    fn a_record_has_no_layout_until_it_has_been_completed() {
        let mut types = Types::new();
        let id = types.declare_record(RecordKind::Struct, None);
        let ty = types.record(id);
        assert_eq!(layout(&types, ty, &linux()), Err(LayoutError::Incomplete));
        types.complete_record(id, Layout::new(16, 8));
        assert_eq!(layout(&types, ty, &linux()).unwrap(), Layout::new(16, 8));
    }

    #[test]
    fn an_enum_takes_the_layout_of_its_underlying_type() {
        let mut types = Types::new();
        let id = types.declare_enum(None);
        let ty = types.enumeration(id);
        assert_eq!(layout(&types, ty, &linux()), Err(LayoutError::Incomplete));
        let int = types.int(IntKind::Int);
        types.complete_enum(id, int, false);
        assert_eq!(layout(&types, ty, &linux()).unwrap(), Layout::new(4, 4));
    }

    #[test]
    fn the_scalar_widths_come_from_the_target() {
        let mut types = Types::new();
        let linux = linux();
        let windows = target("x86_64-pc-windows-msvc");
        let darwin = target("aarch64-apple-darwin");

        let long = types.int(IntKind::Long);
        assert_eq!(layout(&types, long, &linux).unwrap(), Layout::new(8, 8));
        assert_eq!(layout(&types, long, &windows).unwrap(), Layout::new(4, 4), "LLP64");

        let ldouble = types.float(FloatKind::LongDouble);
        assert_eq!(layout(&types, ldouble, &linux).unwrap(), Layout::new(16, 16));
        assert_eq!(layout(&types, ldouble, &darwin).unwrap(), Layout::new(8, 8));

        let pointer = types.pointer(types.void());
        assert_eq!(layout(&types, pointer, &linux).unwrap(), Layout::new(8, 8));

        let boolean = types.boolean();
        assert_eq!(layout(&types, boolean, &linux).unwrap(), Layout::new(1, 1));
    }

    #[test]
    fn a_complex_type_is_two_of_its_component_with_the_components_alignment() {
        // `_Complex long double` on SysV x86-64 is thirty two bytes aligned to sixteen, which
        // is the case that catches an implementation that aligns the pair to its own size.
        let mut types = Types::new();
        let linux = linux();
        let cfloat = types.complex(FloatKind::Float);
        assert_eq!(layout(&types, cfloat, &linux).unwrap(), Layout::new(8, 4));
        let cdouble = types.complex(FloatKind::Double);
        assert_eq!(layout(&types, cdouble, &linux).unwrap(), Layout::new(16, 8));
        let cldouble = types.complex(FloatKind::LongDouble);
        assert_eq!(layout(&types, cldouble, &linux).unwrap(), Layout::new(32, 16));
        let darwin = target("aarch64-apple-darwin");
        assert_eq!(layout(&types, cldouble, &darwin).unwrap(), Layout::new(16, 8));
    }

    #[test]
    fn an_atomic_type_can_be_more_aligned_than_the_type_it_wraps() {
        // The whole reason `_Atomic` is a type here rather than a qualifier. A sixteen byte
        // record is aligned to eight and the atomic version of it is aligned to sixteen.
        let mut types = Types::new();
        let linux = linux();
        let record = types.declare_record(RecordKind::Struct, None);
        types.complete_record(record, Layout::new(16, 8));
        let plain = types.record(record);
        let atomic = types.atomic(plain);
        assert_eq!(layout(&types, plain, &linux).unwrap(), Layout::new(16, 8));
        assert_eq!(layout(&types, atomic, &linux).unwrap(), Layout::new(16, 16));

        // An odd size cannot be accessed atomically in one go, so nothing is raised.
        let odd = types.declare_record(RecordKind::Struct, None);
        types.complete_record(odd, Layout::new(24, 8));
        let odd = types.record(odd);
        let atomic_odd = types.atomic(odd);
        assert_eq!(layout(&types, atomic_odd, &linux).unwrap(), Layout::new(24, 8));

        let int = types.int(IntKind::Int);
        let atomic_int = types.atomic(int);
        assert_eq!(layout(&types, atomic_int, &linux).unwrap(), Layout::new(4, 4));
    }

    #[test]
    fn a_bit_int_is_laid_out_like_a_standard_integer_until_it_outgrows_one() {
        // Measured with clang 18 on x86-64 Linux and clang on AArch64 Darwin. The two disagree
        // above sixty four bits, which is why the granule is a target fact.
        let mut types = Types::new();
        let linux = linux();
        let darwin = target("aarch64-apple-darwin");
        let cases = [(7, 1, 1), (8, 1, 1), (9, 2, 2), (17, 4, 4), (33, 8, 8), (64, 8, 8)];
        for (width, size, align) in cases {
            let ty = types.bit_int(true, width);
            assert_eq!(layout(&types, ty, &linux).unwrap(), Layout::new(size, align), "{width}");
            assert_eq!(layout(&types, ty, &darwin).unwrap(), Layout::new(size, align), "{width}");
        }
        for width in [65, 96, 128] {
            let ty = types.bit_int(false, width);
            assert_eq!(layout(&types, ty, &linux).unwrap(), Layout::new(16, 8), "{width}");
            assert_eq!(layout(&types, ty, &darwin).unwrap(), Layout::new(16, 16), "{width}");
        }
        let wide = types.bit_int(true, 129);
        assert_eq!(layout(&types, wide, &linux).unwrap(), Layout::new(24, 8));
        assert_eq!(layout(&types, wide, &darwin).unwrap(), Layout::new(32, 16));
    }

    #[test]
    fn an_array_is_its_element_repeated_and_keeps_its_elements_alignment() {
        let mut types = Types::new();
        let linux = linux();
        let int = types.int(IntKind::Int);
        let ty = types.array(int, ArrayLen::Fixed(10));
        assert_eq!(layout(&types, ty, &linux).unwrap(), Layout::new(40, 4));
        let nested = types.array(ty, ArrayLen::Fixed(3));
        assert_eq!(layout(&types, nested, &linux).unwrap(), Layout::new(120, 4));
    }

    #[test]
    fn an_array_without_a_size_is_incomplete_and_an_impossible_one_says_so() {
        let mut types = Types::new();
        let linux = linux();
        let int = types.int(IntKind::Int);
        for len in [ArrayLen::Unknown, ArrayLen::Star, ArrayLen::Variable(VlaId(0))] {
            let ty = types.array(int, len);
            assert_eq!(layout(&types, ty, &linux), Err(LayoutError::Incomplete));
        }
        let huge = types.array(int, ArrayLen::Fixed(u64::MAX));
        assert_eq!(layout(&types, huge, &linux), Err(LayoutError::TooLarge));
    }

    #[test]
    fn two_variable_length_arrays_of_the_same_element_are_still_different_types() {
        let mut types = Types::new();
        let int = types.int(IntKind::Int);
        let a = types.array(int, ArrayLen::Variable(VlaId(0)));
        let b = types.array(int, ArrayLen::Variable(VlaId(1)));
        assert_ne!(a, b);
    }

    #[test]
    fn a_vector_is_rounded_up_to_a_power_of_two_and_aligned_to_the_whole_thing() {
        // What GCC does with a `vector_size` that is not already one, checked against clang on
        // AArch64 Darwin, which accepts the three element case that GCC rejects outright.
        let mut types = Types::new();
        let linux = linux();
        let int = types.int(IntKind::Int);
        let four = types.vector(int, 4);
        assert_eq!(layout(&types, four, &linux).unwrap(), Layout::new(16, 16));
        let three = types.vector(int, 3);
        assert_eq!(layout(&types, three, &linux).unwrap(), Layout::new(16, 16));
        let three_chars = types.vector(types.int(IntKind::Char), 3);
        assert_eq!(layout(&types, three_chars, &linux).unwrap(), Layout::new(4, 4));
    }

    #[test]
    fn the_types_without_a_size_say_which_kind_of_without_they_are() {
        // Kept apart because GNU C gives both of them a size of one and a different warning,
        // and because a caller that cannot tell them apart cannot write either message.
        let mut types = Types::new();
        let linux = linux();
        let void = types.void();
        assert_eq!(layout(&types, void, &linux), Err(LayoutError::Incomplete));
        let int = types.int(IntKind::Int);
        let function = types.function(FunctionType {
            ret: int,
            params: Vec::new(),
            variadic: false,
            prototyped: true,
        });
        assert_eq!(layout(&types, function, &linux), Err(LayoutError::Function));
        let pointer_to_function = types.pointer(function);
        assert_eq!(layout(&types, pointer_to_function, &linux).unwrap(), Layout::new(8, 8));
    }

    #[test]
    fn layout_reads_through_sugar() {
        let mut interner = Interner::new();
        let mut types = Types::new();
        let long = types.int(IntKind::Long);
        let name = types.typedef(interner.intern("word"), long);
        let array = types.array(name, ArrayLen::Fixed(4));
        assert_eq!(layout(&types, array, &linux()).unwrap(), Layout::new(32, 8));
    }
}
