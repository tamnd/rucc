//! The type layouts, against the facts the target documents state.
//!
//! Design: `spec/cross-compile/06-abis.md` section 6.2 items 1 to 3 and item 11.
//!
//! These are the disagreements that make a target's own headers unreadable, which is a different
//! and earlier failure than a miscompiled call. Almost every case here is a pair: the same C on
//! two targets with two answers, because a layout field is only interesting where it differs.

use rucc_abi::{BitfieldOrder, DataLayout, Format};
use rucc_tuple::{TARGETS, TargetEntry, TargetTuple};

/// The layout of the target with this tuple.
fn layout(tuple: &str) -> DataLayout {
    let target: TargetTuple = tuple.parse().expect("a row in the target table");
    DataLayout::for_target(target)
}

#[test]
fn a_long_is_eight_bytes_on_linux_and_four_on_windows() {
    // LLP64 against LP64, which is the field the old three field triple could not carry and the
    // reason `spec/cross-compile/03-target-model.md` puts the data model in the tuple. A header that says
    // `long` where it means a pointer sized integer compiles differently on these two.
    assert_eq!(layout("x86_64-linux-gnu").long_size, 8);
    assert_eq!(layout("x86_64-pc-windows-msvc").long_size, 4);
    assert_eq!(layout("x86_64-pc-windows-gnu").long_size, 4);
    // Pointers are eight bytes on all three, which is what makes the difference a trap rather
    // than an obvious one.
    for tuple in ["x86_64-linux-gnu", "x86_64-pc-windows-msvc", "x86_64-pc-windows-gnu"] {
        assert_eq!(layout(tuple).pointer_size, 8);
    }
}

#[test]
fn a_char_is_signed_on_x86_and_unsigned_on_aarch64_linux() {
    // The classic one, from spec/cross-compile/06-abis.md section 6.2 item 3. A program that indexes an array
    // with a `char` holding a byte over 127 works on one of these and not the other, and nothing
    // in the program is wrong.
    assert!(layout("x86_64-linux-gnu").char_is_signed);
    assert!(!layout("aarch64-linux-gnu").char_is_signed);
    // Darwin signs it on both architectures, so this is not an architecture fact either.
    assert!(layout("aarch64-apple-darwin").char_is_signed);
    assert!(layout("x86_64-apple-darwin").char_is_signed);
    // s390x is unsigned, which the corpus in tamnd/rucc-cross caught and this file used to have
    // backwards. `facts/s390x-linux-gnu.facts` records `char_signed=no`, from `__CHAR_UNSIGNED__`
    // being defined by a cross compiler targeting it, and the ELF ABI supplement agrees.
    assert!(!layout("s390x-linux-gnu").char_is_signed);
}

#[test]
fn mingw_and_msvc_are_the_same_operating_system_with_different_layouts() {
    // Found by the corpus in tamnd/rucc-cross. The rule here used to key on the operating system,
    // which handed every Windows target the MSVC answer, and mingw-w64 is not an MSVC target. It
    // is GCC's answer with Microsoft's `long`, so it needs the environment field and not the OS.
    //
    // `facts/x86_64-windows-gnu.facts` records `long_double_format=x87_extended` with
    // `sizeof_long_double=16`, and `facts/x86_64-windows-msvc.facts` records `double` with 8.
    let mingw = layout("x86_64-pc-windows-gnu").long_double;
    assert_eq!(mingw.format, Format::X87Extended);
    assert_eq!((mingw.size, mingw.align), (16, 16));
    assert!(!layout("x86_64-pc-windows-gnu").long_double_is_double());
    assert!(layout("x86_64-pc-windows-msvc").long_double_is_double());

    // Twelve bytes on i386 under either environment, aligned to four under both, which is worth an
    // assertion because mingw does follow Microsoft on `double` and `long long` just below and the
    // guess that it does the same here is wrong.
    let mingw32 = layout("i686-pc-windows-gnu").long_double;
    assert_eq!(mingw32.format, Format::X87Extended);
    assert_eq!((mingw32.size, mingw32.align), (12, 4));

    // The half mingw does take from Microsoft. `double` and `long long` are eight aligned on
    // i686-windows-gnu and four aligned on i686-linux-gnu, so a struct holding either lays out
    // differently on the two, and the System V i386 psABI only governs the second one.
    let mingw32 = layout("i686-pc-windows-gnu");
    assert_eq!(mingw32.long_long_align, 8);
    assert_eq!(mingw32.double.align, 8);
    assert_eq!(mingw32.max_field_align, None);
}

#[test]
fn s390x_caps_scalar_alignment_at_eight() {
    // The other thing the corpus caught. The s390x ELF ABI caps alignment at eight, so a sixteen
    // byte IEEE quad `long double` is eight aligned there and sixteen aligned on AArch64 Linux
    // with the same format and the same width. `__int128` goes the same way, which is what the
    // cap on member alignment is for.
    let s390x = layout("s390x-linux-gnu").long_double;
    assert_eq!(s390x.format, Format::Quad);
    assert_eq!((s390x.size, s390x.align), (16, 8));
    assert_eq!(layout("s390x-linux-gnu").max_field_align, Some(8));

    let aarch64 = layout("aarch64-linux-gnu").long_double;
    assert_eq!(aarch64.format, s390x.format);
    assert_eq!(aarch64.size, s390x.size);
    assert_ne!(aarch64.align, s390x.align);
}

