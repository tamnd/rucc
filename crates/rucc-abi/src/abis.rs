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

use rucc_tuple::{Arch, Env, Os, TargetTuple};

use crate::describe::{
    AbiDescription, Banks, BitInts, Cleanup, Narrow, ReturnPointer, Rule, Scalars, Short,
    StackArgs, Test, Travel, Variadic,
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
    scalars: Scalars {
        in_memory: Some(Format::X87Extended),
        wide_integer_is_all_or_nothing: true,
        wide_integer_starts_even: false,
        wide_integer_drains: false,
        wide_is_by_reference: false,
        wide_integer_returns_in: None,
        wide_integer_in_memory: false,
        decimal_in_integers: false,
        quad_returns_by_reference: false,
    },
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
    narrow: Narrow::Unspecified,
    bit_ints: BitInts::SpareBits,
    cleanup: Cleanup::Caller,
};

/// The shared part of [`AAPCS64`] and [`DARWIN_ARM64`].
///
/// A constant rather than a second copy of the rules, so that "Darwin arm64 differs in three
/// fields" is a fact the file states rather than a claim the reader has to check by diffing.
const AAPCS64_BASE: AbiDescription = AbiDescription {
    name: "AAPCS64",
    banks: Banks { integer: 8, float: 8, shared: false, integer_width: 8, float_width: 16 },
    scalars: Scalars {
        in_memory: None,
        wide_integer_is_all_or_nothing: false,
        wide_integer_starts_even: true,
        wide_integer_drains: true,
        wide_is_by_reference: false,
        wide_integer_returns_in: None,
        wide_integer_in_memory: false,
        decimal_in_integers: false,
        quad_returns_by_reference: false,
    },
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
    narrow: Narrow::Unspecified,
    bit_ints: BitInts::Untaught,
    cleanup: Cleanup::Caller,
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
/// Three fields different from [`AAPCS64`], and `spec/cross-compile/06-abis.md` section 6.3 explains why the
/// first two are enough to make it a separate ABI rather than a footnote on that one.
///
/// A variadic argument is always in the argument area, whatever registers are left. That is why
/// a variadic call here is ABI-incompatible with a non-variadic one, which means calling an
/// unprototyped function works right up until the day it does not, and it is why `printf("%d",
/// x)` prints garbage on a compiler that got this wrong and nothing else does.
///
/// Stack arguments are packed at their natural size rather than taking a register's worth each,
/// so a function with more than eight arguments returns garbage from the ninth onward if the
/// backend assumed otherwise. The backend reads that field to place each argument, and
/// [`crate::Call::in_memory`] reads it to say what an aggregate in the argument area is aligned
/// to, which is its own alignment for a homogeneous floating point aggregate and a word for
/// anything else.
///
/// An `__int128` goes in the next two x registers whatever their numbers are, where AAPCS64 starts
/// it at an even one. clang for arm64-apple-darwin passes one after an `int` in x1 and x2.
///
/// The third divergence, `long double` being a `double`, is in the data layout rather than here,
/// because it is a fact about the type and not about how a value of the type travels. The
/// fourth, x18 being reserved, is a register allocation constraint and `spec/cross-compile/06-abis.md` section
/// 6.7 keeps those as code.
pub static DARWIN_ARM64: AbiDescription = AbiDescription {
    name: "Darwin arm64",
    scalars: Scalars { wide_integer_starts_even: false, ..AAPCS64_BASE.scalars },
    variadic: Variadic::AlwaysMemory,
    stack_args: StackArgs::Packed,
    narrow: Narrow::ToInt,
    bit_ints: BitInts::Untaught,
    cleanup: Cleanup::Caller,
    ..AAPCS64_BASE
};

/// Windows on AArch64.
///
/// [`AAPCS64`] in every call to a function without a `...`, and in the value coming back from any
/// function, and one field different, which is what a `...` does. Microsoft's convention homes the
/// eight x registers of a variadic callee directly below the arguments the caller left in memory
/// and walks the whole run with a `char *`, so everything such a function is passed has to be in
/// that run, the named arguments included. A `double` goes as its bits in the next x register, a
/// structure of up to sixteen bytes goes in x registers whatever its members are, and one over
/// sixteen bytes goes as the address of a copy, homogeneous or not. That is
/// [`Variadic::IntegersOnly`], and it is what clang for aarch64-w64-mingw32 and Microsoft's own
/// compiler both emit.
///
/// A structure that finds too few x registers left goes to the argument area and the x registers
/// after it are spent, as the rule it shares with AAPCS64 says. Microsoft's document allows the
/// structure to be split between x7 and the stack instead, and clang does not split it, and clang
/// is what the rest of a mingw program was built with. The other difference people list, x18 being
/// the thread's own register, is a fact about the register allocator and lives in `rucc-target`
/// with the registers.
pub static WINDOWS_ARM64: AbiDescription =
    AbiDescription { name: "Windows arm64", variadic: Variadic::IntegersOnly, ..AAPCS64_BASE };

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
///
/// The size rule is the whole rule, so it reaches a scalar as well. A `long double`, a
/// `_Float128` and an `__int128` are sixteen bytes here and none of the four positions holds one,
/// so each of them travels as the address of a copy the caller made and comes back through the
/// address the caller passed, which is what [`Scalars::wide_is_by_reference`] says.
pub static WIN64: AbiDescription = AbiDescription {
    name: "Windows x64",
    banks: Banks { integer: 4, float: 0, shared: true, integer_width: 8, float_width: 16 },
    scalars: Scalars {
        in_memory: None,
        wide_integer_is_all_or_nothing: false,
        wide_integer_starts_even: false,
        wide_integer_drains: false,
        wide_is_by_reference: true,
        wide_integer_returns_in: Some(Format::Quad),
        wide_integer_in_memory: false,
        decimal_in_integers: true,
        quad_returns_by_reference: false,
    },
    returns: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::SizeOneOf(&[1, 2, 4, 8]), Travel::AsOneInteger),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    // No rule for an empty aggregate going in, unlike coming back. Its size is not one of the
    // four, so gcc passes it the way it passes any other size that is not, as the address of a
    // copy, and it takes up one of the four positions. A caller that passed nothing for it would
    // put every argument after it one position early, which is what va-arg-22 in the gcc torture
    // tests caught. An empty aggregate coming back is nothing, as it is everywhere else.
    arguments: &[
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
    narrow: Narrow::Unspecified,
    bit_ints: BitInts::Untaught,
    cleanup: Cleanup::Caller,
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
    scalars: Scalars {
        in_memory: None,
        wide_integer_is_all_or_nothing: false,
        wide_integer_starts_even: false,
        wide_integer_drains: false,
        wide_is_by_reference: false,
        wide_integer_returns_in: None,
        wide_integer_in_memory: false,
        decimal_in_integers: false,
        quad_returns_by_reference: false,
    },
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
    narrow: Narrow::Unspecified,
    bit_ints: BitInts::Untaught,
    cleanup: Cleanup::Caller,
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
/// return list below says memory for every size but one. A `_Complex float` is not a structure, and
/// it comes back in edx:eax, which is what libgcc's `__mulsc3` does too. Getting that backwards is the failure this crate
/// exists to avoid, and it is why i686 Windows, where the small structure rule is the default,
/// has [`I386_MINGW`] and [`I386_MSVC`] rather than this one.
pub static I386_SYSV: AbiDescription = AbiDescription {
    name: "i386 SysV",
    banks: Banks { integer: 0, float: 0, shared: false, integer_width: 4, float_width: 8 },
    scalars: Scalars {
        in_memory: None,
        wide_integer_is_all_or_nothing: false,
        wide_integer_starts_even: false,
        wide_integer_drains: false,
        wide_is_by_reference: false,
        wide_integer_returns_in: None,
        wide_integer_in_memory: false,
        decimal_in_integers: false,
        quad_returns_by_reference: true,
    },
    returns: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::ComplexFloat, Travel::AsIntegers),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    arguments: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::Anything, Travel::InMemory),
    ],
    return_pointer: ReturnPointer::FirstArgumentPopped,
    // Everything is in the argument area already, so there is nothing a variadic argument could
    // do differently. This is the only ABI on the list where that is true for a reason rather
    // than by coincidence.
    variadic: Variadic::SameAsFixed,
    stack_args: StackArgs::RegisterSized,
    narrow: Narrow::Unspecified,
    bit_ints: BitInts::Untaught,
    cleanup: Cleanup::Caller,
};

