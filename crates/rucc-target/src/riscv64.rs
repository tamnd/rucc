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
//! The rule set that selects these instructions. Until it arrives
//! `rucc_codegen::Machine::for_target` has no machine for this target and says so. The opcodes,
//! their text, the encoder, the printer, the reader and the tables the passes read are all
//! here, so each can be checked against the cross assembler before anything selects them.
//!
//! # No condition state
//!
//! This machine has no flags. A branch compares two registers itself, and a comparison that
//! keeps its answer writes it to a register. The passes that place branches are written for a
//! test and a jump that reads what the test left, so the test and the two jumps on it are named
//! here, and the layout always folds the pair into `beqz` or `bnez`. Their spellings are ones
//! the encoder refuses, so a pair that was not folded is an error rather than wrong code.

mod encode;
mod insts;
mod read;
mod text;
mod timing;
mod write;

pub use crate::riscv64::encode::{
    Addr, Encoded, Error, FAR, Fixup, Offset, Operator, Round, Value, compressed, encode, fits,
    li_words, reaches,
};
pub use crate::riscv64::insts::{ADDRESSES, Form, INSTS, TEMPLATE, address, form};
pub use crate::riscv64::read::{Error as ReadError, Line, read, register};
pub use crate::riscv64::text::{Arg, Missing, Operands, Written, fill, operand_width, written};
pub use crate::riscv64::timing::{LOAD, MODEL, TIMING};
pub use crate::riscv64::write::write;

use crate::bits::BitInsts;
use crate::branch::{BranchInsts, Zero};
use crate::counts::CountInst;
use crate::flags::FlagInsts;
use crate::frame::{Canary, ClassMoves, FrameInsts, Pair, Probe};
use crate::machine::MachineInsts;
use crate::operand::OperandDesc;
use crate::regs::{CallRegs, ClassInfo, Guard, PhysReg, RegClass, RegFile};
use crate::short::ShortInsts;

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

/// Whether one of gcc's RISC-V immediate letters in an `asm` constraint takes that constant. `I`
/// is the twelve bit signed number an `addi` carries, `J` is zero, and `K` is the five bit unsigned
/// number a `csrrsi` carries, which is what the kernel's `csr_set` hands `"rK"`. A constant a letter
/// does not take goes in a register. Any other letter takes any constant.
#[must_use]
pub fn takes(letter: char, number: i128) -> bool {
    match letter {
        'I' => (-2048..=2047).contains(&number),
        'J' => number == 0,
        'K' => (0..=31).contains(&number),
        _ => true,
    }
}

/// The integer registers by number, as their ABI names, which is how GNU as and objdump write them.
pub static GPR_NAMES: [&str; 32] = [
    "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4",
    "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4",
    "t5", "t6",
];

