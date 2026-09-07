//! The psABIs, as descriptions.
//!
//! Design: `spec/cross-compile/06-abis.md` sections 6.1 to 6.5.
//!
//! Five of the fifteen ABIs on section 6.1's list, which is the four the compiler implements by
//! hand today plus Darwin arm64, and Darwin arm64 is the point. Section 6.3 argues that it is a
//! separate ABI rather than AAPCS64 with notes, and the two descriptions here differ in exactly
//! two fields, which is what "separate ABI" turns out to mean once the ABI is a described thing.
//!
//! The ten that are not here are not here because nothing emits for those targets yet. Each of
//! them is a description of this size and none of them needs a new mechanism except ELFv2, whose
//! parameter save area is a frame question rather than a classification one, and the i386 pair,
//! which need a four byte register width that [`Banks::integer_width`] already carries.

use rucc_tuple::{Arch, Os, TargetTuple};

use crate::describe::{
    AbiDescription, Banks, ReturnPointer, Rule, Scalars, Short, StackArgs, Test, Travel, Variadic,
};
use crate::shape::Format;

/// SysV AMD64: x86-64 everywhere but Windows.
///
/// The intricate one, and the one every other ABI on the list is simpler than. An aggregate is
/// cut into eightbytes and each eightbyte is classified by merging what reaches into it, so
/// `struct { int a; float b; }` travels in one general purpose register and the `float` with it.
///
/// The x87 `long double` is the other half. As an argument it sits in the argument area and
/// spends no register, because there is no register file it could travel in. As a return value
/// it comes back on the x87 stack, and so does a `_Complex long double` in st(0) and st(1),
/// which is the one place the `_Complex` flag on a shape changes an answer.
pub static SYSV_AMD64: AbiDescription = AbiDescription {
    name: "SysV AMD64",
    banks: Banks { integer: 6, float: 8, shared: false, integer_width: 8, float_width: 16 },
    scalars: Scalars { in_memory: Some(Format::X87Extended), wide_integer_is_all_or_nothing: true },
    returns: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::X87Stack, Travel::AsFound),
        Rule::new(Test::Eightbytes { limit: 16 }, Travel::AsFound),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    arguments: &[
        Rule::new(Test::Empty, Travel::Ignore),
        // Short of registers is memory without draining, which is where this ABI parts company
        // with AAPCS64: an aggregate that did not fit does not stop a later scalar getting a
        // register.
        Rule::new(Test::Eightbytes { limit: 16 }, Travel::AsFound).short(Short::Memory),
        Rule::new(Test::Anything, Travel::InMemory),
    ],
    return_pointer: ReturnPointer::FirstArgument,
    variadic: Variadic::SameAsFixed,
    stack_args: StackArgs::RegisterSized,
};

/// The shared part of [`AAPCS64`] and [`DARWIN_ARM64`].
///
/// A constant rather than a second copy of the rules, so that "Darwin arm64 differs in two
/// fields" is a fact the file states rather than a claim the reader has to check by diffing.
const AAPCS64_BASE: AbiDescription = AbiDescription {
    name: "AAPCS64",
    banks: Banks { integer: 8, float: 8, shared: false, integer_width: 8, float_width: 16 },
    scalars: Scalars { in_memory: None, wide_integer_is_all_or_nothing: false },
    returns: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::Homogeneous { limit: 4 }, Travel::AsFound),
        Rule::new(Test::SizeAtMost(16), Travel::AsIntegers),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    arguments: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::Homogeneous { limit: 4 }, Travel::AsFound).short(Short::MemoryAndDrain),
        Rule::new(Test::SizeAtMost(16), Travel::AsIntegers).short(Short::MemoryAndDrain),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    // x8, which is not one of the eight argument registers, so a function returning a large
    // structure still has all eight for what it was called with.
    return_pointer: ReturnPointer::Dedicated,
    variadic: Variadic::SameAsFixed,
    stack_args: StackArgs::RegisterSized,
};

/// AAPCS64: AArch64 everywhere but Darwin and Windows.
///
/// Cleaner than SysV and with one idea SysV does not have. An aggregate of at most sixteen bytes
/// travels in general purpose registers, one per eightbyte, and anything larger travels as the
/// address of a copy the caller made. The exception is the homogeneous floating point aggregate:
/// up to four members, all the same floating point type and nothing else in it, in consecutive
/// vector registers. `struct { float x, y, z; }` is three vector registers, and adding one `int`
/// to it makes it eight bytes in one general purpose register instead.
///
/// The draining is what makes it easy to get wrong. An aggregate that wants more registers than
/// are left does not fall back to fewer of them: it goes in the argument area and takes the rest
/// of that bank with it, so the ninth argument of a call is not classified the way the first one
/// is.
pub static AAPCS64: AbiDescription = AAPCS64_BASE;

/// Darwin arm64: macOS and iOS on AArch64.
///
/// Two fields different from [`AAPCS64`], and `spec/cross-compile/06-abis.md` section 6.3 explains why those
/// two are enough to make it a separate ABI rather than a footnote on that one.
///
/// A variadic argument is always in the argument area, whatever registers are left. That is why
/// a variadic call here is ABI-incompatible with a non-variadic one, which means calling an
/// unprototyped function works right up until the day it does not, and it is why `printf("%d",
/// x)` prints garbage on a compiler that got this wrong and nothing else does.
///
/// Stack arguments are packed at their natural size rather than taking a register's worth each,
/// so a function with more than eight arguments returns garbage from the ninth onward if the
/// backend assumed otherwise. Nothing in this crate reads that field; it is here because the
/// backend that does read it should be reading it from the same description the tests are
/// generated from.
///
/// The third divergence, `long double` being a `double`, is in the data layout rather than here,
/// because it is a fact about the type and not about how a value of the type travels. The
/// fourth, x18 being reserved, is a register allocation constraint and `spec/cross-compile/06-abis.md` section
/// 6.7 keeps those as code.
pub static DARWIN_ARM64: AbiDescription = AbiDescription {
    name: "Darwin arm64",
    variadic: Variadic::AlwaysMemory,
    stack_args: StackArgs::Packed,
    ..AAPCS64_BASE
};