/// i386 System V with the first `n` words of arguments in eax, edx and ecx, which is gcc's
/// `regparm(n)` and what `-mregparm=n` makes of every function in a unit. The Linux kernel is
/// built with `-mregparm=3` on 32 bit x86, its boot and real mode code included, and `asmlinkage`
/// there is `regparm(0)`, the plain convention, for the functions assembly calls.
///
/// gcc counts in words. An integer or pointer of a word or less takes the next register. A `long
/// long` takes two when two are left and goes in the argument area when they are not, and the
/// register it could not use goes with it, so nothing after it gets one. A structure is the same
/// as its words: a twelve byte one fills all three registers and a thirteen byte one is on the
/// stack with every register left spent. The one structure that is not is a lone `float` or
/// `double`, which gcc gives the floating point mode of its member and so treats as that
/// floating point value, on the stack and spending nothing, as every `float` and `double` is here.
///
/// The address a structure comes back through is the first argument, so it is in eax, and the
/// callee leaves it where it was rather than taking it off the stack, since it is not there.
/// Everything about the value that comes back is [`I386_SYSV`]'s.
///
/// gcc gives a variadic function none of this, and `rucc_target::TargetInfo::convention_for`
/// makes the same choice, so these descriptions never see one.
pub static I386_SYSV_REGPARM: [AbiDescription; 3] = [
    regparm("i386 SysV regparm(1)", 1),
    regparm("i386 SysV regparm(2)", 2),
    regparm("i386 SysV regparm(3)", 3),
];

