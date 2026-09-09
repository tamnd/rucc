//! The psABIs, as descriptions.
//!
//! Design: `spec/cross-compile/06-abis.md` sections 6.1 to 6.5.
//!
//! Six of the fifteen ABIs on section 6.1's list, which is the four the compiler implements by
//! hand today plus Darwin arm64 and i386 SysV. Darwin arm64 is the point of the first five:
//! section 6.3 argues that it is a separate ABI rather than AAPCS64 with notes, and the two
//! descriptions here differ in exactly two fields, which is what "separate ABI" turns out to mean
//! once the ABI is a described thing.
//!
//! i386 SysV is the point of the sixth. It was added as data with no change under `src/` other
//! than the description itself and the two lines of dispatch, which is what section 6.7's proposal
//! predicted and the first time it has been tested on an ABI that ships rather than on the
//! fixture in `tests/descriptions.rs`. It is also the first description with a four byte register
//! width, which nothing in this crate can observe on it, because an ABI with no argument registers
//! never asks how wide one is. [`Banks::integer_width`] earns its place there anyway: with
//! [`StackArgs::RegisterSized`] it is what says an argument slot on i386 is four bytes and not
//! eight, and that is read by the backend rather than by the classifier.
//!
//! # What the next three cost, which is not nothing
//!
//! The nine that are not here are not here because nothing emits for those targets yet, and the
//! claim this file used to make was that each of them is a description of this size and none of
//! them needs a new mechanism. Writing three of them out is how that claim gets checked, and it
//! does not survive in that form. Two of the three need something the language cannot say. Both
//! are about *which* register rather than about how many, and neither is reachable by adding
//! another [`Test`], which is what makes them mechanisms rather than policies.
//!
//! **A floating point scalar that takes two vector registers.** s390x passes a 128-bit `long
//! double` in an even and odd pair of floating point registers, f0 with f2 or f4 with f6. The
//! model here says a floating point value either fits one vector register, decided by
//! [`Banks::float_width`], or moves to the general purpose bank the way a `long double` does on
//! RISC-V LP64D. There is no third answer, so the aggregate half of s390x is describable today and
//! the `long double` half is not. That is why s390x is still the fixture in
//! `tests/descriptions.rs` and is not dispatched to from [`for_target`]: a description that is
//! right about structures and wrong about one scalar is the almost-right answer this file must
//! not give.
//!
//! **An argument that has to start on an even numbered register.** LoongArch LP64D puts a `long
//! double` in a pair of general purpose registers aligned to an even one, and AAPCS32 puts a
//! 64-bit value in r0 and r1 or in r2 and r3 and never in r1 and r2. A bank here is a count, and a
//! count cannot carry a parity constraint, so an odd number of preceding arguments gives the wrong
//! register on both. LoongArch is otherwise identical to RISC-V LP64D, which is the shape section
//! 6.1 predicted, and identical-except-one-mechanism is still not identical.
//!
//! Neither gap is expensive to close and neither is closed here, because closing a mechanism on
//! behalf of a target with no backend is how a mechanism ends up fitted to a guess. They are
//! written down so the next person to reach for LoongArch finds the reason it is absent rather
//! than the absence.

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

/// The i386 System V psABI: 32-bit x86 on Linux and the other ELF systems.
///
/// The one with no argument registers at all. Every argument is in the argument area, in source
/// order, each rounded up to four bytes, and every aggregate return value comes back in memory
/// through a hidden first argument. There is no classification left to do once that is said,
/// which is why this description is the shortest one in the file and why it is the one worth
/// having: an ABI whose answer is always the same is still an ABI, and a target with no
/// description gets no answer rather than the easy one.
///
/// Both bank sizes are zero and both register widths are set anyway. The widths are not read by
/// the classifier here, because an ABI with no argument registers never asks how wide one is, and
/// they are not zero because [`Banks::integer_width`] is what tells the backend that an argument
/// slot on i386 is four bytes rather than eight, and because a vector register holding nothing is
/// a statement the property test in `tests/descriptions.rs` refuses on every description rather
/// than carrying an exception for this one.
///
/// The `long double` here is the eighty bit x87 one, twelve bytes and four byte aligned, and it
/// travels in the argument area like everything else. That makes [`Scalars::in_memory`]
/// unnecessary rather than wrong: the field exists to move a scalar off a register bank, and
/// there is no bank to move it off.
///
/// Returning a small structure in edx:eax is a real convention and it is not this one. GCC calls
/// it `-freg-struct-return`, Darwin and some BSDs default to it, and Linux does not, so the
/// return list below says memory for every size. Getting that backwards is the failure this crate
/// exists to avoid, and it is why the dispatch below answers for the ELF systems and declines
/// i686 Windows, whose stdcall and fastcall decoration is a different ABI wearing the same
/// architecture.
pub static I386_SYSV: AbiDescription = AbiDescription {
    name: "i386 SysV",
    banks: Banks { integer: 0, float: 0, shared: false, integer_width: 4, float_width: 8 },
    scalars: Scalars { in_memory: None, wide_integer_is_all_or_nothing: false },
    returns: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    arguments: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::Anything, Travel::InMemory),
    ],
    return_pointer: ReturnPointer::FirstArgument,
    // Everything is in the argument area already, so there is nothing a variadic argument could
    // do differently. This is the only ABI on the list where that is true for a reason rather
    // than by coincidence.
    variadic: Variadic::SameAsFixed,
    stack_args: StackArgs::RegisterSized,
};

/// Every ABI described here, which is what the report and the tests iterate.
pub static DESCRIBED: &[&AbiDescription] =
    &[&SYSV_AMD64, &AAPCS64, &DARWIN_ARM64, &WIN64, &RISCV_LP64D, &I386_SYSV];

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
        // Windows on 32-bit x86 is not this one. stdcall, fastcall and thiscall each pass and
        // clean up differently and each decorates the symbol name, which makes it the only place
        // C has mangling, per `spec/cross-compile/04-target-matrix.md`. Answering i386 SysV for it
        // would be right for the arguments and wrong for the name, and a link failure is the good
        // outcome there.
        (Arch::X86, Os::Windows) => return None,
        (Arch::X86, _) => &I386_SYSV,
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