/// The floating point registers the same way.
pub static FPR_NAMES: [&str; 32] = [
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
    // A variadic function stores a0 to a7 just below what the caller left in memory, so that a
    // `void *` walks the registers and the memory as one run of words.
    home: 64,
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

static RISCV64_MOVES: [ClassMoves; 2] = [
    ClassMoves { mov: "mv_rr_64", load: "ld_64", store: "st_64", narrow: &[] },
    ClassMoves { mov: "fmv_f64", load: "ld_f64", store: "st_f64", narrow: &[] },
];

/// What a RISC-V prologue, epilogue, spill and reload are made of.
///
/// The arrangement of `rucc_target::aarch64::FRAME`. A push and a pop move the stack pointer by
/// sixteen, which is the alignment the psABI keeps it at, and the frame pointer and the return
/// address go on together as one pair. A displacement that does not fit in twelve bits goes
/// through the far pseudo instructions, which build it in `t5`, a register held back from the
/// allocator for that. There is no step to break a large adjustment into, so `step_bits` is
/// `None` and the frame hands the whole displacement to `lea_far_64`.
pub static FRAME: FrameInsts = FrameInsts {
    prefix: "rv.",
    classes: &RISCV64_MOVES,
    push: "push_64",
    pop: "pop_64",
    pair: Some(Pair { push: "push_pair_64", pop: "pop_pair_64" }),
    kept: None,
    add: "add_far_64",
    sub: "sub_far_64",
    grow: "sub_rr_64",
    scaled: None,
    insert: None,
    align: "align_sp_64",
    imm: "li_64",
    lea: "lea_far_64",
    sum: "add_rr_64",
    ret: "ret",
    ret_pop: None,
    iret: None,
    clear_direction: None,
    differ: "cmp_set_ne_64",
    differs_from: None,
    not_below: "cmp_set_hs_64",
    away: Some("tail"),
    call: "call",
    probe: Some(PROBE),
    landing: None,
    signing: None,
    canary: Some(Canary { near: "addr_64", far: "got_64", system: "thread_64" }),
    targets: None,
    pad: Some("nop"),
    step_bits: None,
    reaches: Some(frame_reaches),
    thunks: None,
    through: None,
};

/// Whether the instruction of that name can carry that displacement from the stack pointer.
///
/// The encoder's answer for the instruction written with every register numbered zero and the
/// stack pointer as its base. Only a refused number is a no, as on AArch64.
fn frame_reaches(name: &str, disp: i32) -> bool {
    let Some(insts) = written(name) else { return true };
    let mem = Addr { base: 2, offset: Offset::Imm(i64::from(disp)) };
    let with = Operands { regs: &[0; 8], reads: 0, imm: 0, mem: Some(mem) };
    insts.iter().all(|inst| {
        let Ok(values) =
            inst.args.iter().map(|&arg| fill(arg, &with)).collect::<Result<Vec<_>, _>>()
        else {
            return true;
        };
        !matches!(encode(inst.mnemonic, &values), Err(Error::Immediate { .. }))
    })
}

/// How a RISC-V prologue touches a page of the stack it has just reached, which is a store of
/// the zero register, as gcc writes it.
pub static PROBE: Probe = Probe { inst: "probe_64", interval: 4096 };

/// How much of a register each RISC-V instruction reads and writes.
///
/// Every integer register has one name at every width, so the widths are in the text as numbers
/// rather than as names, and they are already the width of the value: the source of a widening of
/// a byte is eight bits there.
pub static BITS: BitInsts = BitInsts { prefix: "rv.", width: operand_width, copies_low };

/// Whether the instruction of that name is one that copies the low bits of its source.
#[must_use]
fn copies_low(name: &str) -> bool {
    form(name) == Some(Form::Convert)
}

/// What a RISC-V instruction has to look like for this machine to have one.
///
/// Every answer is [`INSTS`] read back. An address is a base and a displacement and nothing else.
pub static MACHINE: MachineInsts = MachineInsts {
    prefix: "rv.",
    operands: machine_operands,
    takes_imm: machine_takes_imm,
    takes_mem: machine_takes_mem,
    touches_mem: machine_touches_mem,
    calls: machine_calls,
    commutes: machine_commutes,
    scales: &[],
    index_and_disp: false,
};

/// The operands an instruction of that name has, or `None` if this machine has no such name.
#[must_use]
fn machine_operands(name: &str) -> Option<&'static [OperandDesc]> {
    form(name).map(Form::operands)
}

/// Whether an instruction of that name carries an immediate.
#[must_use]
fn machine_takes_imm(name: &str) -> bool {
    form(name).is_some_and(Form::takes_imm)
}

/// Whether an instruction of that name carries an addressing mode.
#[must_use]
fn machine_takes_mem(name: &str) -> bool {
    form(name).is_some_and(Form::takes_mem)
}

/// Whether an instruction of that name reads or writes memory.
#[must_use]
fn machine_touches_mem(name: &str) -> bool {
    form(name).is_some_and(Form::touches_mem)
}

/// Whether an instruction of that name is a call, which is `call` and `jalr`.
#[must_use]
fn machine_calls(name: &str) -> bool {
    form(name) == Some(Form::Call)
}

/// Whether an instruction of that name reads its two sources either way round. Nothing here
/// writes its answer over a source, so there is no choice for the allocator to make.
#[must_use]
fn machine_commutes(_name: &str) -> bool {
    false
}

/// What a RISC-V conditional branch becomes once the blocks are in an order.
///
/// The test and the two jumps on it are always folded into `beqz` or `bnez`, which is what
/// `FOLDS` says. Those reach four kilobytes either side, and a function longer than `NEAR`
/// opcodes has its far branches written around a `j` instead.
pub static BRANCH: BranchInsts = BranchInsts {
    prefix: "rv.",
    cond: "br_cond_32",
    test: "test",
    if_true: "b_ne",
    if_false: "b_eq",
    jump: "j",
    indirect: "jr",
    goto: "template",
    conditional: &["b_eq", "b_ne"],
    fused: &[],
    readings: &[],
    moves: &[],
    zero: &FOLDS,
    bits: &[],
    near: NEAR,
};

/// The two folds of a test and a jump into one branch on a register.
static FOLDS: [Zero; 2] = [
    Zero { test: "test", when: "b_eq", into: "beqz" },
    Zero { test: "test", when: "b_ne", into: "bnez" },
];

/// The most opcodes a function can have for `beqz` to reach across all of it.
///
/// Four kilobytes either side is a thousand instructions, and counting each opcode as two leaves
/// room for the ones that are several.
const NEAR: usize = 512;