/// [`I386_SYSV`] under `-freg-struct-return`, which the Linux kernel builds every 32 bit unit
/// with.
///
/// Only the return value changes, and it changes to gcc's rule, which is the one mingw-w64 has by
/// default and [`I386_MINGW`] spells out: a structure of one, two, four or eight bytes comes back
/// in `al`, `ax`, `eax` or `edx:eax`, one whose only member is a `float` or a `double` comes back in
/// `st(0)`, and every other size still comes back through the hidden first argument. Those are the
/// sizes gcc gives an integer mode, and the rule is gcc's for anything with a mode of its own. A
/// one word structure is what `pte_t` and `pgd_t` are on a 32 bit kernel without PAE, so getting
/// this wrong breaks the calls between a unit built by rucc and one built by gcc everywhere a
/// page table is touched.
pub static I386_SYSV_REG_STRUCT: AbiDescription = AbiDescription {
    name: "i386 SysV -freg-struct-return",
    returns: &REG_STRUCT_RETURNS,
    ..I386_SYSV
};

/// [`I386_SYSV_REGPARM`] under `-freg-struct-return`, which is the kernel's pair of flags.
pub static I386_SYSV_REGPARM_REG_STRUCT: [AbiDescription; 3] = [
    AbiDescription {
        name: "i386 SysV regparm(1) -freg-struct-return",
        returns: &REG_STRUCT_RETURNS,
        ..regparm("", 1)
    },
    AbiDescription {
        name: "i386 SysV regparm(2) -freg-struct-return",
        returns: &REG_STRUCT_RETURNS,
        ..regparm("", 2)
    },
    AbiDescription {
        name: "i386 SysV regparm(3) -freg-struct-return",
        returns: &REG_STRUCT_RETURNS,
        ..regparm("", 3)
    },
];

/// Where a value comes back under `-freg-struct-return`. See [`I386_SYSV_REG_STRUCT`].
static REG_STRUCT_RETURNS: [Rule; 4] = [
    Rule::new(Test::Empty, Travel::Ignore),
    Rule::new(Test::LoneFloat, Travel::AsFound),
    Rule::new(Test::SizeOneOf(&[1, 2, 4, 8]), Travel::AsIntegers),
    Rule::new(Test::Anything, Travel::ByReference),
];

/// How a structure travels under [`I386_SYSV_REGPARM`].
static REGPARM_ARGUMENTS: [Rule; 3] = [
    Rule::new(Test::Empty, Travel::Ignore),
    Rule::new(Test::LoneFloat, Travel::InMemory),
    Rule::new(Test::Anything, Travel::AsIntegers).short(Short::MemoryAndDrain),
];

/// [`I386_SYSV_REGPARM`] with `registers` registers.
const fn regparm(name: &'static str, registers: u32) -> AbiDescription {
    AbiDescription {
        name,
        banks: Banks { integer: registers, ..I386_SYSV.banks },
        scalars: Scalars {
            wide_integer_is_all_or_nothing: true,
            wide_integer_drains: true,
            ..I386_SYSV.scalars
        },
        arguments: &REGPARM_ARGUMENTS,
        return_pointer: ReturnPointer::FirstArgument,
        ..I386_SYSV
    }
}

