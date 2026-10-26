//! The RISC-V register file, and where the LP64D convention over it puts things.
//!
//! Design: `spec/10-backend.md` section 10.8 and `spec/cross-compile/06-abis.md` section 6.1, and
//! issue #3284, which is the backend this is the first piece of.
//!
//! Registers are numbered the way the instruction encoding numbers them, which is also the way
//! DWARF numbers the integer ones, so `x0` is zero and `x31` is thirty one. The names are the ABI
//! names GNU as and gcc write, `a0` rather than `x10`, and both spellings are taken in an `asm`
//! statement. `x0` reads as zero and throws away what is written to it, so it is a register only
//! to the encoder, and nothing allocates it, the stack pointer, the global pointer or the thread
//! pointer.
//!
//! The floating point registers are eight bytes, which is the D extension and what LP64D assumes.
//! A `long double` is sixteen bytes in the quad format and lives in a pair of integer registers,
//! so nothing wider than a `double` is ever in this file.
//!
//! # What is not here yet
//!
//! Everything a pass reads besides the registers and the encoder: the instructions, the frame
//! and the branches. Those arrive with the rule set that selects them, and until then
//! `rucc_codegen::Machine::for_target` has no machine for this target and says so. The encoder
//! in [`encode`] takes RV64G as GNU as writes it, pseudo instructions included, so the words
//! can be checked against the cross assembler before anything selects them.

mod encode;

pub use crate::riscv64::encode::{
    Addr, Encoded, Error, Fixup, Offset, Operator, Round, Value, encode, fits, li_words, reaches,
};

use crate::regs::{CallRegs, ClassInfo, Guard, PhysReg, RegClass, RegFile};

/// The integer registers.
pub const GPR: RegClass = RegClass::new(0);
/// The floating point registers.
pub const FPR: RegClass = RegClass::new(1);

/// The integer register with that number.
///
/// # Panics
///
/// Panics at thirty two or more.
#[must_use]
pub const fn x(number: u8) -> PhysReg {
    assert!(number < 32, "RISC-V has thirty two integer registers");
    PhysReg::new(number)
}

/// The floating point register with that number.
///
/// # Panics
///
/// Panics at thirty two or more.
#[must_use]
pub const fn f(number: u8) -> PhysReg {
    assert!(number < 32, "RISC-V has thirty two floating point registers");
    PhysReg::new(number)
}

/// The register that reads as zero.
pub const ZERO: PhysReg = x(0);
/// The return address, where a call leaves the address it came from.
pub const RA: PhysReg = x(1);
/// The stack pointer.
pub const SP: PhysReg = x(2);
/// The global pointer, which the linker may relax an access to a small datum through.
pub const GP: PhysReg = x(3);
/// The thread pointer, which a Linux kernel keeps the running task in.
pub const TP: PhysReg = x(4);
/// The register gcc passes a nested function its static chain in.
pub const T2: PhysReg = x(7);
/// The frame pointer, `s0`.
pub const FP: PhysReg = x(8);
/// The first argument and the first half of a result.
pub const A0: PhysReg = x(10);
/// The first of the two registers held back for whatever needs one after allocation.
pub const T5: PhysReg = x(30);
/// The second of them.
pub const T6: PhysReg = x(31);