/// What each RISC-V instruction leaves in the condition state, which is only the test.
///
/// A call is on the list as well, and so is every name this target does not have, so that
/// nothing is moved between the test and the jump that reads it.
pub static FLAGS: FlagInsts = FlagInsts {
    prefix: "rv.",
    width: operand_width,
    writes: writes_flags,
    compares: &[],
    readers: &[],
    compares_itself,
    zeroing: &[],
};

/// Whether the instruction of that name compares and keeps the answer in one opcode.
fn compares_itself(name: &str) -> bool {
    matches!(form(name), Some(Form::CmpSet | Form::FCmpSet))
}

/// Whether the instruction of that name leaves the condition state other than it found it.
#[must_use]
fn writes_flags(name: &str) -> bool {
    form(name).is_none_or(|shape| matches!(shape, Form::Test | Form::Call))
}

/// The shorter spellings RISC-V has for the same answer, which is none, since nothing here writes
/// the compressed forms.
pub static SHORT: ShortInsts = ShortInsts {
    prefix: "rv.",
    zeroing: &[],
    narrowing: &[],
    testing: &[],
    stepping: &[],
    copying: &[],
    spreading: &[],
    unindexed: &[],
};

/// The bit counts this machine has an instruction for, which is none without the Zbb extension.
pub const COUNTS: &[CountInst] = &[];

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

    /// Every name the descriptions hand out is one this machine has, since a pass writes what it
    /// is given and the encoder is the first thing to find out that it is nothing.
    #[test]
    fn every_name_a_description_hands_out_is_an_opcode() {
        let mut names = vec![FRAME.push, FRAME.pop, FRAME.add, FRAME.sub, FRAME.grow, FRAME.align];
        names.extend([FRAME.imm, FRAME.lea, FRAME.sum, FRAME.ret, FRAME.differ, FRAME.not_below]);
        names.extend([FRAME.call, PROBE.inst, FRAME.pad.expect("a pad")]);
        names.extend(FRAME.away);
        for moves in FRAME.classes {
            names.extend([moves.mov, moves.load, moves.store]);
        }
        if let Some(pair) = FRAME.pair {
            names.extend([pair.push, pair.pop]);
        }
        if let Some(canary) = FRAME.canary {
            names.extend([canary.near, canary.far, canary.system]);
        }
        names.extend([BRANCH.cond, BRANCH.test, BRANCH.if_true, BRANCH.if_false, BRANCH.jump]);
        names.extend([BRANCH.indirect, BRANCH.goto]);
        names.extend(BRANCH.conditional);
        for entry in BRANCH.zero {
            names.extend([entry.test, entry.when, entry.into]);
        }
        for name in names {
            assert!(form(name).is_some(), "{name} is not an opcode");
        }
        assert_eq!(FRAME.classes.len(), 2, "both files are spilled");
    }

    /// There is no step to break a frame into, so the whole of it goes to the far pseudo
    /// instructions, which take any size.
    #[test]
    fn a_frame_is_taken_in_one_step_of_any_size() {
        assert_eq!(FRAME.steps(16), [16]);
        assert_eq!(FRAME.steps(70016), [70016]);
        for bytes in [16, 2032, 4096, 70016, 0x0100_0010] {
            let imm = i64::from(bytes);
            assert!(encode("subi.far", &[Value::Gpr(2), Value::Gpr(2), Value::Imm(imm)]).is_ok());
            assert!(encode("addi.far", &[Value::Gpr(2), Value::Gpr(2), Value::Imm(imm)]).is_ok());
        }
    }

    #[test]
    fn how_far_from_the_stack_pointer_an_instruction_reaches_is_the_encoders_answer() {
        let reaches = FRAME.reaches.unwrap();
        assert!(reaches("ld_64", 2047));
        assert!(!reaches("ld_64", 2048));
        assert!(reaches("st_f64", -2048));
        assert!(!reaches("st_f64", -2056));
        assert!(reaches("lea_far_64", 1 << 20));
        assert!(!reaches("lea_64", 4096));
        assert!(reaches("ret", 1 << 20));
    }

    #[test]
    fn nothing_but_the_test_and_a_call_leaves_a_condition_behind() {
        assert!(writes_flags("test"));
        assert!(writes_flags("call"));
        assert!(writes_flags("nothing"));
        assert!(!writes_flags("cmp_set_lt_64"));
        assert!(!writes_flags("add_rr_64"));
        assert!(compares_itself("cmp_set_lt_64"));
        assert!(compares_itself("fcmp_set_lt_f64"));
        assert!(!compares_itself("test"));
    }

    #[test]
    fn the_widths_of_a_widening_are_the_widths_of_its_value() {
        assert_eq!((BITS.width)("sextb_32", 1), Some(8));
        assert_eq!((BITS.width)("zexth_64", 1), Some(16));
        assert!((BITS.copies_low)("zextw_64"));
        assert!(!(BITS.copies_low)("add_rr_64"));
    }
}