/// i386 System V `stdcall`, which gcc keeps outside Windows too: [`I386_SYSV`] with the callee
/// taking the argument area off the stack with `ret $n`.
///
/// The address a structure comes back through is the first word of that area and is counted in
/// `n` with the rest, so a function returning one ends in `ret $4` and more rather than in the
/// `ret $4` cdecl has. A unit built with `-mregparm=` gives a `stdcall` function its registers as
/// well, which is [`I386_SYSV_REGPARM_STDCALL`].
pub static I386_SYSV_STDCALL: AbiDescription = AbiDescription {
    name: "i386 SysV stdcall",
    return_pointer: ReturnPointer::FirstArgument,
    cleanup: Cleanup::Callee,
    ..I386_SYSV
};

/// [`I386_SYSV_REGPARM`] for a `stdcall` function: the same registers, and the callee takes what
/// is left in the argument area off the stack. When nothing is left that is a plain `ret`.
pub static I386_SYSV_REGPARM_STDCALL: [AbiDescription; 3] = [
    AbiDescription {
        name: "i386 SysV stdcall regparm(1)",
        cleanup: Cleanup::Callee,
        ..regparm("", 1)
    },
    AbiDescription {
        name: "i386 SysV stdcall regparm(2)",
        cleanup: Cleanup::Callee,
        ..regparm("", 2)
    },
    AbiDescription {
        name: "i386 SysV stdcall regparm(3)",
        cleanup: Cleanup::Callee,
        ..regparm("", 3)
    },
];

/// [`I386_SYSV_STDCALL`] under `-freg-struct-return`.
pub static I386_SYSV_REG_STRUCT_STDCALL: AbiDescription = AbiDescription {
    name: "i386 SysV stdcall -freg-struct-return",
    returns: &REG_STRUCT_RETURNS,
    ..I386_SYSV_STDCALL
};

/// [`I386_SYSV_REGPARM_STDCALL`] under `-freg-struct-return`.
pub static I386_SYSV_REGPARM_REG_STRUCT_STDCALL: [AbiDescription; 3] = [
    AbiDescription {
        name: "i386 SysV stdcall regparm(1) -freg-struct-return",
        returns: &REG_STRUCT_RETURNS,
        ..I386_SYSV_REGPARM_STDCALL[0]
    },
    AbiDescription {
        name: "i386 SysV stdcall regparm(2) -freg-struct-return",
        returns: &REG_STRUCT_RETURNS,
        ..I386_SYSV_REGPARM_STDCALL[1]
    },
    AbiDescription {
        name: "i386 SysV stdcall regparm(3) -freg-struct-return",
        returns: &REG_STRUCT_RETURNS,
        ..I386_SYSV_REGPARM_STDCALL[2]
    },
];

/// i386 System V `fastcall`, which is [`I386_MINGW_FASTCALL`]'s arguments with [`I386_SYSV`]'s
/// return value.
///
/// The first two integers or pointers of a word or less are in ecx and edx, a `double` is on the
/// stack and leaves the registers to the integers after it, and a `long long` or a structure is
/// on the stack and takes the registers with it, all as i686-linux-gnu-gcc 13 has it. A structure
/// comes back through an address in ecx whatever its size, which `-freg-struct-return` changes as
/// it does for cdecl, and `-mregparm=` changes nothing, since `fastcall` names its own registers.
pub static I386_SYSV_FASTCALL: AbiDescription = AbiDescription {
    name: "i386 SysV fastcall",
    returns: I386_SYSV.returns,
    ..I386_MINGW_FASTCALL
};

/// [`I386_SYSV_FASTCALL`] under `-freg-struct-return`.
pub static I386_SYSV_REG_STRUCT_FASTCALL: AbiDescription = AbiDescription {
    name: "i386 SysV fastcall -freg-struct-return",
    returns: &REG_STRUCT_RETURNS,
    ..I386_MINGW_FASTCALL
};

