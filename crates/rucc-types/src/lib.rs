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
//! Records are laid out by [`layout_record`], which takes the members and gives back their
//! offsets, and the result is handed to [`Types::complete_record`] so that the record then has
//! a size like any other type. Bit-fields, `packed`, `#pragma pack`, `aligned`, zero width
//! bit-fields and flexible array members are all in there, and every one of their rules was
//! measured against gcc and clang rather than read off a document.
//!
//! [`promote`] and [`usual_arithmetic`] are 6.3.1.1 and 6.3.1.8, the rules that decide what
//! type an arithmetic expression has. Their answers were read out of gcc and clang with
//! `_Generic` naming the type of every interesting pair, which is also how the C23 changes were
//! pinned down: `_BitInt` does not promote, and an enumeration promotes through whatever it is
//! represented in.
//!
//! # Status
//!
//! The type universe, the interner, the canonical and sugar split, the qualifier rules, layout
//! with records included, and the arithmetic conversions are implemented.
//!
//! Not here yet, and named so that the gaps are not mistaken for decisions: type compatibility
//! and the composite type, the decimal floating types, and printing a type back as C
//! declaration syntax.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-types/0.2.0")]

mod convert;
mod kind;
mod layout;
mod record;
mod types;

pub use crate::convert::{promote, promote_bit_field, usual_arithmetic};
pub use crate::kind::{
    ArrayLen, EnumId, FloatKind, FunctionId, FunctionType, IntKind, Qualifiers, RecordId,
    RecordKind, Type, TypeKind, VlaId,
};
pub use crate::layout::{Layout, LayoutError, float_width, int_width, layout};
pub use crate::record::{
    Field, FieldDecl, RecordError, RecordLayout, RecordOptions, layout_record,
};
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

    /// Lays out a record with no attributes on it, on x86-64 Linux.
    fn lay_out(types: &Types, kind: RecordKind, fields: &[FieldDecl]) -> RecordLayout {
        layout_record(types, kind, fields, &RecordOptions::default(), &linux())
            .expect("a record every member of which has a layout")
    }

    /// The offsets of the members, in bits, which is what a measurement of a real compiler
    /// gives back once its byte offsets and its bit dumps are put together.
    fn offsets(laid_out: &RecordLayout) -> Vec<u64> {
        laid_out.fields.iter().map(|field| field.offset).collect()
    }

    /// A complete record type built out of the given members.
    fn record(types: &mut Types, kind: RecordKind, fields: &[FieldDecl]) -> TypeId {
        let id = types.declare_record(kind, None);
        let laid_out = lay_out(types, kind, fields);
        types.complete_record(id, laid_out);
        types.record(id)
    }

    /// An ordinary member of the given type, unnamed, which is all most of these tests need.
    fn member(ty: TypeId) -> FieldDecl {
        FieldDecl::new(None, ty)
    }

    /// A named bit-field, which is what a measurement of a real compiler has to use to be able
    /// to read the field back.
    fn bits(interner: &mut Interner, name: &str, ty: TypeId, width: u32) -> FieldDecl {
        FieldDecl::bit_field(Some(interner.intern(name)), ty, width)
    }

    /// An unnamed bit-field, which occupies bits and raises nothing.
    fn unnamed_bits(ty: TypeId, width: u32) -> FieldDecl {
        FieldDecl::bit_field(None, ty, width)
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
        let long_long = types.int(IntKind::LongLong);
        let laid_out = lay_out(&types, RecordKind::Struct, &[member(long_long); 2]);
        types.complete_record(id, laid_out);
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
        let long_long = types.int(IntKind::LongLong);
        let plain = record(&mut types, RecordKind::Struct, &[member(long_long); 2]);
        let atomic = types.atomic(plain);
        assert_eq!(layout(&types, plain, &linux).unwrap(), Layout::new(16, 8));
        assert_eq!(layout(&types, atomic, &linux).unwrap(), Layout::new(16, 16));

        // An odd size cannot be accessed atomically in one go, so nothing is raised.
        let odd = record(&mut types, RecordKind::Struct, &[member(long_long); 3]);
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
    fn a_struct_puts_each_member_at_the_next_offset_it_is_allowed_to_start_at() {
        let types = Types::new();
        let char_ = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let laid_out = lay_out(&types, RecordKind::Struct, &[member(char_), member(int)]);
        assert_eq!(laid_out.layout, Layout::new(8, 4));
        assert_eq!(offsets(&laid_out), [0, 32]);
        assert_eq!(laid_out.fields[1].byte_offset(), 4);

        // And the tail is padded, which is what makes an array of the thing work.
        let long_long = types.int(IntKind::LongLong);
        let laid_out = lay_out(&types, RecordKind::Struct, &[member(long_long), member(char_)]);
        assert_eq!(laid_out.layout, Layout::new(16, 8));
    }

    #[test]
    fn a_union_starts_every_member_at_zero_and_is_as_large_as_the_largest() {
        let mut types = Types::new();
        let char_ = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let laid_out = lay_out(&types, RecordKind::Union, &[member(char_), member(int)]);
        assert_eq!(laid_out.layout, Layout::new(4, 4));
        assert_eq!(offsets(&laid_out), [0, 0]);

        // Nine bytes and a short is ten, not nine and not sixteen: the size is rounded up to
        // the alignment rather than to the largest member.
        let nine = types.array(char_, ArrayLen::Fixed(9));
        let short = types.int(IntKind::Short);
        let laid_out = lay_out(&types, RecordKind::Union, &[member(nine), member(short)]);
        assert_eq!(laid_out.layout, Layout::new(10, 2));
    }

    #[test]
    fn bit_fields_share_a_unit_until_one_of_them_would_span_two() {
        // Measured with gcc 13.3 on x86-64 Linux and clang on AArch64 Darwin, including where
        // the bits landed, by setting each field to all ones and dumping the bytes.
        let mut interner = Interner::new();
        let types = Types::new();
        let char_ = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let long_long = types.int(IntKind::LongLong);

        let fields = [bits(&mut interner, "a", int, 3), bits(&mut interner, "b", int, 5)];
        let laid_out = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(laid_out.layout, Layout::new(4, 4));
        assert_eq!(offsets(&laid_out), [0, 3]);

        // Thirty bits do not fit in what is left of the first int, so they start a new one.
        let fields = [member(char_), bits(&mut interner, "b", int, 30)];
        let laid_out = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(laid_out.layout, Layout::new(8, 4));
        assert_eq!(offsets(&laid_out), [0, 32]);

        // Thirty three bits of a `long long` do fit in what is left of the first one, because
        // the unit is eight bytes rather than four, so they stay where they are.
        let fields = [member(char_), bits(&mut interner, "b", long_long, 33)];
        let laid_out = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(laid_out.layout, Layout::new(8, 8));
        assert_eq!(offsets(&laid_out), [0, 8]);

        // An ordinary member after a bit-field starts at the next byte it is allowed to.
        let fields = [bits(&mut interner, "a", int, 3), member(char_)];
        let laid_out = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(offsets(&laid_out), [0, 8]);
    }

    #[test]
    fn a_zero_width_bit_field_moves_the_next_member_on_and_nothing_else() {
        let types = Types::new();
        let char_ = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let fields = [member(char_), unnamed_bits(int, 0), member(char_)];
        let laid_out = lay_out(&types, RecordKind::Struct, &fields);
        // Five bytes aligned to one: the zero width field pushed the second `char` to offset
        // four without giving the record the alignment of an `int`. Both compilers report that.
        assert_eq!(laid_out.layout, Layout::new(5, 1));
        assert_eq!(offsets(&laid_out), [0, 32, 32]);
        assert_eq!(laid_out.fields.len(), 3, "one field per declaration, so indices line up");
    }

    #[test]
    fn an_unnamed_bit_field_does_not_raise_the_records_alignment_but_a_named_one_does() {
        let mut interner = Interner::new();
        let types = Types::new();
        let char_ = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);

        let unnamed = [member(char_), unnamed_bits(int, 20)];
        let unnamed = lay_out(&types, RecordKind::Struct, &unnamed);
        assert_eq!(unnamed.layout, Layout::new(4, 1));

        let named = [member(char_), bits(&mut interner, "b", int, 20)];
        let named = lay_out(&types, RecordKind::Struct, &named);
        assert_eq!(named.layout, Layout::new(4, 4));
        assert_eq!(offsets(&named), [0, 8], "the same place either way");

        // The unit an unnamed field has to fit inside is still its own type's, so this one
        // moves to bit thirty two and the record is eight bytes aligned to one.
        let wider = [member(char_), unnamed_bits(int, 30)];
        let wider = lay_out(&types, RecordKind::Struct, &wider);
        assert_eq!(wider.layout, Layout::new(8, 1));
        assert_eq!(offsets(&wider), [0, 32]);
    }

    #[test]
    fn packed_drops_every_member_to_a_byte_and_bit_fields_to_the_next_free_bit() {
        let mut interner = Interner::new();
        let types = Types::new();
        let char_ = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let packed = RecordOptions { packed: true, ..RecordOptions::default() };

        let fields = [member(char_), member(int)];
        let laid_out = layout_record(&types, RecordKind::Struct, &fields, &packed, &linux())
            .expect("a packed struct of two complete members");
        assert_eq!(laid_out.layout, Layout::new(5, 1));
        assert_eq!(offsets(&laid_out), [0, 8]);

        let fields = [member(char_), bits(&mut interner, "b", int, 30)];
        let laid_out = layout_record(&types, RecordKind::Struct, &fields, &packed, &linux())
            .expect("a packed struct with a bit-field");
        assert_eq!(laid_out.layout, Layout::new(5, 1));
        assert_eq!(offsets(&laid_out), [0, 8], "no boundary left to move to");

        // A zero width bit-field still rounds to its own type, packed or not, which is the
        // whole reason a program writes one inside a packed structure.
        let fields = [member(char_), unnamed_bits(int, 0), member(char_)];
        let laid_out = layout_record(&types, RecordKind::Struct, &fields, &packed, &linux())
            .expect("a packed struct with a zero width bit-field");
        assert_eq!(laid_out.layout, Layout::new(5, 1));
        assert_eq!(offsets(&laid_out), [0, 32, 32]);
    }

    #[test]
    fn pragma_pack_caps_alignment_and_leaves_a_bit_field_where_it_already_is() {
        let mut interner = Interner::new();
        let types = Types::new();
        let char_ = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let pack = RecordOptions { pack: Some(2), ..RecordOptions::default() };

        let fields = [member(char_), member(int)];
        let laid_out = layout_record(&types, RecordKind::Struct, &fields, &pack, &linux())
            .expect("a packed struct of two complete members");
        assert_eq!(laid_out.layout, Layout::new(6, 2));
        assert_eq!(offsets(&laid_out), [0, 16]);

        // Six bytes with the field at bit eight, not at bit sixteen. Once the alignment has
        // been capped below the type's own there is no boundary to move to, so the field stays
        // put. Measured, because moving it is at least as plausible a reading.
        let fields = [member(char_), bits(&mut interner, "b", int, 30)];
        let laid_out = layout_record(&types, RecordKind::Struct, &fields, &pack, &linux())
            .expect("a packed struct with a bit-field");
        assert_eq!(laid_out.layout, Layout::new(6, 2));
        assert_eq!(offsets(&laid_out), [0, 8]);

        // The same structure with the field unnamed is five bytes aligned to one, because the
        // capped alignment reached it through the record and an unnamed field gives none back.
        let fields = [member(char_), unnamed_bits(int, 30)];
        let laid_out = layout_record(&types, RecordKind::Struct, &fields, &pack, &linux())
            .expect("a packed struct with an unnamed bit-field");
        assert_eq!(laid_out.layout, Layout::new(5, 1));
    }

    #[test]
    fn an_alignment_the_program_asked_for_raises_the_member_and_the_record() {
        let types = Types::new();
        let char_ = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);

        let aligned = FieldDecl { align: Some(16), ..member(int) };
        let laid_out = lay_out(&types, RecordKind::Struct, &[member(char_), aligned]);
        assert_eq!(laid_out.layout, Layout::new(32, 16));
        assert_eq!(offsets(&laid_out), [0, 128]);

        // `packed, aligned(4)` together: the members pack and the record does not, which is
        // the combination the attribute pair exists for.
        let options = RecordOptions { packed: true, align: Some(4), pack: None };
        let fields = [member(char_), member(int)];
        let laid_out = layout_record(&types, RecordKind::Struct, &fields, &options, &linux())
            .expect("a packed struct with an alignment asked for");
        assert_eq!(laid_out.layout, Layout::new(8, 4));
        assert_eq!(offsets(&laid_out), [0, 8]);
    }

    #[test]
    fn a_flexible_array_member_costs_nothing_but_its_alignment() {
        // What makes `malloc(sizeof(struct S) + n)` the idiom it is.
        let mut types = Types::new();
        let char_ = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let long_long = types.int(IntKind::LongLong);

        let chars = types.array(char_, ArrayLen::Unknown);
        let laid_out = lay_out(&types, RecordKind::Struct, &[member(int), member(chars)]);
        assert_eq!(laid_out.layout, Layout::new(4, 4));
        assert_eq!(offsets(&laid_out), [0, 32]);

        // The alignment still applies, so this is eight bytes of which one is the `char`.
        let longs = types.array(long_long, ArrayLen::Unknown);
        let laid_out = lay_out(&types, RecordKind::Struct, &[member(char_), member(longs)]);
        assert_eq!(laid_out.layout, Layout::new(8, 8));
        assert_eq!(offsets(&laid_out), [0, 64]);

        // Anywhere but last it is an incomplete member, and which member is part of the answer.
        let fields = [member(chars), member(int)];
        let error =
            layout_record(&types, RecordKind::Struct, &fields, &RecordOptions::default(), &linux());
        assert_eq!(error, Err(RecordError::Member { index: 0, error: LayoutError::Incomplete }));
    }

    #[test]
    fn a_record_with_no_members_is_zero_bytes_aligned_to_one() {
        // The GNU empty structure, which C itself does not have and which real headers do.
        let types = Types::new();
        let laid_out = lay_out(&types, RecordKind::Struct, &[]);
        assert_eq!(laid_out.layout, Layout::new(0, 1));
    }

    #[test]
    fn a_bit_field_wider_than_the_type_it_is_declared_with_is_refused() {
        let types = Types::new();
        let int = types.int(IntKind::Int);
        let fields = [unnamed_bits(int, 33)];
        let error =
            layout_record(&types, RecordKind::Struct, &fields, &RecordOptions::default(), &linux());
        let want = RecordError::BitFieldTooWide { index: 0, width: 33, capacity: 32 };
        assert_eq!(error, Err(want));
    }

    #[test]
    fn a_record_reports_its_members_once_it_has_been_completed() {
        let mut interner = Interner::new();
        let mut types = Types::new();
        let char_ = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let name = interner.intern("count");
        let fields = [member(char_), FieldDecl::new(Some(name), int)];
        let id = types.declare_record(RecordKind::Struct, None);
        let laid_out = lay_out(&types, RecordKind::Struct, &fields);
        types.complete_record(id, laid_out);
        let ty = types.record(id);
        assert_eq!(layout(&types, ty, &linux()).unwrap(), Layout::new(8, 4));
        let field = types.field(id, name).expect("the member that was declared");
        assert_eq!(field.byte_offset(), 4);
        assert!(!field.is_bit_field());
        assert_eq!(types.field(id, interner.intern("missing")), None);
    }

    #[test]
    fn a_nested_record_brings_its_own_alignment_with_it() {
        let mut types = Types::new();
        let char_ = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let inner = record(&mut types, RecordKind::Struct, &[member(char_)]);
        let laid_out = lay_out(&types, RecordKind::Struct, &[member(inner), member(int)]);
        assert_eq!(laid_out.layout, Layout::new(8, 4));
        assert_eq!(offsets(&laid_out), [0, 32]);

        // An anonymous member is an ordinary member with no name, so the same code lays it out
        // and the four bytes of padding after the `char` are there either way.
        let anonymous = record(&mut types, RecordKind::Struct, &[member(int), member(char_)]);
        let laid_out = lay_out(&types, RecordKind::Struct, &[member(char_), member(anonymous)]);
        assert_eq!(laid_out.layout, Layout::new(12, 4));
        assert_eq!(offsets(&laid_out), [0, 32]);
    }

    #[test]
    fn everything_narrower_than_an_int_promotes_to_one() {
        // Measured by naming the type of `+x` with `_Generic` in gcc 13.3 and clang 18. Every
        // one of these answers `int`, including the unsigned ones, because an `int` holds every
        // value a sixteen bit unsigned type has.
        let mut types = Types::new();
        let linux = linux();
        let int = types.int(IntKind::Int);
        let narrow =
            [IntKind::Char, IntKind::SChar, IntKind::UChar, IntKind::Short, IntKind::UShort];
        for kind in narrow {
            let ty = types.int(kind);
            assert_eq!(promote(&mut types, ty, &linux), int, "{}", kind.as_str());
        }
        let boolean = types.boolean();
        assert_eq!(promote(&mut types, boolean, &linux), int, "C23 made bool a real type");

        // From `int` up, a type is its own promotion.
        for kind in [IntKind::Int, IntKind::UInt, IntKind::Long, IntKind::ULongLong] {
            let ty = types.int(kind);
            assert_eq!(promote(&mut types, ty, &linux), ty, "{}", kind.as_str());
        }
    }

    #[test]
    fn a_bit_int_is_not_promoted_at_all() {
        // C23 6.3.1.1p2, and the point of the type. `_BitInt(8) + _BitInt(8)` stays eight bits
        // wide where `char + char` is an `int`, which is what makes the width mean something.
        let mut types = Types::new();
        let linux = linux();
        let small = types.bit_int(true, 8);
        assert_eq!(promote(&mut types, small, &linux), small);
        assert_eq!(usual_arithmetic(&mut types, small, small, &linux), Some(small));
    }

    #[test]
    fn a_bit_field_is_promoted_by_its_width_and_not_by_its_type() {
        let mut types = Types::new();
        let linux = linux();
        let int = types.int(IntKind::Int);
        let uint = types.int(IntKind::UInt);
        let ullong = types.int(IntKind::ULongLong);

        // Three bits of an unsigned field all fit in an `int`, so it is signed afterwards.
        assert_eq!(promote_bit_field(&mut types, uint, 3, &linux), int);
        // Thirty two of them do not.
        assert_eq!(promote_bit_field(&mut types, uint, 32, &linux), uint);
        // Twenty bits of a signed field, which is an `int` either way.
        assert_eq!(promote_bit_field(&mut types, int, 20, &linux), int);
        // Forty bits keep the declared type. The C17 wording says `unsigned int` here, which
        // would silently drop eight bits; both compilers answer the declared type instead.
        assert_eq!(promote_bit_field(&mut types, ullong, 40, &linux), ullong);
    }

    #[test]
    fn an_enumeration_promotes_through_what_it_is_represented_in() {
        let mut types = Types::new();
        let linux = linux();
        let int = types.int(IntKind::Int);
        let short = types.int(IntKind::Short);
        let uint = types.int(IntKind::UInt);

        // `enum E : short` promotes the same way a `short` does, which is to `int`.
        let fixed = types.declare_enum(None);
        types.complete_enum(fixed, short, true);
        let fixed = types.enumeration(fixed);
        assert_eq!(promote(&mut types, fixed, &linux), int);

        // An enumeration all of whose enumerators are non-negative is represented in
        // `unsigned int` by both compilers, and then it promotes to itself.
        let unsigned = types.declare_enum(None);
        types.complete_enum(unsigned, uint, false);
        let unsigned = types.enumeration(unsigned);
        assert_eq!(promote(&mut types, unsigned, &linux), uint);

        // An enumeration nobody has decided on yet answers `int`, so that an expression using
        // one is still checkable while the diagnostic about it is being written.
        let undecided = types.declare_enum(None);
        let undecided = types.enumeration(undecided);
        assert_eq!(promote(&mut types, undecided, &linux), int);
    }

    #[test]
    fn the_qualifiers_and_the_atomic_come_off_before_anything_else() {
        // By the time a value is being promoted the lvalue conversion has already happened, so
        // `_Atomic const int` and `int` are the same operand.
        let mut types = Types::new();
        let linux = linux();
        let int = types.int(IntKind::Int);
        let konst = types.qualified(int, Qualifiers::CONST);
        let atomic = types.atomic(konst);
        assert_eq!(promote(&mut types, atomic, &linux), int);
        assert_eq!(usual_arithmetic(&mut types, atomic, konst, &linux), Some(int));
    }

    #[test]
    fn the_usual_arithmetic_conversions_between_the_standard_integer_types() {
        // Every row measured with `_Generic` in gcc 13.3 and clang 18 on x86-64 Linux.
        let mut types = Types::new();
        let linux = linux();
        let cases = [
            (IntKind::Int, IntKind::UInt, IntKind::UInt),
            (IntKind::Int, IntKind::Long, IntKind::Long),
            (IntKind::UInt, IntKind::Long, IntKind::Long),
            (IntKind::UInt, IntKind::ULong, IntKind::ULong),
            (IntKind::Int, IntKind::LongLong, IntKind::LongLong),
            (IntKind::UInt, IntKind::LongLong, IntKind::LongLong),
            (IntKind::ULong, IntKind::LongLong, IntKind::ULongLong),
            (IntKind::Char, IntKind::Char, IntKind::Int),
            (IntKind::UChar, IntKind::UShort, IntKind::Int),
        ];
        for (left, right, want) in cases {
            let left = types.int(left);
            let right = types.int(right);
            let want = types.int(want);
            assert_eq!(usual_arithmetic(&mut types, left, right, &linux), Some(want));
            assert_eq!(usual_arithmetic(&mut types, right, left, &linux), Some(want), "either way");
        }
    }

    #[test]
    fn the_last_arm_takes_the_unsigned_type_of_the_wider_one() {
        // `unsigned long + long long` is `unsigned long long` on Linux: the `long long` wins on
        // rank and cannot hold every value of the `unsigned long`, so neither operand's own
        // type is the answer. This is the arm programs are surprised by.
        let mut types = Types::new();
        let linux = linux();
        let ulong = types.int(IntKind::ULong);
        let long_long = types.int(IntKind::LongLong);
        let want = types.int(IntKind::ULongLong);
        assert_eq!(usual_arithmetic(&mut types, ulong, long_long, &linux), Some(want));

        // The same pair on Windows, where `long` is thirty two bits, comes out as `long long`,
        // because there it does hold every value. A host-driven implementation gets one of
        // these two wrong.
        let windows = target("x86_64-pc-windows-msvc");
        assert_eq!(usual_arithmetic(&mut types, ulong, long_long, &windows), Some(long_long));
    }

    #[test]
    fn a_bit_int_is_ranked_by_its_width_against_the_standard_types() {
        // Measured with clang 18 on x86-64 Linux, which is the compiler that has `_BitInt`.
        let mut types = Types::new();
        let linux = linux();
        let b40 = types.bit_int(true, 40);
        let ub40 = types.bit_int(false, 40);
        let b8 = types.bit_int(true, 8);
        let b32 = types.bit_int(true, 32);
        let int = types.int(IntKind::Int);
        let uint = types.int(IntKind::UInt);
        let long = types.int(IntKind::Long);
        let char_ = types.int(IntKind::Char);

        // Wider than an `int`, so it outranks one.
        assert_eq!(usual_arithmetic(&mut types, b40, int, &linux), Some(b40));
        // Narrower than a `long`, so it loses to one.
        assert_eq!(usual_arithmetic(&mut types, b40, long, &linux), Some(long));
        // The same width as an `int`, and a standard type wins the tie.
        assert_eq!(usual_arithmetic(&mut types, b32, int, &linux), Some(int));
        assert_eq!(usual_arithmetic(&mut types, b32, uint, &linux), Some(uint));
        // The other side promotes first, so a `char` next to a narrow `_BitInt` is an `int`
        // and the `_BitInt` loses to it.
        assert_eq!(usual_arithmetic(&mut types, b8, char_, &linux), Some(int));
        // Unsigned and higher ranked wins outright, and unsigned and lower ranked loses to a
        // signed type wide enough to hold it.
        assert_eq!(usual_arithmetic(&mut types, ub40, int, &linux), Some(ub40));
        assert_eq!(usual_arithmetic(&mut types, ub40, long, &linux), Some(long));
        // Two bit-precise types of the same width and different signedness.
        assert_eq!(usual_arithmetic(&mut types, b40, ub40, &linux), Some(ub40));
    }

    #[test]
    fn a_floating_operand_decides_the_answer_whatever_the_other_side_is() {
        let mut types = Types::new();
        let linux = linux();
        let float = types.float(FloatKind::Float);
        let double = types.float(FloatKind::Double);
        let long_double = types.float(FloatKind::LongDouble);
        let ullong = types.int(IntKind::ULongLong);
        let int = types.int(IntKind::Int);

        assert_eq!(usual_arithmetic(&mut types, int, float, &linux), Some(float));
        assert_eq!(usual_arithmetic(&mut types, float, double, &linux), Some(double));
        assert_eq!(usual_arithmetic(&mut types, double, long_double, &linux), Some(long_double));
        // Sixty four bits of unsigned integer against a `float`, which is a `float` and loses
        // most of them. That is the rule rather than an oversight.
        assert_eq!(usual_arithmetic(&mut types, ullong, float, &linux), Some(float));
    }

    #[test]
    fn a_complex_operand_makes_the_answer_complex_after_the_real_types_have_combined() {
        let mut types = Types::new();
        let linux = linux();
        let cfloat = types.complex(FloatKind::Float);
        let cdouble = types.complex(FloatKind::Double);
        let cldouble = types.complex(FloatKind::LongDouble);
        let double = types.float(FloatKind::Double);
        let long_double = types.float(FloatKind::LongDouble);
        let float = types.float(FloatKind::Float);
        let int = types.int(IntKind::Int);

        assert_eq!(usual_arithmetic(&mut types, cfloat, double, &linux), Some(cdouble));
        assert_eq!(usual_arithmetic(&mut types, cfloat, int, &linux), Some(cfloat));
        assert_eq!(usual_arithmetic(&mut types, cdouble, long_double, &linux), Some(cldouble));
        assert_eq!(usual_arithmetic(&mut types, cfloat, float, &linux), Some(cfloat));
    }

    #[test]
    fn an_operand_that_is_not_arithmetic_has_no_common_type() {
        // The caller is the one holding the span, so this says no rather than guessing.
        let mut types = Types::new();
        let linux = linux();
        let int = types.int(IntKind::Int);
        let pointer = types.pointer(int);
        assert_eq!(usual_arithmetic(&mut types, pointer, int, &linux), None);
        assert_eq!(usual_arithmetic(&mut types, pointer, pointer, &linux), None);
        let void = types.void();
        assert_eq!(usual_arithmetic(&mut types, void, int, &linux), None);
        // And a type that is not arithmetic is still its own promotion, so a caller may promote
        // first and ask questions afterwards.
        assert_eq!(promote(&mut types, pointer, &linux), pointer);
    }

    #[test]
    fn the_conversions_read_through_sugar() {
        let mut interner = Interner::new();
        let mut types = Types::new();
        let linux = linux();
        let char_ = types.int(IntKind::Char);
        let name = types.typedef(interner.intern("byte"), char_);
        let int = types.int(IntKind::Int);
        assert_eq!(promote(&mut types, name, &linux), int);
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
