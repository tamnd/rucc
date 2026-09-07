//! Type sizes, alignments and signedness, which is the half of a psABI that decides whether a
//! header is read correctly.
//!
//! Design: `spec/cross-compile/06-abis.md` section 6.2 items 1 to 3.
//!
//! Section 6.2 splits a psABI into twelve independent decisions and then splits those into two
//! groups. Items 4 to 9 decide whether a call works, and the differential harness of
//! `spec/cross-compile/14-testing.md` is what tests them. Items 1 to 3 plus item 11 decide whether a *header*
//! is interpreted correctly, and those are this file.
//!
//! The second group is cheaper to test and catches more, which is why `spec/cross-compile/06-abis.md` section
//! 6.8 puts the `_Static_assert` corpus first for every new ABI: it costs nothing to run, needs
//! no target machine and no execution, and it catches every layout disagreement there is. The
//! corpus is generated from this description, which is why the description comes first.
//!
//! # The two traps
//!
//! A `long double`'s width and its format are separate facts. Sixteen bytes on x86-64 Linux of
//! which eighty bits are the value, eight bytes and a plain `double` on Darwin and Windows,
//! IEEE binary128 on AArch64 Linux and s390x and RISC-V, and IBM double-double on legacy
//! 64-bit PowerPC. A description that carried only the width would call three of those the same.
//!
//! `char`'s signedness is a target fact and not a C fact. Unsigned on AArch64 Linux, on ARM, on
//! PowerPC and on s390x, signed on x86 and on Darwin. A program that indexes an array with a
//! `char` holding a byte over 127 works on one and not the other, and nothing in it is wrong.

use rucc_tuple::{Arch, DataModel, Os, TargetTuple};

use crate::shape::Format;

/// A floating point type, as the two separate facts it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatType {
    /// How the bits mean a number.
    pub format: Format,
    /// How many bytes it takes in memory, which is not the format's width.
    pub size: u64,
    /// What it is aligned to, in bytes, which is not always its size: an x87 `long double` is
    /// twelve bytes aligned to four on i386 and sixteen aligned to sixteen on x86-64, and it is
    /// the same eighty bits of value both times.
    pub align: u64,
}

/// How a bit-field is allocated within its storage unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitfieldOrder {
    /// The first field declared gets the low order bits, which is every little-endian target
    /// here.
    LowestFirst,
    /// The first field declared gets the high order bits, which is s390x and every other
    /// big-endian ELF target.
    HighestFirst,
}

/// The type sizes and alignments a target's headers were written against.
///
/// Everything here is a fact the psABI states and the compiler has to agree with. None of it is
/// something C decides, which is the point: two compilers can both implement C correctly and
/// disagree about every field in this struct, and the one that disagrees with the target's
/// headers is the one that is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataLayout {
    /// Whether a plain `char` is signed.
    pub char_is_signed: bool,
    /// The width of a `short` in bytes, which is two everywhere on the target list.
    pub short_size: u64,
    /// The width of an `int` in bytes.
    pub int_size: u64,
    /// The width of a `long` in bytes, which is the field the old three field triple got wrong
    /// for every 64-bit Windows target.
    pub long_size: u64,
    /// The width of a `long long` in bytes.
    pub long_long_size: u64,
    /// What a `long long` is aligned to.
    ///
    /// Four on i386, where it is eight bytes aligned to four, which is `spec/cross-compile/06-abis.md` section
    /// 6.2 item 2's example of a layout rule that is not derivable from the member alignments.
    pub long_long_align: u64,
    /// The width of a pointer in bytes.
    pub pointer_size: u64,
    /// What a pointer is aligned to.
    pub pointer_align: u64,
    /// `float`.
    pub float: FloatType,
    /// `double`.
    pub double: FloatType,
    /// `long double`, which is the one that differs across almost every target.
    pub long_double: FloatType,
    /// Whether a symbol gets a leading underscore, which is section 6.2 item 11.
    pub leading_underscore: bool,
    /// Which end of a storage unit a bit-field starts at.
    pub bitfield_order: BitfieldOrder,
    /// The largest alignment the ABI will give a struct member on its own, in bytes, and [`None`]
    /// where there is no cap.
    ///
    /// Eight on i386 Linux and on ARM's older ABI. A cap is invisible until a program uses a
    /// sixteen byte type inside a struct, and then it is a layout difference rather than an
    /// error.
    pub max_field_align: Option<u64>,
}