/// i686 Windows as mingw-w64 has it, with gcc or clang.
///
/// Arguments are what they are on i386 SysV: every one of them in the argument area, in source
/// order, rounded up to four bytes. The return value is where the two part. A structure of one,
/// two, four or eight bytes comes back in al, ax, eax or edx:eax, the way Windows x64 returns one
/// in rax, and every other size comes back through a hidden first argument. A structure whose
/// only member is a `float` or a `double` comes back in st(0) instead, which is gcc's rule and
/// the one thing that keeps this description apart from [`I386_MSVC`].
///
/// The hidden pointer itself is also different, and the difference is not in this table. On
/// Linux the callee pops it with `ret $4`, and here the caller does, so a function returning a
/// structure ends in a plain `ret` on Windows.
///
/// The widths are the ones [`I386_SYSV`] has, for the reason given there. The eight byte `long
/// long` and `double` alignment inside a structure is a layout fact, and `crate::layout` already
/// answers it for this target.
pub static I386_MINGW: AbiDescription = AbiDescription {
    name: "i386 Windows (mingw)",
    banks: Banks { integer: 0, float: 0, shared: false, integer_width: 4, float_width: 8 },
    scalars: Scalars {
        in_memory: None,
        wide_integer_is_all_or_nothing: false,
        wide_integer_starts_even: false,
        wide_integer_drains: false,
        wide_is_by_reference: false,
        wide_integer_returns_in: None,
        wide_integer_in_memory: false,
        decimal_in_integers: false,
        quad_returns_by_reference: true,
    },
    returns: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::LoneFloat, Travel::AsFound),
        Rule::new(Test::SizeOneOf(&[1, 2, 4, 8]), Travel::AsIntegers),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    arguments: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::Anything, Travel::InMemory),
    ],
    return_pointer: ReturnPointer::FirstArgument,
    variadic: Variadic::SameAsFixed,
    stack_args: StackArgs::RegisterSized,
    narrow: Narrow::Unspecified,
    bit_ints: BitInts::Untaught,
    cleanup: Cleanup::Caller,
};

/// i686 Windows as Microsoft's compiler has it, and clang for `i686-pc-windows-msvc`.
///
/// [`I386_MINGW`] with one rule fewer: a structure holding a lone `float` or `double` is not
/// special, so it comes back in eax or edx:eax by its size like any other structure of one, two,
/// four or eight bytes.
pub static I386_MSVC: AbiDescription = AbiDescription {
    name: "i386 Windows (MSVC)",
    returns: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::SizeOneOf(&[1, 2, 4, 8]), Travel::AsIntegers),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    ..I386_MINGW
};

/// i686 Windows `stdcall` as mingw-w64 has it, which is how nearly all of the Windows API is
/// declared: `WINAPI`, `CALLBACK` and `APIENTRY` are each this.
///
/// [`I386_MINGW`] in every way but one. The arguments are where cdecl puts them and the value
/// comes back where cdecl brings it back, and it is the callee that takes the argument area off
/// the stack, with `ret $n`. The address a structure comes back through is on the stack like any
/// other argument and is counted in `n` with them.
///
/// A variadic function cannot be one, since only its caller knows how many bytes it pushed, and
/// gcc makes a variadic `stdcall` function cdecl without a word. So does this compiler, in the
/// front end, which is why this description never sees one.
pub static I386_MINGW_STDCALL: AbiDescription =
    AbiDescription { name: "i386 Windows stdcall (mingw)", cleanup: Cleanup::Callee, ..I386_MINGW };

/// i686 Windows `fastcall` as mingw-w64 has it.
///
/// [`I386_MINGW_STDCALL`] with two registers. The first two arguments that are integers or
/// pointers of four bytes or fewer are in ecx and edx, and everything else is on the stack where
/// cdecl would put it. What keeps it from being a two register bank like any other is what
/// happens around the things that do not fit. A `double`, a `_Complex` or a structure holding one
/// `float` or `double` is on the stack and the integer after it still gets the next register. A
/// structure of four bytes or fewer is on the stack and takes a register anyway, so the integer
/// after it gets edx. A `long long` or a bigger structure is on the stack and takes the registers
/// that were left with it, so the integer after one of those is on the stack too. That is what
/// gcc does, measured with i686-w64-mingw32-gcc and i686-linux-gnu-gcc. The address a structure
/// comes back through is the first argument and so is in ecx, and it is not counted in the
/// `ret $n`.
pub static I386_MINGW_FASTCALL: AbiDescription = AbiDescription {
    name: "i386 Windows fastcall (mingw)",
    banks: Banks { integer: 2, ..I386_MINGW.banks },
    scalars: Scalars { wide_integer_in_memory: true, ..I386_MINGW.scalars },
    arguments: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::FloatingMode, Travel::InMemory),
        Rule::new(Test::Anything, Travel::InMemoryAndSpend),
    ],
    cleanup: Cleanup::Callee,
    ..I386_MINGW
};

