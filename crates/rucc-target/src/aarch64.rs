//! The AArch64 register file, and where the two conventions over it put things.
//!
//! Design: `spec/10-backend.md` section 10.8, `spec/12-abi-and-runtime.md` section 12.3 and
//! `spec/cross-compile/06-abis.md` sections 6.2 and 6.3.
//!
//! Registers are numbered the way the instruction encoding numbers them, which on this machine is
//! also the way DWARF numbers them, so `x0` is zero and `x30` is thirty. Number thirty one is the
//! one place that needs a sentence. The encoding uses it for two different registers, the stack
//! pointer in an operand that can be one and the zero register in an operand that cannot, and
//! which it is belongs to the instruction rather than to the number. Here it is `sp`, because that
//! is the one a frame names and a calling convention has to say, and the zero register is a
//! spelling the encoder picks when an instruction reads nothing or throws its result away. Nothing
//! allocates either of them.
//!
//! The names are the sixty four bit ones for the reason [`crate::x86_64`] gives for its own: `w0`
//! is the low half of `x0` rather than a second register, and the width is the instruction's. The
//! vector registers are `v0` to `v31` for the same reason, and `d0`, `s0` and `q0` are what an
//! instruction that reads eight, four or sixteen bytes of one writes.
//!
//! # Instructions
//!
//! [`encode`] writes one instruction as its word, [`read`] reads one written the way GNU as takes
//! it, and [`write()`] writes one the way GNU as takes it, which together are enough to check every
//! word against the ones GNU as writes and every line against what it reads back as.
//!
//! [`INSTS`] is what each opcode of the machine IR does with its operands, which is what the
//! allocator reads, and [`written`] is what it is written as. [`fill`] turns an argument of one of
//! those into the value the encoder and the writer both take, so a listing and the object beside it
//! are written from the same values.
//!
//! # What is not here yet
//!
//! The lowering that selects these opcodes, which is what makes the descriptions below more than
//! tables. [`FRAME`], [`BRANCH`], [`BITS`], [`FLAGS`], [`MACHINE`], [`TIMING`] and [`SHORT`] are
//! what a pipeline pass reads this machine through, and they are all here, but no function reaches
//! them until a rule set does. The flags register, for the reason x86-64 leaves it out: a
//! comparison and whatever reads it are one rule. And the scalable vector and predicate registers,
//! which arrive with the target features that have them.

mod encode;
mod insts;
mod read;
mod text;
mod timing;
mod write;

pub use crate::aarch64::encode::{
    Addr, Arrangement, Cond, Encoded, Error, Extend, Fixup, Mode, Offset, Operator, Scalar, Shift,
    Value, Width, encode,
};
pub use crate::aarch64::insts::{ADDRESSES, Form, INSTS, address, form};
pub use crate::aarch64::read::{Error as ReadError, Line, read};
pub use crate::aarch64::text::{Arg, Missing, Operands, Written, fill, operand_width, written};
pub use crate::aarch64::timing::{LOAD, MODEL, TIMING};
pub use crate::aarch64::write::{cond_name, write};

use crate::bits::BitInsts;
use crate::branch::{BranchInsts, Fusion, Move};
use crate::flags::{Compare, FlagInsts, Reader, Reads};
use crate::frame::{ClassMoves, FrameInsts, Pair, Probe};
use crate::machine::MachineInsts;
use crate::operand::OperandDesc;
use crate::regs::{CallRegs, ClassInfo, PhysReg, RegClass, RegFile};
use crate::short::ShortInsts;

/// The general purpose registers.
pub const GPR: RegClass = RegClass::new(0);
/// The floating point and vector registers.
pub const FPR: RegClass = RegClass::new(1);

/// The general purpose register with that number.
///
/// # Panics
///
/// Panics at thirty one or more. Thirty one is the stack pointer and has a name of its own,
/// [`SP`], because an instruction that writes `x31` means the zero register and a frame that means
/// the stack pointer has to say so.
#[must_use]
pub const fn x(number: u8) -> PhysReg {
    assert!(number < 31, "x31 is not a general purpose register, it is sp or xzr");
    PhysReg::new(number)
}

/// The vector register with that number.
///
/// # Panics
///
/// Panics at thirty two or more.
#[must_use]
pub const fn v(number: u8) -> PhysReg {
    assert!(number < 32, "AArch64 has thirty two vector registers");
    PhysReg::new(number)
}

/// Where a call returns a structure too large for registers, which the callee writes through.
pub const X8: PhysReg = x(8);
/// The first of the two registers a linker's veneer and a PLT entry may destroy.
pub const X16: PhysReg = x(16);
/// The second of them.
pub const X17: PhysReg = x(17);
/// The platform register, which Apple and Microsoft reserve and Linux does not.
pub const X18: PhysReg = x(18);
/// The frame pointer.
pub const FP: PhysReg = x(29);
/// The link register, where a call leaves the address it came from.
pub const LR: PhysReg = x(30);
/// The stack pointer.
pub const SP: PhysReg = PhysReg::new(31);

static GPR_NAMES: [&str; 32] = [
    "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13", "x14",
    "x15", "x16", "x17", "x18", "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27",
    "x28", "x29", "x30", "sp",
];

static FPR_NAMES: [&str; 32] = [
    "v0", "v1", "v2", "v3", "v4", "v5", "v6", "v7", "v8", "v9", "v10", "v11", "v12", "v13", "v14",
    "v15", "v16", "v17", "v18", "v19", "v20", "v21", "v22", "v23", "v24", "v25", "v26", "v27",
    "v28", "v29", "v30", "v31",
];

static CLASSES: [ClassInfo; 2] = [
    ClassInfo { name: "gpr", bits: 64, regs: &GPR_NAMES, allocatable: true },
    ClassInfo { name: "fpr", bits: 128, regs: &FPR_NAMES, allocatable: true },
];

/// Every register AArch64 has, as far as this compiler is concerned.
pub static REGS: RegFile = RegFile::new(&CLASSES);