/// The register a name in an `asm` statement means, and the file it is in, or `None` for a name
/// that is not one a program may hand the allocator.
///
/// Both the ABI name and the `x` or `f` name are taken, and `fp` is `s0`, the name GNU as gives
/// it. `tp` and `gp` are here although nothing allocates them, because a kernel reads the task it
/// is running from `tp` through a local register variable and a clobber of either is a promise the
/// allocator can keep by doing nothing. `zero` and `sp` are not, since neither is somewhere a value
/// can be put or taken away.
#[must_use]
pub fn named(name: &str) -> Option<(PhysReg, RegClass)> {
    if name == "fp" {
        return Some((FP, GPR));
    }
    if let Some(at) = GPR_NAMES.iter().position(|&it| it == name) {
        let reg = x(u8::try_from(at).ok()?);
        return (reg != ZERO && reg != SP).then_some((reg, GPR));
    }
    if let Some(at) = FPR_NAMES.iter().position(|&it| it == name) {
        return Some((f(u8::try_from(at).ok()?), FPR));
    }
    let (file, number) = name.split_at_checked(1)?;
    if number.len() > 1 && number.starts_with('0') {
        return None;
    }
    let number: u8 = number.parse().ok()?;
    match file {
        "x" if number < 32 && number != 0 && number != 2 => Some((x(number), GPR)),
        "f" if number < 32 => Some((f(number), FPR)),
        _ => None,
    }
}

static GPR_NAMES: [&str; 32] = [
    "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4",
    "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4",
    "t5", "t6",
];

static FPR_NAMES: [&str; 32] = [
    "ft0", "ft1", "ft2", "ft3", "ft4", "ft5", "ft6", "ft7", "fs0", "fs1", "fa0", "fa1", "fa2",
    "fa3", "fa4", "fa5", "fa6", "fa7", "fs2", "fs3", "fs4", "fs5", "fs6", "fs7", "fs8", "fs9",
    "fs10", "fs11", "ft8", "ft9", "ft10", "ft11",
];

static CLASSES: [ClassInfo; 2] = [
    ClassInfo { name: "gpr", bits: 64, regs: &GPR_NAMES, allocatable: true },
    ClassInfo { name: "fpr", bits: 64, regs: &FPR_NAMES, allocatable: true },
];

/// Every register RV64GC has, as far as this compiler is concerned.
pub static REGS: RegFile = RegFile::new(&CLASSES);