/// i686 Windows `stdcall` as Microsoft's compiler has it, which is [`I386_MINGW_STDCALL`] with
/// the return rules of [`I386_MSVC`].
pub static I386_MSVC_STDCALL: AbiDescription = AbiDescription {
    name: "i386 Windows stdcall (MSVC)",
    returns: I386_MSVC.returns,
    ..I386_MINGW_STDCALL
};

/// i686 Windows `fastcall` as Microsoft's compiler has it, which is [`I386_MINGW_FASTCALL`] with
/// the return rules of [`I386_MSVC`].
pub static I386_MSVC_FASTCALL: AbiDescription = AbiDescription {
    name: "i386 Windows fastcall (MSVC)",
    returns: I386_MSVC.returns,
    ..I386_MINGW_FASTCALL
};

/// Which calling convention one function is defined and called with.
///
/// A target has one convention, and nearly every function in a program is defined and called with
/// it, which is what [`Convention::Target`] says and why it is the default. x86-64 is the one
/// machine with two conventions over one register file, and gcc lets a program pick the other one
/// for a single function with `__attribute__((ms_abi))` or `__attribute__((sysv_abi))`. UEFI is the
/// reason that exists: its firmware interfaces are Windows x64 whatever the loader is built on, so
/// a UEFI application built with a Linux toolchain calls every one of them through a pointer whose
/// type says `ms_abi`. Wine is the other user, going the other way.
///
/// The two named conventions are only ever the one the target does not already follow. The front
/// end writes `ms_abi` on a Windows target as [`Convention::Target`], which is what makes the
/// attribute change nothing there, not even which function type a declaration has: every header
/// mingw-w64 ships spells its own convention out on some declaration and not on the next.
///
/// 32-bit Windows is the other machine with more than one, and there it is the rule rather than
/// the exception: cdecl is the target's own, and `stdcall` and `fastcall` are what the Windows API
/// and a good deal of the code written against it declare. `cdecl` written out is the target's
/// own again, the same way `ms_abi` is on Windows x64.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Convention {
    /// Whatever the target follows, which is what a function without either attribute has.
    #[default]
    Target,
    /// Windows x64, on an x86-64 target that is not Windows.
    Ms,
    /// SysV AMD64, on an x86-64 target that is Windows.
    Sysv,
    /// `stdcall` on 32-bit x86: cdecl's arguments with the callee taking them off the stack.
    /// Outside Windows that is the unit's own convention with the callee popping, so under
    /// `-mregparm=` the first words are in registers here too.
    Stdcall,
    /// `fastcall` on 32-bit x86: `stdcall` with the first two small integers in ecx and edx.
    Fastcall,
    /// gcc's `regparm(n)` on 32-bit x86 outside Windows, with the first `n` words of arguments in
    /// eax, edx and ecx. `regparm(0)` is the plain convention, and is a convention of its own only
    /// in a unit `-mregparm=` gave another default to, which is where the kernel's `asmlinkage`
    /// uses it.
    Regparm(u8),
}