#[test]
fn a_long_double_has_a_width_and_a_format_and_they_are_separate_facts() {
    // Four targets, four answers, and two of them share a width while disagreeing about every
    // bit in it. A description carrying only the width would call those two the same.
    let linux = layout("x86_64-linux-gnu").long_double;
    assert_eq!(linux.format, Format::X87Extended);
    assert_eq!((linux.size, linux.align), (16, 16));

    let windows = layout("x86_64-pc-windows-msvc").long_double;
    assert_eq!(windows.format, Format::Double);
    assert_eq!((windows.size, windows.align), (8, 8));

    let aarch64 = layout("aarch64-linux-gnu").long_double;
    assert_eq!(aarch64.format, Format::Quad);
    assert_eq!((aarch64.size, aarch64.align), (16, 16));

    // Same sixteen bytes as AArch64 Linux, and not one bit means the same thing, because
    // double-double is a pair of `double`s whose sum is the value rather than a wide binary
    // float.
    let powerpc = layout("powerpc64le-linux-gnu").long_double;
    assert_eq!(powerpc.format, Format::DoubleDouble);
    assert_eq!((powerpc.size, powerpc.align), (16, 16));
}

#[test]
fn darwin_makes_a_long_double_a_double_and_linux_does_not() {
    // spec/cross-compile/06-abis.md section 6.3's third divergence. `%Lf` disagrees, `LDBL_MAX` is wrong, and
    // the `l` suffixed math functions resolve to differently named symbols, all from this field.
    assert!(layout("aarch64-apple-darwin").long_double_is_double());
    assert!(!layout("aarch64-linux-gnu").long_double_is_double());
    assert!(layout("aarch64-pc-windows-msvc").long_double_is_double());
}

#[test]
fn system_v_i386_aligns_an_eight_byte_type_to_four() {
    // Section 6.2 item 2's example of a layout rule that does not follow from the member sizes.
    // `struct { char c; long long v; }` is twelve bytes here and sixteen on x86-64, and both are
    // correct C. It is the psABI and not the architecture that says so, which is why mingw on the
    // same architecture answers eight and has a test of its own above.
    let i386 = layout("i686-linux-gnu");
    assert_eq!((i386.long_long_size, i386.long_long_align), (8, 4));
    assert_eq!(i386.double.align, 4);
    assert_eq!(
        i386.long_double,
        rucc_abi::FloatType { format: Format::X87Extended, size: 12, align: 4 }
    );
    // The cap on member alignment goes with it, so a sixteen byte aligned type inside a struct is
    // aligned to four here and to sixteen on x86-64.
    assert_eq!(i386.max_field_align, Some(4));

    let amd64 = layout("x86_64-linux-gnu");
    assert_eq!((amd64.long_long_size, amd64.long_long_align), (8, 8));
    assert_eq!(amd64.double.align, 8);
    assert_eq!(amd64.max_field_align, None);
}

#[test]
fn a_symbol_gets_a_leading_underscore_on_darwin_and_not_on_elf() {
    // Section 6.2 item 11. Getting it wrong produces a link error rather than a wrong answer,
    // which makes it the least dangerous field here and the one most likely to be forgotten.
    assert!(layout("x86_64-apple-darwin").leading_underscore);
    assert!(layout("aarch64-apple-darwin").leading_underscore);
    assert!(!layout("x86_64-linux-gnu").leading_underscore);
    assert!(!layout("x86_64-pc-windows-msvc").leading_underscore);
}

#[test]
fn a_bitfield_starts_at_the_end_the_endianness_says() {
    assert_eq!(layout("x86_64-linux-gnu").bitfield_order, BitfieldOrder::LowestFirst);
    assert_eq!(layout("aarch64-linux-gnu").bitfield_order, BitfieldOrder::LowestFirst);
    assert_eq!(layout("s390x-linux-gnu").bitfield_order, BitfieldOrder::HighestFirst);
}

#[test]
fn every_target_in_the_table_has_a_layout_that_makes_sense() {
    for entry in TARGETS {
        let target = TargetEntry::parse(entry).expect("the table parses");
        let layout = DataLayout::for_target(target);
        let tuple = entry.tuple;

        // Nothing here is a C rule. These are the ones every psABI on the list happens to agree
        // on, and a row that broke one of them would be a row worth arguing about rather than one
        // to quietly accommodate.
        assert_eq!(layout.short_size, 2, "{tuple} has a short that is not two bytes");
        assert!(layout.int_size >= layout.short_size, "{tuple} has an int narrower than a short");
        assert!(layout.long_size >= layout.int_size, "{tuple} has a long narrower than an int");
        assert!(
            layout.long_long_size >= layout.long_size,
            "{tuple} has a long long narrower than a long"
        );
        assert!(
            layout.long_double.size >= layout.double.size,
            "{tuple} has a long double narrower than a double"
        );
        assert_eq!(
            layout.pointer_size, layout.pointer_align,
            "{tuple} has a pointer aligned to something other than its size"
        );

        // An alignment that is not a power of two is a layout nobody can implement.
        for align in [
            layout.long_long_align,
            layout.pointer_align,
            layout.float.align,
            layout.double.align,
            layout.long_double.align,
        ] {
            assert!(align.is_power_of_two(), "{tuple} aligns something to {align}");
        }
        if let Some(cap) = layout.max_field_align {
            assert!(cap.is_power_of_two(), "{tuple} caps member alignment at {cap}");
        }
    }
}