/// The general purpose registers by DWARF's number for them, which is the machine's.
///
/// The DWARF supplement to AAPCS64 numbers `x0` to `x30` from zero and puts the stack pointer at
/// thirty one, which is exactly the encoding's order, so unlike x86-64 there is no permutation to
/// get wrong. The list is still written out rather than computed, because it is what an unwind
/// table is checked against.
static GPR_DWARF: [u16; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

/// The vector registers by DWARF's number, which start at sixty four.
static FPR_DWARF: [u16; 32] = [
    64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87,
    88, 89, 90, 91, 92, 93, 94, 95,
];

static AARCH64_DWARF: [&[u16]; 2] = [&GPR_DWARF, &FPR_DWARF];

/// The column an unwind table files the return address under, which here is a real register.
///
/// A call on this machine leaves the address it came from in `x30` rather than on the stack, so
/// the return address is whatever `x30` held on entry, and the CIE names that register's column.
pub const DWARF_RETURN_ADDRESS: u16 = 30;

// A register in the other file is moved whole, all sixteen bytes, so that a `long double` spilled
// and reloaded is the one that was spilled. That costs a sixteen byte slot for a `double`, which is
// what the file's width already asks the frame for.
static AARCH64_MOVES: [ClassMoves; 2] = [
    ClassMoves { mov: "mov_rr_64", load: "ldr_64", store: "str_64" },
    ClassMoves { mov: "mov_rr_f128", load: "ldr_f128", store: "str_f128" },
];

/// What an AArch64 prologue, epilogue, spill and reload are made of.
///
/// The same arrangement as `rucc_target::x86_64::FRAME`, and three things about this machine show
/// through it. A push and a pop move the stack pointer by sixteen rather than by a word, because
/// the machine faults on an access through a stack pointer that is not a multiple of sixteen, so
/// the frame code has to count them that way. The frame pointer and the link register go on together
/// as one pair, the frame record, which is what a debugger and `__builtin_frame_address` walk. The
/// stack pointer is register thirty one, which the
/// shifted register forms read as zero, so every instruction here that reads it is one the encoder
/// writes in a form that can say it: an addition of a register to it is the extended form, and the
/// alignment goes through `x16`, since the one instruction that can mask the stack pointer cannot
/// also read it. And there is no landing pad: `-fcf-protection` is an x86 flag, gcc refuses it for
/// this machine, and what this machine has instead is `-mbranch-protection`, which is a different
/// flag with a different instruction and is not here yet.
pub static FRAME: FrameInsts = FrameInsts {
    prefix: "a64.",
    classes: &AARCH64_MOVES,
    push: "push_64",
    pop: "pop_64",
    pair: Some(Pair { push: "push_pair_64", pop: "pop_pair_64" }),
    add: "add_ri_64",
    sub: "sub_ri_64",
    grow: "sub_rr_64",
    align: "align_sp_64",
    imm: "mov_ri_64",
    lea: "lea_64",
    sum: "add_rr_64",
    ret: "ret",
    differ: "cmp_set_ne_64",
    above: "cmp_set_hi_64",
    call: "bl",
    probe: Some(PROBE),
    landing: None,
    pad: Some("nop"),
};

/// How an AArch64 prologue touches a page of the stack it has just reached.
///
/// A store of the zero register, which is what gcc writes. It is not a read and a write back the
/// way the x86 probe is, and it does not have to be: the page is below every local the function
/// has, so nothing is there yet to be kept. Four kilobytes, which is less than the sixty four gcc
/// assumes a guard is on this machine, and so touches more pages than it needs to rather than
/// fewer.
pub static PROBE: Probe = Probe { inst: "probe_64", interval: 4096 };

/// How much of a register each AArch64 instruction reads and writes.
///
/// The widths come from the assembly description, where every general register is named as a `w`
/// or an `x`, and the instructions that copy the low bits of their source are [`Form::Convert`],
/// which is the eight widenings and nothing else.
pub static BITS: BitInsts = BitInsts { prefix: "a64.", width: operand_width, copies_low };

/// What an AArch64 instruction has to look like for this machine to have one.
///
/// Every answer is [`INSTS`] read back. There is one scale, which is one. An index register here is
/// shifted by the size of what is loaded or not at all, so which other scale an address may have
/// depends on the instruction it is in, and this list is not asked per instruction. One is the
/// scale every load and store has.
pub static MACHINE: MachineInsts = MachineInsts {
    prefix: "a64.",
    operands: machine_operands,
    takes_imm: machine_takes_imm,
    takes_mem: machine_takes_mem,
    touches_mem: machine_touches_mem,
    calls: machine_calls,
    scales: &[1],
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

/// Whether an instruction of that name is a call, which is `bl` and `blr`.
#[must_use]
fn machine_calls(name: &str) -> bool {
    form(name) == Some(Form::Call)
}

/// Whether the instruction of that name is one that copies the low bits of its source.
#[must_use]
fn copies_low(name: &str) -> bool {
    form(name) == Some(Form::Convert)
}

/// What an AArch64 conditional branch becomes once the blocks are in an order.
///
/// The test compares the condition register with zero and the two branches read the answer, the
/// way the x86 description has it. This machine also has `cbz` and `cbnz`, which are the test and
/// the branch in one instruction, and they are not used yet: they reach a megabyte either side
/// rather than the whole of a function, and which branches reach is a question for the layout
/// that is not answered here.
pub static BRANCH: BranchInsts = BranchInsts {
    prefix: "a64.",
    cond: "br_cond_32",
    test: "test_32",
    if_true: "b_ne",
    if_false: "b_eq",
    jump: "b",
    indirect: "br",
    conditional: &CONDITIONAL,
    fused: &FUSED,
    moves: &MOVES,
};

/// Every branch on the condition state this machine has, which is fourteen.
///
/// The ten the comparisons are made at, and the sign and the overflow each way round, which only
/// a template writes. The machine has `b.al` and `b.nv` as well, which are a branch that is always
/// taken spelled as a conditional one, and nothing writes those.
static CONDITIONAL: [&str; 14] = [
    "b_eq", "b_ne", "b_lt", "b_le", "b_gt", "b_ge", "b_lo", "b_ls", "b_hi", "b_hs", "b_mi", "b_pl",
    "b_vs", "b_vc",
];

/// Every comparison the test in front of a branch can be taken off.
///
/// Ten conditions at two widths, against a register and against a constant. There is nothing
/// against memory, since this machine has no comparison that reads it.
static FUSED: [Fusion; 40] = [
    Fusion { set: "cmp_set_eq_32", cmp: "cmp_rr_32", if_true: "b_eq", if_false: "b_ne" },
    Fusion { set: "cmp_set_eq_64", cmp: "cmp_rr_64", if_true: "b_eq", if_false: "b_ne" },
    Fusion { set: "cmp_set_ne_32", cmp: "cmp_rr_32", if_true: "b_ne", if_false: "b_eq" },
    Fusion { set: "cmp_set_ne_64", cmp: "cmp_rr_64", if_true: "b_ne", if_false: "b_eq" },
    Fusion { set: "cmp_set_lt_32", cmp: "cmp_rr_32", if_true: "b_lt", if_false: "b_ge" },
    Fusion { set: "cmp_set_lt_64", cmp: "cmp_rr_64", if_true: "b_lt", if_false: "b_ge" },
    Fusion { set: "cmp_set_le_32", cmp: "cmp_rr_32", if_true: "b_le", if_false: "b_gt" },
    Fusion { set: "cmp_set_le_64", cmp: "cmp_rr_64", if_true: "b_le", if_false: "b_gt" },
    Fusion { set: "cmp_set_gt_32", cmp: "cmp_rr_32", if_true: "b_gt", if_false: "b_le" },
    Fusion { set: "cmp_set_gt_64", cmp: "cmp_rr_64", if_true: "b_gt", if_false: "b_le" },
    Fusion { set: "cmp_set_ge_32", cmp: "cmp_rr_32", if_true: "b_ge", if_false: "b_lt" },
    Fusion { set: "cmp_set_ge_64", cmp: "cmp_rr_64", if_true: "b_ge", if_false: "b_lt" },
    Fusion { set: "cmp_set_lo_32", cmp: "cmp_rr_32", if_true: "b_lo", if_false: "b_hs" },
    Fusion { set: "cmp_set_lo_64", cmp: "cmp_rr_64", if_true: "b_lo", if_false: "b_hs" },
    Fusion { set: "cmp_set_ls_32", cmp: "cmp_rr_32", if_true: "b_ls", if_false: "b_hi" },
    Fusion { set: "cmp_set_ls_64", cmp: "cmp_rr_64", if_true: "b_ls", if_false: "b_hi" },
    Fusion { set: "cmp_set_hi_32", cmp: "cmp_rr_32", if_true: "b_hi", if_false: "b_ls" },
    Fusion { set: "cmp_set_hi_64", cmp: "cmp_rr_64", if_true: "b_hi", if_false: "b_ls" },
    Fusion { set: "cmp_set_hs_32", cmp: "cmp_rr_32", if_true: "b_hs", if_false: "b_lo" },
    Fusion { set: "cmp_set_hs_64", cmp: "cmp_rr_64", if_true: "b_hs", if_false: "b_lo" },
    Fusion { set: "cmp_set_eq_ri_32", cmp: "cmp_ri_32", if_true: "b_eq", if_false: "b_ne" },
    Fusion { set: "cmp_set_eq_ri_64", cmp: "cmp_ri_64", if_true: "b_eq", if_false: "b_ne" },
    Fusion { set: "cmp_set_ne_ri_32", cmp: "cmp_ri_32", if_true: "b_ne", if_false: "b_eq" },
    Fusion { set: "cmp_set_ne_ri_64", cmp: "cmp_ri_64", if_true: "b_ne", if_false: "b_eq" },
    Fusion { set: "cmp_set_lt_ri_32", cmp: "cmp_ri_32", if_true: "b_lt", if_false: "b_ge" },
    Fusion { set: "cmp_set_lt_ri_64", cmp: "cmp_ri_64", if_true: "b_lt", if_false: "b_ge" },
    Fusion { set: "cmp_set_le_ri_32", cmp: "cmp_ri_32", if_true: "b_le", if_false: "b_gt" },
    Fusion { set: "cmp_set_le_ri_64", cmp: "cmp_ri_64", if_true: "b_le", if_false: "b_gt" },
    Fusion { set: "cmp_set_gt_ri_32", cmp: "cmp_ri_32", if_true: "b_gt", if_false: "b_le" },
    Fusion { set: "cmp_set_gt_ri_64", cmp: "cmp_ri_64", if_true: "b_gt", if_false: "b_le" },
    Fusion { set: "cmp_set_ge_ri_32", cmp: "cmp_ri_32", if_true: "b_ge", if_false: "b_lt" },
    Fusion { set: "cmp_set_ge_ri_64", cmp: "cmp_ri_64", if_true: "b_ge", if_false: "b_lt" },
    Fusion { set: "cmp_set_lo_ri_32", cmp: "cmp_ri_32", if_true: "b_lo", if_false: "b_hs" },
    Fusion { set: "cmp_set_lo_ri_64", cmp: "cmp_ri_64", if_true: "b_lo", if_false: "b_hs" },
    Fusion { set: "cmp_set_ls_ri_32", cmp: "cmp_ri_32", if_true: "b_ls", if_false: "b_hi" },
    Fusion { set: "cmp_set_ls_ri_64", cmp: "cmp_ri_64", if_true: "b_ls", if_false: "b_hi" },
    Fusion { set: "cmp_set_hi_ri_32", cmp: "cmp_ri_32", if_true: "b_hi", if_false: "b_ls" },
    Fusion { set: "cmp_set_hi_ri_64", cmp: "cmp_ri_64", if_true: "b_hi", if_false: "b_ls" },
    Fusion { set: "cmp_set_hs_ri_32", cmp: "cmp_ri_32", if_true: "b_hs", if_false: "b_lo" },
    Fusion { set: "cmp_set_hs_ri_64", cmp: "cmp_ri_64", if_true: "b_hs", if_false: "b_lo" },
];

/// What a select on a comparison's answer becomes, which is the `csel` on the condition the
/// comparison was asked about.
static MOVES: [Move; 20] = [
    Move { select: "sel_32", when: "b_eq", cmov: "csel_eq_32" },
    Move { select: "sel_32", when: "b_ne", cmov: "csel_ne_32" },
    Move { select: "sel_32", when: "b_lt", cmov: "csel_lt_32" },
    Move { select: "sel_32", when: "b_le", cmov: "csel_le_32" },
    Move { select: "sel_32", when: "b_gt", cmov: "csel_gt_32" },
    Move { select: "sel_32", when: "b_ge", cmov: "csel_ge_32" },
    Move { select: "sel_32", when: "b_lo", cmov: "csel_lo_32" },
    Move { select: "sel_32", when: "b_ls", cmov: "csel_ls_32" },
    Move { select: "sel_32", when: "b_hi", cmov: "csel_hi_32" },
    Move { select: "sel_32", when: "b_hs", cmov: "csel_hs_32" },
    Move { select: "sel_64", when: "b_eq", cmov: "csel_eq_64" },
    Move { select: "sel_64", when: "b_ne", cmov: "csel_ne_64" },
    Move { select: "sel_64", when: "b_lt", cmov: "csel_lt_64" },
    Move { select: "sel_64", when: "b_le", cmov: "csel_le_64" },
    Move { select: "sel_64", when: "b_gt", cmov: "csel_gt_64" },
    Move { select: "sel_64", when: "b_ge", cmov: "csel_ge_64" },
    Move { select: "sel_64", when: "b_lo", cmov: "csel_lo_64" },
    Move { select: "sel_64", when: "b_ls", cmov: "csel_ls_64" },
    Move { select: "sel_64", when: "b_hi", cmov: "csel_hi_64" },
    Move { select: "sel_64", when: "b_hs", cmov: "csel_hs_64" },
];

/// What each AArch64 instruction leaves in the condition state.
pub static FLAGS: FlagInsts = FlagInsts {
    prefix: "a64.",
    width: operand_width,
    writes: writes_flags,
    compares: &COMPARES,
    readers: &READERS,
    compares_itself,
    zeroing: &[],
};

/// The shorter spellings AArch64 has for the same answer, which is none.
///
/// Every instruction is four bytes, so there is no shorter way to write anything, and the
/// rewrites the x86 tables make are all about bytes. The two that are about something else are
/// not worth it here either. `mov x0, x1` and `add x0, x1, #0` are both one instruction the
/// machine does in a cycle, and there is no zero idiom because there is a zero register.
pub static SHORT: ShortInsts = ShortInsts {
    prefix: "a64.",
    zeroing: &[],
    narrowing: &[],
    testing: &[],
    stepping: &[],
    copying: &[],
};

/// Whether the instruction of that name compares and keeps the answer, all in the one instruction.
///
/// The integer ones are in [`COMPARES`] as well. The float ones are not, since nothing takes a
/// float comparison out yet, but what they read is still what they found.
fn compares_itself(name: &str) -> bool {
    matches!(form(name), Some(Form::CmpSet | Form::CmpSetI | Form::FCmpSet))
}

/// Whether the instruction of that name leaves the condition state other than it found it.
///
/// Written as the ones that do, which is the other way round from the x86 list, because on this
/// machine the list is short: an instruction only sets the condition state when its name says
/// so, and the only ones here that do are the comparisons and the two opcodes with a comparison
/// inside them. A call is on it because the function called may have made any comparison it
/// liked, and so is every name this target does not have.
#[must_use]
fn writes_flags(name: &str) -> bool {
    form(name).is_none_or(|shape| {
        matches!(
            shape,
            Form::Cmp
                | Form::CmpI
                | Form::Test
                | Form::CmpSet
                | Form::CmpSetI
                | Form::Select
                | Form::FCmp
                | Form::FCmpSet
                | Form::Call
        )
    })
}

/// Every comparison this machine makes, and what is left of one it has already made.
///
/// Ten conditions at two widths against a register and against a constant, and the four that keep
/// no answer. What is left of one that keeps its answer is the `cset` on its condition. The
/// floating point comparisons are not here, for the reason they are not in the x86 table: a
/// comparison of two numbers where one may be a NaN is not a question this pass knows how to ask
/// twice.
static COMPARES: [Compare; 44] = [
    Compare { name: "cmp_set_eq_32", asks: "cmp_rr_32", kept: Some("cset_eq") },
    Compare { name: "cmp_set_eq_64", asks: "cmp_rr_64", kept: Some("cset_eq") },
    Compare { name: "cmp_set_ne_32", asks: "cmp_rr_32", kept: Some("cset_ne") },
    Compare { name: "cmp_set_ne_64", asks: "cmp_rr_64", kept: Some("cset_ne") },
    Compare { name: "cmp_set_lt_32", asks: "cmp_rr_32", kept: Some("cset_lt") },
    Compare { name: "cmp_set_lt_64", asks: "cmp_rr_64", kept: Some("cset_lt") },
    Compare { name: "cmp_set_le_32", asks: "cmp_rr_32", kept: Some("cset_le") },
    Compare { name: "cmp_set_le_64", asks: "cmp_rr_64", kept: Some("cset_le") },
    Compare { name: "cmp_set_gt_32", asks: "cmp_rr_32", kept: Some("cset_gt") },
    Compare { name: "cmp_set_gt_64", asks: "cmp_rr_64", kept: Some("cset_gt") },
    Compare { name: "cmp_set_ge_32", asks: "cmp_rr_32", kept: Some("cset_ge") },
    Compare { name: "cmp_set_ge_64", asks: "cmp_rr_64", kept: Some("cset_ge") },
    Compare { name: "cmp_set_lo_32", asks: "cmp_rr_32", kept: Some("cset_lo") },
    Compare { name: "cmp_set_lo_64", asks: "cmp_rr_64", kept: Some("cset_lo") },
    Compare { name: "cmp_set_ls_32", asks: "cmp_rr_32", kept: Some("cset_ls") },
    Compare { name: "cmp_set_ls_64", asks: "cmp_rr_64", kept: Some("cset_ls") },
    Compare { name: "cmp_set_hi_32", asks: "cmp_rr_32", kept: Some("cset_hi") },
    Compare { name: "cmp_set_hi_64", asks: "cmp_rr_64", kept: Some("cset_hi") },
    Compare { name: "cmp_set_hs_32", asks: "cmp_rr_32", kept: Some("cset_hs") },
    Compare { name: "cmp_set_hs_64", asks: "cmp_rr_64", kept: Some("cset_hs") },
    Compare { name: "cmp_set_eq_ri_32", asks: "cmp_ri_32", kept: Some("cset_eq") },
    Compare { name: "cmp_set_eq_ri_64", asks: "cmp_ri_64", kept: Some("cset_eq") },
    Compare { name: "cmp_set_ne_ri_32", asks: "cmp_ri_32", kept: Some("cset_ne") },
    Compare { name: "cmp_set_ne_ri_64", asks: "cmp_ri_64", kept: Some("cset_ne") },
    Compare { name: "cmp_set_lt_ri_32", asks: "cmp_ri_32", kept: Some("cset_lt") },
    Compare { name: "cmp_set_lt_ri_64", asks: "cmp_ri_64", kept: Some("cset_lt") },
    Compare { name: "cmp_set_le_ri_32", asks: "cmp_ri_32", kept: Some("cset_le") },
    Compare { name: "cmp_set_le_ri_64", asks: "cmp_ri_64", kept: Some("cset_le") },
    Compare { name: "cmp_set_gt_ri_32", asks: "cmp_ri_32", kept: Some("cset_gt") },
    Compare { name: "cmp_set_gt_ri_64", asks: "cmp_ri_64", kept: Some("cset_gt") },
    Compare { name: "cmp_set_ge_ri_32", asks: "cmp_ri_32", kept: Some("cset_ge") },
    Compare { name: "cmp_set_ge_ri_64", asks: "cmp_ri_64", kept: Some("cset_ge") },
    Compare { name: "cmp_set_lo_ri_32", asks: "cmp_ri_32", kept: Some("cset_lo") },
    Compare { name: "cmp_set_lo_ri_64", asks: "cmp_ri_64", kept: Some("cset_lo") },
    Compare { name: "cmp_set_ls_ri_32", asks: "cmp_ri_32", kept: Some("cset_ls") },
    Compare { name: "cmp_set_ls_ri_64", asks: "cmp_ri_64", kept: Some("cset_ls") },
    Compare { name: "cmp_set_hi_ri_32", asks: "cmp_ri_32", kept: Some("cset_hi") },
    Compare { name: "cmp_set_hi_ri_64", asks: "cmp_ri_64", kept: Some("cset_hi") },
    Compare { name: "cmp_set_hs_ri_32", asks: "cmp_ri_32", kept: Some("cset_hs") },
    Compare { name: "cmp_set_hs_ri_64", asks: "cmp_ri_64", kept: Some("cset_hs") },
    Compare { name: "cmp_rr_32", asks: "cmp_rr_32", kept: None },
    Compare { name: "cmp_rr_64", asks: "cmp_rr_64", kept: None },
    Compare { name: "cmp_ri_32", asks: "cmp_ri_32", kept: None },
    Compare { name: "cmp_ri_64", asks: "cmp_ri_64", kept: None },
];

/// Every instruction that reads the condition state, and which part of it each names.
///
/// The comparisons that keep an answer, the `cset` and `csel` with no comparison in front, and the
/// fourteen branches. `lo` and `hs` are the carry, which is how this machine files an unsigned
/// comparison, and `ls` and `hi` are the carry and the zero together, so all four are unsigned.
static READERS: [Reader; 85] = [
    Reader { name: "cmp_set_eq_32", reads: Reads::Zero },
    Reader { name: "cmp_set_eq_64", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_32", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_64", reads: Reads::Zero },
    Reader { name: "cmp_set_lt_32", reads: Reads::Signed },
    Reader { name: "cmp_set_lt_64", reads: Reads::Signed },
    Reader { name: "cmp_set_le_32", reads: Reads::Signed },
    Reader { name: "cmp_set_le_64", reads: Reads::Signed },
    Reader { name: "cmp_set_gt_32", reads: Reads::Signed },
    Reader { name: "cmp_set_gt_64", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_32", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_64", reads: Reads::Signed },
    Reader { name: "cmp_set_lo_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_lo_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ls_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ls_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_hi_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_hi_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_hs_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_hs_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_eq_ri_32", reads: Reads::Zero },
    Reader { name: "cmp_set_eq_ri_64", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_ri_32", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_ri_64", reads: Reads::Zero },
    Reader { name: "cmp_set_lt_ri_32", reads: Reads::Signed },
    Reader { name: "cmp_set_lt_ri_64", reads: Reads::Signed },
    Reader { name: "cmp_set_le_ri_32", reads: Reads::Signed },
    Reader { name: "cmp_set_le_ri_64", reads: Reads::Signed },
    Reader { name: "cmp_set_gt_ri_32", reads: Reads::Signed },
    Reader { name: "cmp_set_gt_ri_64", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_ri_32", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_ri_64", reads: Reads::Signed },
    Reader { name: "cmp_set_lo_ri_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_lo_ri_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ls_ri_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ls_ri_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_hi_ri_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_hi_ri_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_hs_ri_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_hs_ri_64", reads: Reads::Unsigned },
    Reader { name: "cset_eq", reads: Reads::Zero },
    Reader { name: "cset_ne", reads: Reads::Zero },
    Reader { name: "cset_lt", reads: Reads::Signed },
    Reader { name: "cset_le", reads: Reads::Signed },
    Reader { name: "cset_gt", reads: Reads::Signed },
    Reader { name: "cset_ge", reads: Reads::Signed },
    Reader { name: "cset_lo", reads: Reads::Unsigned },
    Reader { name: "cset_ls", reads: Reads::Unsigned },
    Reader { name: "cset_hi", reads: Reads::Unsigned },
    Reader { name: "cset_hs", reads: Reads::Unsigned },
    Reader { name: "cset_mi", reads: Reads::Bit },
    Reader { name: "csel_eq_32", reads: Reads::Zero },
    Reader { name: "csel_eq_64", reads: Reads::Zero },
    Reader { name: "csel_ne_32", reads: Reads::Zero },
    Reader { name: "csel_ne_64", reads: Reads::Zero },
    Reader { name: "csel_lt_32", reads: Reads::Signed },
    Reader { name: "csel_lt_64", reads: Reads::Signed },
    Reader { name: "csel_le_32", reads: Reads::Signed },
    Reader { name: "csel_le_64", reads: Reads::Signed },
    Reader { name: "csel_gt_32", reads: Reads::Signed },
    Reader { name: "csel_gt_64", reads: Reads::Signed },
    Reader { name: "csel_ge_32", reads: Reads::Signed },
    Reader { name: "csel_ge_64", reads: Reads::Signed },
    Reader { name: "csel_lo_32", reads: Reads::Unsigned },
    Reader { name: "csel_lo_64", reads: Reads::Unsigned },
    Reader { name: "csel_ls_32", reads: Reads::Unsigned },
    Reader { name: "csel_ls_64", reads: Reads::Unsigned },
    Reader { name: "csel_hi_32", reads: Reads::Unsigned },
    Reader { name: "csel_hi_64", reads: Reads::Unsigned },
    Reader { name: "csel_hs_32", reads: Reads::Unsigned },
    Reader { name: "csel_hs_64", reads: Reads::Unsigned },
    Reader { name: "b_eq", reads: Reads::Zero },
    Reader { name: "b_ne", reads: Reads::Zero },
    Reader { name: "b_lt", reads: Reads::Signed },
    Reader { name: "b_le", reads: Reads::Signed },
    Reader { name: "b_gt", reads: Reads::Signed },
    Reader { name: "b_ge", reads: Reads::Signed },
    Reader { name: "b_lo", reads: Reads::Unsigned },
    Reader { name: "b_ls", reads: Reads::Unsigned },
    Reader { name: "b_hi", reads: Reads::Unsigned },
    Reader { name: "b_hs", reads: Reads::Unsigned },
    Reader { name: "b_mi", reads: Reads::Bit },
    Reader { name: "b_pl", reads: Reads::Bit },
    Reader { name: "b_vs", reads: Reads::Bit },
    Reader { name: "b_vc", reads: Reads::Bit },
];

static INT_ARGS: [PhysReg; 8] = [x(0), x(1), x(2), x(3), x(4), x(5), x(6), x(7)];
static FP_ARGS: [PhysReg; 8] = [v(0), v(1), v(2), v(3), v(4), v(5), v(6), v(7)];
// Two for a sixteen byte integer or a small structure, and four for a homogeneous aggregate of four
// members, which is the most either bank gives back.
static INT_RETURNS: [PhysReg; 2] = [x(0), x(1)];
static FP_RETURNS: [PhysReg; 4] = [v(0), v(1), v(2), v(3)];
static NO_X87: [PhysReg; 0] = [];
// The frame pointer and the link register are not here, although a callee gives both back. The
// frame record the prologue writes saves them as a pair whether or not the function writes to
// either, so they are the frame's business rather than the allocator's, the way `rbp` is on
// x86-64.
static INT_SAVED: [PhysReg; 10] =
    [x(19), x(20), x(21), x(22), x(23), x(24), x(25), x(26), x(27), x(28)];
// Only the low sixty four bits of these are preserved, which is `d8` to `d15`. That is enough for
// everything this compiler keeps in one, which is a scalar, and it is why a prologue saves eight
// bytes of each rather than sixteen. A value that used the whole register would not survive a call
// in any of them.
static FP_SAVED: [PhysReg; 8] = [v(8), v(9), v(10), v(11), v(12), v(13), v(14), v(15)];

// The ones a call destroys first, so a value that does not live across one costs no save, then the
// ones it preserves. Left out: `x16` and `x17`, which a linker's veneer or a PLT entry may write
// between any call and the function it reaches, so nothing can live in one across a branch the
// compiler did not write; `x18`, for the reason [`X18`] gives, which on Linux is a choice to be the
// same as the other two platforms rather than an obligation; the frame pointer, the link register
// and the stack pointer. `x8` is handed out like any other, because it only has a job at the
// instant of a call that returns a large structure and the call constrains it there.
static INT_ORDER: [PhysReg; 26] = [
    x(0),
    x(1),
    x(2),
    x(3),
    x(4),
    x(5),
    x(6),
    x(7),
    x(8),
    x(9),
    x(10),
    x(11),
    x(12),
    x(13),
    x(14),
    x(15),
    x(19),
    x(20),
    x(21),
    x(22),
    x(23),
    x(24),
    x(25),
    x(26),
    x(27),
    x(28),
];
// The same idea: the eight argument registers, the sixteen above the preserved block, and the
// preserved block last.
static FP_ORDER: [PhysReg; 32] = [
    v(0),
    v(1),
    v(2),
    v(3),
    v(4),
    v(5),
    v(6),
    v(7),
    v(16),
    v(17),
    v(18),
    v(19),
    v(20),
    v(21),
    v(22),
    v(23),
    v(24),
    v(25),
    v(26),
    v(27),
    v(28),
    v(29),
    v(30),
    v(31),
    v(8),
    v(9),
    v(10),
    v(11),
    v(12),
    v(13),
    v(14),
    v(15),
];

/// Where an AAPCS64 call puts things on Linux and every other platform that uses it unchanged, per
/// `spec/12-abi-and-runtime.md` section 12.3.
pub static AAPCS64: CallRegs = aapcs64(&rucc_abi::abis::AAPCS64, 0);

/// Where a call puts things on Apple's platforms.
///
/// The same registers as [`AAPCS64`]. What Apple changed is how a variadic argument and a stack
/// argument travel, which is in the description rather than here, and one thing that is here: a
/// leaf function may use the hundred and twenty eight bytes below the stack pointer, which Apple's
/// ABI document promises and the Linux one does not.
pub static DARWIN: CallRegs = aapcs64(&rucc_abi::abis::DARWIN_ARM64, 128);

const fn aapcs64(abi: &'static rucc_abi::AbiDescription, red_zone: u32) -> CallRegs {
    CallRegs {
        abi,
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
        int_order: &INT_ORDER,
        sse_order: &FP_ORDER,
        stack_pointer: SP,
        frame_pointer: FP,
        // The frame record is the first thing pushed and the pointer is set to it straight away,
        // which is the order that keeps the chain: the word it names is the caller's frame pointer
        // and the one above it is the return address.
        late_frame_pointer: false,
        // Nothing says how many vector registers a variadic call used. The callee saves all eight
        // in its prologue if it is variadic at all, which is cheaper on this machine than the
        // branch SysV spends a register on.
        vector_count: None,
        red_zone,
        shadow: 0,
        stack_align: 16,
        // A call leaves the return address in the link register, so it pushes nothing and the
        // stack pointer is aligned on entry, which is the thing x86-64's frame layout has to undo.
        return_address: 0,
        word: 8,
        push: 16,
        link: Some(LR),
        dwarf: &AARCH64_DWARF,
        dwarf_return_address: DWARF_RETURN_ADDRESS,
        // None for now, and it is a different mechanism rather than a missing number. glibc and
        // musl on this machine keep the canary in an ordinary global, `__stack_chk_guard`, which is
        // read through the GOT like any other, where [`crate::Guard`] describes a word at a fixed
        // distance into a thread's own block. A command line that asks for a protector is told so
        // until the frame code can load a global.
        guard: None,
        // None for the same kind of reason. gcc's hook on this machine is `_mcount` called with the
        // caller's link register, and `-mfentry` does not exist here, so it waits for the frame code
        // too rather than being described as something it is not.
        trace: None,
        // Linux and Apple both grow a stack by faulting, as System V does on x86-64.
        chkstk: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_has_one_name_per_register() {
        assert_eq!(REGS.duplicate(), None);
        assert_eq!(REGS.len(GPR), 32);
        assert_eq!(REGS.len(FPR), 32);
        assert_eq!(REGS.name(GPR, SP), Some("sp"));
        assert_eq!(REGS.name(GPR, LR), Some("x30"));
        assert_eq!(REGS.reg_named("v31"), Some((FPR, v(31))));
    }

    #[test]
    fn dwarf_numbers_are_the_machine_numbers_and_the_vectors_start_at_sixty_four() {
        for regs in [&AAPCS64, &DARWIN] {
            assert_eq!(regs.dwarf(GPR, x(0)), Some(0));
            assert_eq!(regs.dwarf(GPR, SP), Some(31));
            assert_eq!(regs.dwarf(FPR, v(8)), Some(72));
            assert_eq!(regs.machine(FPR, 95), Some(v(31)));
            assert_eq!(regs.dwarf_return_address, 30);
        }
    }

    #[test]
    fn nothing_with_a_job_is_handed_out() {
        for reserved in [X16, X17, X18, FP, LR, SP] {
            assert!(!AAPCS64.int_order.contains(&reserved), "{reserved:?}");
        }
        // Every register the allocator may use is either one a call destroys or one the frame
        // saves, and every one the frame saves is one it may use.
        for reg in AAPCS64.int_saved {
            assert!(AAPCS64.int_order.contains(reg), "{reg:?}");
        }
        assert_eq!(AAPCS64.int_order.len(), 26);
        assert_eq!(AAPCS64.sse_order.len(), 32);
    }

    #[test]
    fn the_two_conventions_differ_in_the_red_zone_and_the_description() {
        assert_eq!(AAPCS64.red_zone, 0);
        assert_eq!(DARWIN.red_zone, 128);
        assert!(std::ptr::eq(AAPCS64.abi, &rucc_abi::abis::AAPCS64));
        assert!(std::ptr::eq(DARWIN.abi, &rucc_abi::abis::DARWIN_ARM64));
        assert_eq!(AAPCS64.int_args, DARWIN.int_args);
        assert_eq!(AAPCS64.return_address, 0);
    }

    fn described(name: &str) -> bool {
        form(name).is_some()
    }

    /// Every name the descriptions hand out is one this machine has, since a pass writes what it
    /// is given and the encoder is the first thing to find out that it is nothing.
    #[test]
    fn every_name_a_description_hands_out_is_an_opcode() {
        let mut names = vec![FRAME.push, FRAME.pop, FRAME.add, FRAME.sub, FRAME.grow, FRAME.align];
        names.extend([FRAME.imm, FRAME.lea, FRAME.sum, FRAME.ret, FRAME.differ, FRAME.above]);
        names.extend([FRAME.call, PROBE.inst, FRAME.pad.expect("a pad")]);
        for moves in FRAME.classes {
            names.extend([moves.mov, moves.load, moves.store]);
        }
        names.extend([BRANCH.cond, BRANCH.test, BRANCH.if_true, BRANCH.if_false, BRANCH.jump]);
        names.push(BRANCH.indirect);
        names.extend(BRANCH.conditional);
        for entry in COMPARES {
            names.push(entry.asks);
            names.extend(entry.kept);
        }
        names.extend(READERS.iter().map(|entry| entry.name));
        for name in names {
            assert!(described(name), "{name} is not an opcode");
        }
        assert_eq!(FRAME.classes.len(), 2, "both files are spilled");
    }

    /// Every comparison that keeps its answer has a branch it folds into, and the two branches of
    /// an entry are a condition and its opposite, checked by going there and back.
    #[test]
    fn every_comparison_folds_into_a_branch_and_its_opposite() {
        let sets: Vec<&str> = INSTS
            .iter()
            .filter(|&&(_, shape)| matches!(shape, Form::CmpSet | Form::CmpSetI))
            .map(|&(name, _)| name)
            .collect();
        let entries: Vec<&str> = BRANCH.fused.iter().map(|fusion| fusion.set).collect();
        assert_eq!(entries, sets);
        for fusion in BRANCH.fused {
            let before = form(fusion.set).expect("described").operands();
            let after = form(fusion.cmp).expect("described").operands();
            assert_eq!(after, &before[1..], "{} and {}", fusion.set, fusion.cmp);
            assert_eq!(form(fusion.if_true), Some(Form::Jcc));
            assert_eq!(form(fusion.if_false), Some(Form::Jcc));
            let back = BRANCH
                .fused
                .iter()
                .find(|other| other.if_true == fusion.if_false && other.cmp == fusion.cmp)
                .unwrap_or_else(|| panic!("{} has no opposite", fusion.if_false));
            assert_eq!(back.if_false, fusion.if_true);
            let asked = fusion.if_true.strip_prefix("b_").expect("a branch");
            assert!(fusion.set.starts_with(&format!("cmp_set_{asked}_")), "{}", fusion.set);
        }
        assert_eq!(BRANCH.conditional.len(), 14);
    }

    /// Every select has one move per condition, and the move is the select without its byte.
    #[test]
    fn every_select_has_the_csel_that_reads_each_condition() {
        for entry in BRANCH.moves {
            let before = form(entry.select).expect("described").operands();
            let after = form(entry.cmov).expect("described").operands();
            assert_eq!(form(entry.cmov), Some(Form::Csel));
            assert_eq!(after, &before[..before.len() - 1], "{} and {}", entry.select, entry.cmov);
            let asked = entry.when.strip_prefix("b_").expect("a branch");
            assert!(entry.cmov.starts_with(&format!("csel_{asked}_")), "{}", entry.cmov);
        }
        assert_eq!(BRANCH.moves.len(), 2 * 10);
    }

    /// Every comparison is in the flag table, and what is left of one that keeps its answer is the
    /// `cset` on its own condition, with the operands adding back up to the comparison.
    #[test]
    fn every_comparison_says_what_it_asks_and_what_is_left_of_it() {
        let comparisons: Vec<&str> = INSTS
            .iter()
            .filter(|&&(_, shape)| {
                matches!(shape, Form::CmpSet | Form::CmpSetI | Form::Cmp | Form::CmpI)
            })
            .map(|&(name, _)| name)
            .collect();
        let mut entries: Vec<&str> = COMPARES.iter().map(|entry| entry.name).collect();
        let mut sorted = comparisons.clone();
        entries.sort_unstable();
        sorted.sort_unstable();
        assert_eq!(entries, sorted);
        for entry in &COMPARES {
            let operands = form(entry.name).expect("described").operands();
            match entry.kept {
                Some(kept) => {
                    assert_eq!(form(kept), Some(Form::Set), "{kept}");
                    assert_eq!(form(kept).expect("described").operands(), &operands[..1]);
                    let asks = form(entry.asks).expect("described").operands();
                    assert_eq!(asks, &operands[1..], "{} and {}", entry.name, entry.asks);
                    let condition = kept.strip_prefix("cset_").expect("a cset");
                    assert!(entry.name.starts_with(&format!("cmp_set_{condition}_")));
                }
                None => assert_eq!(entry.asks, entry.name),
            }
            assert!(writes_flags(entry.name), "{}", entry.name);
        }
    }

    /// Every instruction that reads the condition state names which part, and the part is the one
    /// the condition in its name is about.
    #[test]
    fn every_condition_says_which_part_of_the_state_it_is_about() {
        let readers: Vec<&str> = INSTS
            .iter()
            .filter(|&&(_, shape)| {
                matches!(shape, Form::CmpSet | Form::CmpSetI | Form::Set | Form::Csel | Form::Jcc)
            })
            .map(|&(name, _)| name)
            .collect();
        let mut entries: Vec<&str> = READERS.iter().map(|entry| entry.name).collect();
        let mut sorted = readers.clone();
        entries.sort_unstable();
        sorted.sort_unstable();
        assert_eq!(entries, sorted);
        for entry in &READERS {
            let condition = entry
                .name
                .split('_')
                .find(|part| part.len() == 2 && !part.starts_with('r'))
                .unwrap_or_else(|| panic!("{} has no condition", entry.name));
            let want = match condition {
                "eq" | "ne" => Reads::Zero,
                "lt" | "le" | "gt" | "ge" => Reads::Signed,
                "lo" | "ls" | "hi" | "hs" => Reads::Unsigned,
                _ => Reads::Bit,
            };
            assert_eq!(entry.reads, want, "{}", entry.name);
        }
    }

    /// Only the comparisons, the two opcodes with one inside and a call write the condition state.
    #[test]
    fn arithmetic_leaves_the_condition_state_alone_and_a_comparison_does_not() {
        for name in ["add_rr_64", "sub_ri_32", "and_rr_64", "ldr_64", "csel_eq_32", "b_ne"] {
            assert!(!writes_flags(name), "{name}");
        }
        for name in ["cmp_rr_32", "test_64", "sel_32", "fcmp_f64", "fcmp_set_lt_f32", "bl"] {
            assert!(writes_flags(name), "{name}");
        }
        assert!(writes_flags("adds_rr_64"), "a name this target does not have");
        assert!(FLAGS.zeroing.is_empty());
    }

    /// A compare that keeps its answer reads what it found, and a `cset` or a `csel` reads what
    /// the instruction in front of it left.
    #[test]
    fn a_compare_that_keeps_its_answer_reads_what_it_found() {
        for name in ["cmp_set_eq_32", "cmp_set_hs_ri_64", "fcmp_set_lt_f64"] {
            assert!(compares_itself(name), "{name}");
        }
        for name in ["cset_eq", "csel_ne_64", "b_lt", "cmp_rr_64", "sel_32", "adds_rr_64"] {
            assert!(!compares_itself(name), "{name}");
        }
    }

    /// The widths come from the listing, and an operand in the other file or an addressing mode is
    /// all of it.
    #[test]
    fn the_width_of_an_operand_is_the_width_it_is_written_at() {
        assert_eq!((BITS.width)("add_rr_32", 0), Some(32));
        assert_eq!((BITS.width)("add_rr_64", 2), Some(64));
        assert_eq!((BITS.width)("sxtb_64", 0), Some(64));
        assert_eq!((BITS.width)("sxtb_64", 1), Some(32));
        assert_eq!((BITS.width)("sel_64", 3), Some(32));
        assert_eq!((BITS.width)("fadd_f64", 0), None);
        assert_eq!((BITS.width)("scvtf_64_f64", 1), Some(64));
        assert_eq!((BITS.width)("ldr_64", 0), Some(64));
        assert!((BITS.copies_low)("uxtb_32"));
        assert!(!(BITS.copies_low)("mov_rr_64"));
    }

    #[test]
    fn a_machine_pass_is_held_to_the_same_table() {
        assert!(MACHINE.calls("a64.blr"));
        assert!((MACHINE.takes_mem)("probe_64"));
        assert!(MACHINE.touches_mem("a64.push_64"));
        assert!(!MACHINE.touches_mem("a64.lea_64"));
        assert!(MACHINE.has("a64.cset_eq"));
        assert!(!MACHINE.has("a64.adds_rr_64"));
        assert!(MACHINE.scales(1) && !MACHINE.scales(8));
    }
}