impl Convention {
    /// The attribute that asks for this one, which is what a diagnostic and a dump call it, and
    /// [`None`] for the target's own, which is asked for by writing nothing.
    #[must_use]
    pub const fn attribute(self) -> Option<&'static str> {
        match self {
            Self::Target => None,
            Self::Ms => Some("ms_abi"),
            Self::Sysv => Some("sysv_abi"),
            Self::Stdcall => Some("stdcall"),
            Self::Fastcall => Some("fastcall"),
            Self::Regparm(0) => Some("regparm(0)"),
            Self::Regparm(1) => Some("regparm(1)"),
            Self::Regparm(2) => Some("regparm(2)"),
            Self::Regparm(_) => Some("regparm(3)"),
        }
    }

    /// The convention an attribute asks for on this target, and [`None`] for a name that is not
    /// one of the two or a target that has neither.
    ///
    /// The one the target already follows comes back as [`Convention::Target`], which is the
    /// whole of the normalising the paragraph on the type promises. Only x86-64 has the pair, and
    /// gcc knows neither name anywhere else. `stdcall`, `fastcall` and `cdecl` are answered on
    /// 32-bit x86, Linux as well as Windows, since gcc keeps both conventions on both. `cdecl` is
    /// the target's own on both, and under `-mregparm=` it keeps the registers, as gcc's does.
    #[must_use]
    pub fn asked(target: TargetTuple, name: &str) -> Option<Self> {
        let windows = target.os() == Os::Windows;
        match (target.arch(), name) {
            (Arch::X86_64, "ms_abi") if windows => Some(Self::Target),
            (Arch::X86_64, "ms_abi") => Some(Self::Ms),
            (Arch::X86_64, "sysv_abi") if windows => Some(Self::Sysv),
            (Arch::X86_64, "sysv_abi") => Some(Self::Target),
            (Arch::X86, "stdcall") => Some(Self::Stdcall),
            (Arch::X86, "fastcall") => Some(Self::Fastcall),
            (Arch::X86, "cdecl") => Some(Self::Target),
            _ => None,
        }
    }

    /// Whether this is one of the 32-bit x86 conventions whose callee takes the arguments off the
    /// stack, which on Windows is also what decorates a function's name with how many bytes that
    /// is.
    #[must_use]
    pub const fn callee_pops(self) -> bool {
        matches!(self, Self::Stdcall | Self::Fastcall)
    }
}

/// The wasm32 Basic C ABI, which is what clang and wasi-libc follow on every wasm32 row.
///
/// The one ABI here with no shortage of registers. A wasm function has as many parameters as its
/// type says, so every scalar is a parameter of its own and nothing ever runs short. The banks
/// are counted anyway, and they are counted large, so the classifier asks the same questions it
/// asks everywhere else and always gets the answer that there is room.
///
/// An aggregate is the address of a copy the caller made, unless it is one scalar and nothing
/// else, which travels as that scalar. A GNU vector is each of its lanes, because clang passes it
/// as a vector value and LLVM splits that value into its lanes when `simd128` is off. That is the
/// whole of clang's `WebAssemblyABIInfo` for C.
/// The same goes for one past the `...`: the variadic area holds the address of the copy and not
/// the bytes, which is what clang's `va_arg` on wasm reads back. A return value that is not one
/// scalar comes back through a hidden first parameter, which is the address the caller wants it
/// written to.
///
/// A `long double` is IEEE quad here and travels as itself, which the backend splits into two
/// `i64` parameters. An integer narrower than an `int` is extended by the caller, which clang
/// says with `signext` and `zeroext` on the parameter and which the wasm backend relies on.
pub static WASM32_BASIC_C: AbiDescription = AbiDescription {
    name: "wasm32 Basic C",
    banks: Banks { integer: 1000, float: 1000, shared: false, integer_width: 8, float_width: 16 },
    scalars: Scalars {
        in_memory: None,
        wide_integer_is_all_or_nothing: false,
        wide_integer_starts_even: false,
        wide_integer_drains: false,
        wide_is_by_reference: false,
        wide_integer_returns_in: None,
        wide_integer_in_memory: false,
        decimal_in_integers: false,
        quad_returns_by_reference: false,
    },
    returns: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::SingleScalar, Travel::AsFound),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    arguments: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::SingleScalar, Travel::AsFound),
        Rule::new(Test::Vector, Travel::AsFound),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    return_pointer: ReturnPointer::FirstArgument,
    variadic: Variadic::SameAsFixed,
    stack_args: StackArgs::RegisterSized,
    narrow: Narrow::ToInt,
    bit_ints: BitInts::Untaught,
    cleanup: Cleanup::Caller,
};