impl DataLayout {
    /// The layout for this target.
    ///
    /// Every field is derived from the tuple rather than from the host, which is
    /// `spec/cross-compile/08-sysroots.md` section 8.5's rule applied to types instead of to directories, and
    /// it is what makes `spec/cross-compile/02-the-goal.md` claim 5 checkable: two hosts asking about the same
    /// target get the same answer because there is nothing in here for the host to influence.
    #[must_use]
    pub fn for_target(target: TargetTuple) -> Self {
        let model = target.data_model();
        let pointer = u64::from(target.pointer_width()) / 8;
        Self {
            char_is_signed: target.char_is_signed(),
            short_size: 2,
            int_size: u64::from(model.int_width()) / 8,
            long_size: u64::from(model.long_width()) / 8,
            long_long_size: u64::from(model.long_long_width()) / 8,
            long_long_align: long_long_align(target),
            pointer_size: pointer,
            pointer_align: pointer,
            float: FloatType { format: Format::Single, size: 4, align: 4 },
            double: FloatType { format: Format::Double, size: 8, align: double_align(target) },
            long_double: long_double(target),
            leading_underscore: target.leading_underscore(),
            bitfield_order: match target.is_little_endian() {
                true => BitfieldOrder::LowestFirst,
                false => BitfieldOrder::HighestFirst,
            },
            max_field_align: max_field_align(target),
        }
    }

    /// Whether a `long double` is really a `double`, which is true on Darwin, on Windows and on
    /// every 32-bit ARM target, and which decides whether `%Lf` and `LDBL_MAX` and the `l`
    /// suffixed math functions mean anything different from their unsuffixed forms.
    #[must_use]
    pub const fn long_double_is_double(&self) -> bool {
        matches!(self.long_double.format, Format::Double)
    }
}

/// What a `long long` is aligned to.
fn long_long_align(target: TargetTuple) -> u64 {
    // i386 is the only target on the list where an eight byte type is aligned to four, and it is
    // the reason a struct holding one lays out differently there than the member sizes suggest.
    match (target.arch(), target.data_model()) {
        (Arch::X86, DataModel::Ilp32) => 4,
        _ => 8,
    }
}

/// What a `double` is aligned to.
fn double_align(target: TargetTuple) -> u64 {
    match (target.arch(), target.data_model()) {
        // Same rule as `long long` and the same reason. A `double` inside a struct on i386 sits
        // at a four byte boundary, which is why an i386 struct is often smaller than the same
        // declaration on any other target.
        (Arch::X86, DataModel::Ilp32) => 4,
        _ => 8,
    }
}

/// The `long double` of this target, as a format and a width, which are separate facts.
fn long_double(target: TargetTuple) -> FloatType {
    let double = FloatType { format: Format::Double, size: 8, align: 8 };
    let quad = FloatType { format: Format::Quad, size: 16, align: 16 };
    match target.arch() {
        // Eighty bits of value in twelve bytes on i386 and sixteen on x86-64, aligned to its
        // storage size both times, and the same format underneath. Windows is the exception and
        // it is handled first, because there a `long double` is a `double` and the x87 format
        // never appears in an interface.
        Arch::X86_64 | Arch::X86 if target.os() == Os::Windows => double,
        Arch::X86_64 => FloatType { format: Format::X87Extended, size: 16, align: 16 },
        Arch::X86 => FloatType { format: Format::X87Extended, size: 12, align: 4 },
        // Darwin's third divergence, `spec/cross-compile/06-abis.md` section 6.3. `%Lf` disagrees, `LDBL_MAX`
        // is wrong, and a math library call resolves to a differently named symbol, all from one
        // field.
        Arch::Aarch64 if target.os().is_darwin() => double,
        Arch::Aarch64 if target.os() == Os::Windows => double,
        Arch::Aarch64 | Arch::Riscv64 | Arch::S390x | Arch::LoongArch64 => quad,
        // Thirty two bit ARM has never had anything wider than a `double` for it.
        Arch::Arm | Arch::Riscv32 | Arch::Wasm32 => double,
        // ELFv2 keeps IBM double-double, a pair of `double`s whose sum is the value, which is
        // not an IEEE format at all and is the reason a `long double` there cannot be treated as
        // a wide binary float.
        Arch::PowerPc64 => FloatType { format: Format::DoubleDouble, size: 16, align: 16 },
        Arch::Arm64Ec => double,
    }
}

/// The largest alignment the ABI gives a member on its own.
fn max_field_align(target: TargetTuple) -> Option<u64> {
    match (target.arch(), target.data_model()) {
        // i386 Linux caps member alignment at four, so a sixteen byte aligned type inside a
        // struct is aligned to four there and to sixteen everywhere else.
        (Arch::X86, DataModel::Ilp32) if target.os() != Os::Windows => Some(4),
        _ => None,
    }
}