/// Windows x64.
///
/// The simplest of the five and the one with the sharpest rule: anything not exactly one, two,
/// four or eight bytes travels as the address of a copy the caller made, so a three byte
/// structure and a three hundred byte structure are passed identically. There is no
/// classification to do and no pair of registers to fill.
///
/// An aggregate that does fit travels as an integer of its size whatever is in it, so a
/// `struct { float x, y; }` arrives in rcx. That is the other half of the shared bank: rcx, rdx,
/// r8 and r9 are the four positions an integer can use, xmm0 to xmm3 are the four a floating
/// point value can use, and they are the same four positions, so a call taking an `int` and then
/// a `double` uses rcx and xmm1 and never xmm0.
///
/// The 32 bytes of shadow space the caller allocates for those four registers, whether or not
/// the callee uses them, is a frame fact and lives with the frame code. Forgetting it corrupts
/// the caller's frame, which is `spec/cross-compile/06-abis.md` section 6.4's first bullet and a good reason
/// for the description to say so somewhere the frame code can find it.
pub static WIN64: AbiDescription = AbiDescription {
    name: "Windows x64",
    banks: Banks { integer: 4, float: 0, shared: true, integer_width: 8, float_width: 16 },
    scalars: Scalars { in_memory: None, wide_integer_is_all_or_nothing: false },
    returns: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::SizeOneOf(&[1, 2, 4, 8]), Travel::AsOneInteger),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    arguments: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::SizeOneOf(&[1, 2, 4, 8]), Travel::AsOneInteger),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    // The address of somewhere to put it is the first argument, in rcx, which moves everything
    // the function was called with one position along.
    return_pointer: ReturnPointer::FirstArgument,
    // A variadic floating point argument goes in the vector register and in the corresponding
    // general purpose one, because the callee does not know which bank to read. It does not
    // change the form the value travels in, so the classifier gives the same answer for a
    // variadic argument as for a fixed one and the backend reads this field.
    variadic: Variadic::BothBanks,
    stack_args: StackArgs::RegisterSized,
};

/// The RISC-V LP64D psABI.
///
/// Two eightbytes for an aggregate that fits, an address for one that does not, and one rule
/// with no analogue in the other four: an aggregate of one or two floating point members travels
/// in floating point registers, and one of a floating point member and an integer member travels
/// in one of each. `struct { double re, im; }` is fa0 and fa1, and `struct { double value; int
/// tag; }` is fa0 and a0.
///
/// The rule stops where the registers do. A `long double` on this ABI is sixteen bytes and a
/// floating point register is eight, so a `long double` is not a floating point member for this
/// purpose and the aggregate holding it is an integer pair like anything else. That single fact
/// is the whole difference between this description and one for LoongArch LP64D, which is why
/// section 6.1 calls that one close to this one.
pub static RISCV_LP64D: AbiDescription = AbiDescription {
    name: "RISC-V LP64D",
    banks: Banks { integer: 8, float: 8, shared: false, integer_width: 8, float_width: 8 },
    scalars: Scalars { in_memory: None, wide_integer_is_all_or_nothing: false },
    returns: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::FloatPair, Travel::AsFound),
        Rule::new(Test::SizeAtMost(16), Travel::AsIntegers),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    arguments: &[
        Rule::new(Test::Empty, Travel::Ignore),
        // A bonus rather than a requirement. An aggregate the rule reached but the registers did
        // not is classified by the ordinary size rules below, and still travels in registers if
        // those find any, which is the one place a rule declines rather than falls back.
        Rule::new(Test::FloatPair, Travel::AsFound).short(Short::TryNextRule),
        Rule::new(Test::SizeAtMost(16), Travel::AsIntegers).short(Short::MemoryAndDrain),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    return_pointer: ReturnPointer::FirstArgument,
    variadic: Variadic::SameAsFixed,
    stack_args: StackArgs::RegisterSized,
};

/// Every ABI described here, which is what the report and the tests iterate.
pub static DESCRIBED: &[&AbiDescription] =
    &[&SYSV_AMD64, &AAPCS64, &DARWIN_ARM64, &WIN64, &RISCV_LP64D];

/// The ABI this target follows, and [`None`] for one whose ABI is not described yet.
///
/// [`None`] is a real answer rather than a gap to be filled in with a guess. `spec/cross-compile/04-target-matrix.md`
/// section 4.2 has a tier for a target the compiler knows about and cannot emit for, and
/// answering with the wrong ABI is the one failure mode `spec/cross-compile/06-abis.md` opens by naming: a
/// program that works for months and then does not.
#[must_use]
pub fn for_target(target: TargetTuple) -> Option<&'static AbiDescription> {
    Some(match (target.arch(), target.os()) {
        (Arch::X86_64, Os::Windows) => &WIN64,
        (Arch::X86_64, _) => &SYSV_AMD64,
        (Arch::Aarch64, os) if os.is_darwin() => &DARWIN_ARM64,
        // Windows on AArch64 is AAPCS64 with different varargs and x18 reserved, per section
        // 6.1. It is not described yet and answering AAPCS64 for it would be answering a
        // question with the almost-right answer, which is the one thing this file must not do.
        (Arch::Aarch64, Os::Windows) => return None,
        (Arch::Aarch64, _) => &AAPCS64,
        (Arch::Riscv64, _) => &RISCV_LP64D,
        _ => return None,
    })
}