/// The ABI a function of that convention follows on this target, which is [`for_target`] for the
/// target's own, the other description on x86-64 for the other one, and the `stdcall` or
/// `fastcall` description of the same runtime on 32-bit x86. Outside Windows that is the one for a
/// unit with no `-mregparm=` and no `-freg-struct-return`, and the others are reached through the
/// registers of the unit, which `rucc_target::TargetInfo::call_under` asks instead.
///
/// Only the passing half of the ABI changes. What a type is stays the target's, so a `long double`
/// is still the eighty bit x87 format in sixteen bytes in an `ms_abi` function on Linux, and the
/// Windows description answers for it the way it answers for any sixteen byte scalar, which is by
/// the address of a copy. gcc 13 passes it that way too.
#[must_use]
pub fn for_convention(
    target: TargetTuple,
    convention: Convention,
) -> Option<&'static AbiDescription> {
    match (convention, target.arch()) {
        (Convention::Target, _) => for_target(target),
        (Convention::Ms, Arch::X86_64) => Some(&WIN64),
        (Convention::Sysv, Arch::X86_64) => Some(&SYSV_AMD64),
        (Convention::Stdcall | Convention::Fastcall, Arch::X86) if target.os() == Os::Windows => {
            let msvc = target.env() == Env::Msvc;
            Some(match (convention, msvc) {
                (Convention::Stdcall, false) => &I386_MINGW_STDCALL,
                (Convention::Stdcall, true) => &I386_MSVC_STDCALL,
                (_, false) => &I386_MINGW_FASTCALL,
                (_, true) => &I386_MSVC_FASTCALL,
            })
        }
        (Convention::Stdcall, Arch::X86) => Some(&I386_SYSV_STDCALL),
        (Convention::Fastcall, Arch::X86) => Some(&I386_SYSV_FASTCALL),
        (Convention::Regparm(0), Arch::X86) if target.os() != Os::Windows => Some(&I386_SYSV),
        (Convention::Regparm(n @ 1..=3), Arch::X86) if target.os() != Os::Windows => {
            Some(&I386_SYSV_REGPARM[usize::from(n) - 1])
        }
        _ => None,
    }
}

/// Every ABI described here, which is what the report and the tests iterate.
pub static DESCRIBED: &[&AbiDescription] = &[
    &SYSV_AMD64,
    &AAPCS64,
    &DARWIN_ARM64,
    &WINDOWS_ARM64,
    &WIN64,
    &RISCV_LP64D,
    &I386_SYSV,
    &I386_SYSV_REGPARM[0],
    &I386_SYSV_REGPARM[1],
    &I386_SYSV_REGPARM[2],
    &I386_SYSV_REG_STRUCT,
    &I386_SYSV_REGPARM_REG_STRUCT[0],
    &I386_SYSV_REGPARM_REG_STRUCT[1],
    &I386_SYSV_REGPARM_REG_STRUCT[2],
    &I386_SYSV_STDCALL,
    &I386_SYSV_REGPARM_STDCALL[0],
    &I386_SYSV_REGPARM_STDCALL[1],
    &I386_SYSV_REGPARM_STDCALL[2],
    &I386_SYSV_REG_STRUCT_STDCALL,
    &I386_SYSV_REGPARM_REG_STRUCT_STDCALL[0],
    &I386_SYSV_REGPARM_REG_STRUCT_STDCALL[1],
    &I386_SYSV_REGPARM_REG_STRUCT_STDCALL[2],
    &I386_SYSV_FASTCALL,
    &I386_SYSV_REG_STRUCT_FASTCALL,
    &I386_MINGW,
    &I386_MSVC,
    &I386_MINGW_STDCALL,
    &I386_MINGW_FASTCALL,
    &I386_MSVC_STDCALL,
    &I386_MSVC_FASTCALL,
    &WASM32_BASIC_C,
];

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
        // Windows on 32-bit x86 is not i386 SysV: small structures come back in registers. This
        // is the cdecl half. stdcall, fastcall and thiscall each clean up differently and each
        // decorates the symbol name, which makes it the only place C has mangling, per
        // `spec/cross-compile/04-target-matrix.md`, and those are a convention a function asks
        // for rather than the target's.
        (Arch::X86, Os::Windows) if target.env() == Env::Msvc => &I386_MSVC,
        (Arch::X86, Os::Windows) => &I386_MINGW,
        (Arch::X86, _) => &I386_SYSV,
        (Arch::Aarch64, os) if os.is_darwin() => &DARWIN_ARM64,
        // AAPCS64 with a different variadic rule, per section 6.1, and not AAPCS64 itself, which
        // would be the almost right answer this file must not give.
        (Arch::Aarch64, Os::Windows) => &WINDOWS_ARM64,
        (Arch::Aarch64, _) => &AAPCS64,
        (Arch::Riscv64, _) => &RISCV_LP64D,
        (Arch::Wasm32, _) => &WASM32_BASIC_C,
        _ => return None,
    })
}