/// The integer registers by DWARF's number for them, which is the machine's.
static GPR_DWARF: [u16; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

/// The floating point registers by DWARF's number, which start at thirty two.
static FPR_DWARF: [u16; 32] = [
    32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55,
    56, 57, 58, 59, 60, 61, 62, 63,
];

static RISCV64_DWARF: [&[u16]; 2] = [&GPR_DWARF, &FPR_DWARF];

/// The column an unwind table files the return address under, which is `ra`'s.
///
/// A call leaves the address it came from in `ra` rather than on the stack, so the CIE names that
/// register's column, as gcc's and LLVM's do.
pub const DWARF_RETURN_ADDRESS: u16 = 1;

static INT_ARGS: [PhysReg; 8] = [x(10), x(11), x(12), x(13), x(14), x(15), x(16), x(17)];
static FP_ARGS: [PhysReg; 8] = [f(10), f(11), f(12), f(13), f(14), f(15), f(16), f(17)];
// Two of each, for a sixteen byte integer or a small structure and for a pair of floating point
// members, and `struct { double d; long l; }` comes back in one of each.
static INT_RETURNS: [PhysReg; 2] = [x(10), x(11)];
static FP_RETURNS: [PhysReg; 2] = [f(10), f(11)];
static NO_X87: [PhysReg; 0] = [];
// `s0` is not here, although a callee gives it back, because it is the frame pointer and the
// prologue saves it with `ra` whether or not the function writes to it, as `rbp` on x86-64.
static INT_SAVED: [PhysReg; 11] =
    [x(9), x(18), x(19), x(20), x(21), x(22), x(23), x(24), x(25), x(26), x(27)];
static FP_SAVED: [PhysReg; 12] =
    [f(8), f(9), f(18), f(19), f(20), f(21), f(22), f(23), f(24), f(25), f(26), f(27)];

// The ones a call destroys first, so a value that does not live across one costs no save, then the
// ones it preserves. Left out: the five with a job of their own, `zero`, `ra`, `sp`, `gp` and
// `tp`; the frame pointer; `t2`, which carries the static chain into a nested function and is kept
// out for the reason AArch64 keeps `x18` out; and `t5` and `t6`, which are held back as the scratch
// registers the passes after allocation write through, since nothing on this machine can add a
// large constant to the stack pointer or reach a far slot without one.
static INT_ORDER: [PhysReg; 23] = [
    x(10),
    x(11),
    x(12),
    x(13),
    x(14),
    x(15),
    x(16),
    x(17),
    x(5),
    x(6),
    x(28),
    x(29),
    x(9),
    x(18),
    x(19),
    x(20),
    x(21),
    x(22),
    x(23),
    x(24),
    x(25),
    x(26),
    x(27),
];
// The same idea: the eight argument registers, the twelve temporaries and the preserved ones last.
static FP_ORDER: [PhysReg; 32] = [
    f(10),
    f(11),
    f(12),
    f(13),
    f(14),
    f(15),
    f(16),
    f(17),
    f(0),
    f(1),
    f(2),
    f(3),
    f(4),
    f(5),
    f(6),
    f(7),
    f(28),
    f(29),
    f(30),
    f(31),
    f(8),
    f(9),
    f(18),
    f(19),
    f(20),
    f(21),
    f(22),
    f(23),
    f(24),
    f(25),
    f(26),
    f(27),
];

/// Where an LP64D call puts things, on Linux and on a bare machine alike.
///
/// The address of a result returned through memory goes in `a0`, as the first argument, and the
/// arguments after it move along by one, so there is no register of its own for it. A variadic
/// argument travels as a fixed one would, which is in the description rather than here, and the
/// list is a pointer into the save area a variadic callee writes `a0` to `a7` into just below
/// whatever the caller left on the stack.
///
/// The stack protector's word is the plain global `__stack_chk_guard`, which is where gcc reads it
/// on this machine and where glibc and musl define it.
pub static LP64D: CallRegs = CallRegs {
    abi: &rucc_abi::abis::RISCV_LP64D,
    int_class: GPR,
    sse_class: FPR,
    int_args: &INT_ARGS,
    sse_args: &FP_ARGS,
    shared_positions: false,
    int_returns: &INT_RETURNS,
    sse_returns: &FP_RETURNS,
    x87_returns: &NO_X87,
    int_saved: &INT_SAVED,
    sse_saved: &FP_SAVED,
    // The whole of each register is kept, since the whole of it is eight bytes.
    sse_kept: None,
    int_order: &INT_ORDER,
    sse_order: &FP_ORDER,
    stack_pointer: SP,
    frame_pointer: FP,
    // gcc points `s0` at the top of the frame, the caller's stack pointer, after `ra` and `s0` are
    // stored just below it, which is the order that keeps the chain a backtrace walks.
    late_frame_pointer: false,
    unwind_codes: false,
    // Nothing says how many floating point registers a variadic call used, and nothing needs to,
    // since a variadic argument travels in the integer registers.
    vector_count: None,
    red_zone: 0,
    shadow: 0,
    home: 0,
    stack_align: 16,
    trusted_align: 16,
    // A call leaves the return address in `ra`, so it pushes nothing.
    return_address: 0,
    word: 8,
    // RVWMO is weakly ordered, so an acquire and a release each need a fence or an AMO.
    total_store_order: false,
    // The base ISA lets an implementation trap a misaligned access, and Linux then emulates it a
    // thousand times slower, so a copy keeps to the alignment it was given.
    unaligned: false,
    // A byte reversal is one instruction only with Zbb, which RV64GC does not have.
    byte_swaps: &[],
    // There is no push. The frame code stores pairs into a frame it took all at once, and sixteen
    // keeps the stack pointer aligned through it.
    push: 16,
    link: Some(RA),
    sret: None,
    chain: Some(T2),
    list: crate::VaList::VoidPointer,
    dwarf: &RISCV64_DWARF,
    dwarf_return_address: DWARF_RETURN_ADDRESS,
    guard: Some(Guard::global("__stack_chk_guard", true)),
    // gcc's hook here is `_mcount` with the return address in `ra`, and a kernel's is the
    // patchable entry instead, so it waits for the frame code rather than being described now.
    trace: None,
    // Linux grows a stack by faulting.
    chkstk: None,
    conventions: crate::Conventions::ONLY,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_has_one_name_per_register() {
        assert_eq!(REGS.duplicate(), None);
        assert_eq!(REGS.len(GPR), 32);
        assert_eq!(REGS.len(FPR), 32);
        assert_eq!(REGS.name(GPR, SP), Some("sp"));
        assert_eq!(REGS.name(GPR, A0), Some("a0"));
        assert_eq!(REGS.name(GPR, T6), Some("t6"));
        assert_eq!(REGS.reg_named("fa0"), Some((FPR, f(10))));
        assert_eq!(REGS.reg_named("fs11"), Some((FPR, f(27))));
    }

    #[test]
    fn dwarf_numbers_are_the_machine_numbers_and_the_floats_start_at_thirty_two() {
        assert_eq!(LP64D.dwarf(GPR, ZERO), Some(0));
        assert_eq!(LP64D.dwarf(GPR, SP), Some(2));
        assert_eq!(LP64D.dwarf(GPR, T6), Some(31));
        assert_eq!(LP64D.dwarf(FPR, f(8)), Some(40));
        assert_eq!(LP64D.machine(FPR, 63), Some(f(31)));
        assert_eq!(LP64D.dwarf_return_address, 1);
    }

    #[test]
    fn nothing_with_a_job_is_handed_out() {
        for reserved in [ZERO, RA, SP, GP, TP, T2, FP, T5, T6] {
            assert!(!LP64D.int_order.contains(&reserved), "{reserved:?}");
        }
        // Every register the allocator may use is either one a call destroys or one the frame
        // saves, and every one the frame saves is one it may use.
        for reg in LP64D.int_saved {
            assert!(LP64D.int_order.contains(reg), "{reg:?}");
        }
        for reg in LP64D.sse_saved {
            assert!(LP64D.sse_order.contains(reg), "{reg:?}");
        }
        for reg in LP64D.int_args.iter().chain(LP64D.int_returns) {
            assert!(LP64D.int_order.contains(reg), "{reg:?}");
        }
        assert_eq!(LP64D.int_order.len(), 23);
        assert_eq!(LP64D.sse_order.len(), 32);
    }

    #[test]
    fn the_convention_is_lp64d_with_the_result_address_as_the_first_argument() {
        assert!(std::ptr::eq(LP64D.abi, &rucc_abi::abis::RISCV_LP64D));
        assert_eq!(LP64D.sret, None);
        assert_eq!(LP64D.int_args.first(), Some(&A0));
        assert_eq!(LP64D.link, Some(RA));
        assert_eq!((LP64D.red_zone, LP64D.return_address, LP64D.stack_align), (0, 0, 16));
        assert_eq!(LP64D.list, crate::VaList::VoidPointer);
    }

    #[test]
    fn a_register_named_in_an_asm_statement_is_taken_by_either_name() {
        assert_eq!(named("a0"), Some((A0, GPR)));
        assert_eq!(named("x10"), Some((A0, GPR)));
        assert_eq!(named("fp"), Some((FP, GPR)));
        assert_eq!(named("s0"), Some((FP, GPR)));
        assert_eq!(named("tp"), Some((TP, GPR)));
        assert_eq!(named("ra"), Some((RA, GPR)));
        assert_eq!(named("fa1"), Some((f(11), FPR)));
        assert_eq!(named("f31"), Some((f(31), FPR)));
        for wrong in ["zero", "x0", "sp", "x2", "x32", "f32", "x07", "a8", "v0", ""] {
            assert_eq!(named(wrong), None, "{wrong}");
        }
    }
}
