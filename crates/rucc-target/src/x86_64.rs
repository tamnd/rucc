//! The x86-64 register file, and where each of the two conventions over it puts things.
//!
//! Design: `spec/10-backend.md` section 10.8 and `spec/12-abi-and-runtime.md` section 12.2.
//!
//! Registers are numbered the way the instruction encoding numbers them, so `rax` is zero,
//! `rsp` is four and `r15` is fifteen. Nothing in the compiler above this file depends on that,
//! because a physical register is a number inside its class and the allocator never asks what a
//! number means. The encoder does, and an encoder that had to translate from an order somebody
//! chose for readability into the order the machine uses would be one more place to be wrong.
//!
//! The names here are the sixty-four bit ones. A register is one register whatever width an
//! instruction reads it at, and `al`, `ax` and `eax` are three ways of writing part of `rax`
//! rather than three registers, so the width belongs to the instruction and the assembler picks
//! the spelling from it. That is also why the classes are the two the machine really has rather
//! than one per width.
//!
//! # What is not here
//!
//! The flags register, because no rule in the lowering set produces one: a comparison and the
//! branch or the set that reads it are one rule and one machine term, which is what keeps flags
//! out of the allocator's way. The segment, control and debug registers, because nothing but
//! inline assembly names them and inline assembly names them as text. And the mask and upper
//! vector registers, which arrive with the target features that have them.
//!
//! What each machine instruction does with its operands is here too, as [`Form`] and the
//! [`INSTS`] table [`form`] reads. It is the same kind of thing as the register file, so it
//! is in this crate and not in the one that selects instructions or the one that encodes them:
//! both of those read it and neither owns it.
//!
//! And what each of them is in assembly, which is [`written`], and what each of them is in bytes,
//! which is [`encode`]. The text an assembler reads and the bytes a processor reads are two
//! spellings of one instruction, so they come from one description rather than from two that
//! could disagree: both paths walk the same [`Written`] list and only the last step differs.

mod encode;
mod insts;
mod read;
mod text;
mod timing;

pub use crate::x86_64::encode::{
    Addr, Encoding, Error, Fields, Fits, Holes, ImmSize, Kind, Size, Value, encode, encoding, nops,
};
pub use crate::x86_64::insts::{
    ADDRESSES, ALIGN, Address, Form, INSTS, LITERAL, LITERALS, TEMPLATE, TEMPLATE_MEM, address,
    form, packed, template_filled, template_name, unpacked,
};
pub use crate::x86_64::read::{At, Disp, Line, Piece, Step, read, read_in};
pub use crate::x86_64::text::{
    Arg, Shape, Width, Written, gpr_high, gpr_letter, gpr_name, gpr_named, machine, operand_width,
    written,
};
pub use crate::x86_64::timing::{LOAD, MODEL, TIMING};

use crate::bits::BitInsts;
use crate::branch::{BranchInsts, Fusion, Move};
use crate::flags::{Compare, FlagInsts, Reader, Reads, Zeroing};
use crate::frame::{ClassMoves, FrameInsts, Probe};
use crate::machine::MachineInsts;
use crate::operand::OperandDesc;
use crate::regs::{CallRegs, Chkstk, ClassInfo, Guard, PhysReg, RegClass, RegFile, Segment, Trace};
use crate::short::{Copied, Narrowed, ShortInsts, Stepped, Tested, Zeroed};

/// The general purpose registers.
pub const GPR: RegClass = RegClass::new(0);
/// The vector registers.
pub const XMM: RegClass = RegClass::new(1);
/// The x87 stack, which is where a `long double` lives.
pub const X87: RegClass = RegClass::new(2);

/// One general purpose register, by the number the encoding gives it.
pub const RAX: PhysReg = PhysReg::new(0);
/// One general purpose register, by the number the encoding gives it.
pub const RCX: PhysReg = PhysReg::new(1);
/// One general purpose register, by the number the encoding gives it.
pub const RDX: PhysReg = PhysReg::new(2);
/// One general purpose register, by the number the encoding gives it.
pub const RBX: PhysReg = PhysReg::new(3);
/// The stack pointer.
pub const RSP: PhysReg = PhysReg::new(4);
/// The frame pointer.
pub const RBP: PhysReg = PhysReg::new(5);
/// One general purpose register, by the number the encoding gives it.
pub const RSI: PhysReg = PhysReg::new(6);
/// One general purpose register, by the number the encoding gives it.
pub const RDI: PhysReg = PhysReg::new(7);
/// One general purpose register, by the number the encoding gives it.
pub const R8: PhysReg = PhysReg::new(8);
/// One general purpose register, by the number the encoding gives it.
pub const R9: PhysReg = PhysReg::new(9);
/// One general purpose register, by the number the encoding gives it.
pub const R10: PhysReg = PhysReg::new(10);
/// One general purpose register, by the number the encoding gives it.
pub const R11: PhysReg = PhysReg::new(11);
/// One general purpose register, by the number the encoding gives it.
pub const R12: PhysReg = PhysReg::new(12);
/// One general purpose register, by the number the encoding gives it.
pub const R13: PhysReg = PhysReg::new(13);
/// One general purpose register, by the number the encoding gives it.
pub const R14: PhysReg = PhysReg::new(14);
/// One general purpose register, by the number the encoding gives it.
pub const R15: PhysReg = PhysReg::new(15);

/// The vector register with that number.
///
/// # Panics
///
/// Panics if there is no such register, which is sixteen or more without an extension this
/// crate does not describe yet.
#[must_use]
pub const fn xmm(number: u8) -> PhysReg {
    assert!(number < 16, "x86-64 has sixteen vector registers without an extension");
    PhysReg::new(number)
}

/// The x87 register with that number, counted from the top of the stack.
///
/// # Panics
///
/// Panics if the number is eight or more, which is past the bottom of the stack.
#[must_use]
pub const fn st(number: u8) -> PhysReg {
    assert!(number < 8, "the x87 stack is eight deep");
    PhysReg::new(number)
}

static GPR_NAMES: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];

static XMM_NAMES: [&str; 16] = [
    "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5", "xmm6", "xmm7", "xmm8", "xmm9", "xmm10",
    "xmm11", "xmm12", "xmm13", "xmm14", "xmm15",
];

static X87_NAMES: [&str; 8] = ["st0", "st1", "st2", "st3", "st4", "st5", "st6", "st7"];

static CLASSES: [ClassInfo; 3] = [
    ClassInfo { name: "gpr", bits: 64, regs: &GPR_NAMES, allocatable: true },
    ClassInfo { name: "xmm", bits: 128, regs: &XMM_NAMES, allocatable: true },
    // Eighty bits of value in a register the machine addresses as a stack rather than by number,
    // which is why nothing allocates from this class: `st1` means whichever register is one below
    // the top at the moment the instruction runs, so a name here does not fix a register the way a
    // name in the other two classes does, and an allocator that handed one out would be handing
    // out something whose meaning depends on how many values happen to be on the stack. So an
    // eighty bit value lives in a stack slot between one operation and the next and the stack is
    // empty on both sides of every group of instructions that uses it, which is what `fld_t` and
    // `fstp_t` come in and out of. The class is described because a `long double` comes back in
    // `st0` and something has to be able to say so.
    ClassInfo { name: "x87", bits: 80, regs: &X87_NAMES, allocatable: false },
];

/// Every register x86-64 has.
pub static REGS: RegFile = RegFile::new(&CLASSES);

/// The general purpose registers by DWARF's number for them, indexed by the machine's.
///
/// The two orders are not the same and the difference is not a shift. The machine's numbering is
/// the one the instruction encoding uses and it puts `rcx` at one and `rbx` at three. DWARF's is
/// the one the psABI fixes for unwind tables and debug information, and it puts `rdx` at one and
/// `rcx` at two, so the first eight are permuted and the upper eight happen to agree. Getting it
/// wrong produces a table that is well formed and describes the wrong registers, which is a
/// backtrace with plausible nonsense in it rather than an error.
static GPR_DWARF: [u16; 16] = [0, 2, 1, 3, 7, 6, 4, 5, 8, 9, 10, 11, 12, 13, 14, 15];

/// The vector registers by DWARF's number, which start above the return address column and run in
/// the machine's order. This is the one place the two numberings agree by construction.
static XMM_DWARF: [u16; 16] = [17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32];

/// What DWARF calls each x86-64 register, per class.
///
/// Two lists rather than three: the x87 stack has no column, because a register whose name means
/// whichever one is on top of the stack is not one a table can have a column for, and nothing
/// saves one across a call so nothing asks.
static X86_64_DWARF: [&[u16]; 2] = [&GPR_DWARF, &XMM_DWARF];

/// The number the return address is filed under, which on x86-64 is a column of its own rather
/// than a real register, because `rip` is not one anything can name.
pub const DWARF_RETURN_ADDRESS: u16 = 16;

// A vector register is moved with the aligned form, which is a fault rather than a slow
// instruction when the address is wrong. That is deliberate: every address a frame produces for
// one is a multiple of sixteen by construction, so the aligned form is both the fast one and the
// one that says so out loud if the frame layout ever stops being true.
static X86_64_MOVES: [ClassMoves; 2] = [
    ClassMoves { mov: "mov_rr_64", load: "mov_rm_64", store: "mov_mr_64" },
    ClassMoves { mov: "movaps_rr", load: "movaps_rm", store: "movaps_mr" },
];

/// What an x86-64 prologue, epilogue, spill and reload are made of.
///
/// The arithmetic and the address computation are opcodes the lowering rules also select, and
/// they are the same opcodes here: a prologue taking its frame is the same instruction as a
/// subtraction the program wrote, and the encoder should not have two answers for it. The moves,
/// the pushes, the pops and the return are opcodes no rule selects, so they appear here and
/// nowhere else.
pub static FRAME: FrameInsts = FrameInsts {
    prefix: "x64.",
    classes: &X86_64_MOVES,
    push: "push_64",
    pop: "pop_64",
    add: "add_ri_64",
    sub: "sub_ri_64",
    grow: "sub_rr_64",
    align: "and_ri_64",
    imm: "mov_ri_64",
    lea: "lea_64",
    sum: "add_rr_64",
    ret: "ret",
    differ: "cmp_set_ne_64",
    above: "cmp_set_a_64",
    call: "call",
    probe: Some(PROBE),
    landing: Some("endbr64"),
    pad: Some("nop"),
};

/// How an x86-64 prologue touches a page of the stack it has just reached.
///
/// The instruction is an inclusive or of zero with the byte the stack pointer points at, which
/// reads that byte and writes it back unchanged. That is what makes it usable on a page nothing
/// has been put in yet: a store would be a write of something, and the whole point is that the
/// page is only being reached rather than used. It is also one instruction rather than two, which
/// a load and a compare would be, and it needs no register at all, which matters in a prologue
/// where the registers still hold the caller's arguments.
///
/// Four kilobytes because that is the page every operating system this target runs on leaves
/// below a stack, and it is what gcc's `--param=stack-clash-protection-probe-interval` defaults
/// to. A kernel configured with a larger guard needs a larger number here and nothing else.
pub static PROBE: Probe = Probe { inst: "or_mi_8", interval: 4096 };

/// How much of a register each x86-64 instruction reads and writes.
///
/// Both answers come out of the tables next door rather than out of a list written here.
/// [`operand_width`] reads the assembly description, which names every operand at the width the
/// instruction uses it at, and the family that copies the low bits of its source is exactly
/// [`Form::Convert`], which is what the eleven widenings, the four that widen a truth value and
/// the three that take the low part are already marked as and what nothing else is.
pub static BITS: BitInsts = BitInsts { prefix: "x64.", width: operand_width, copies_low };

/// What an x86-64 instruction has to look like for this machine to have one.
///
/// Every answer is [`INSTS`] read back, which is the same table the allocator asks what an
/// instruction does with its operands and the same one the encoder is written against. A pass
/// proposing a rewrite is therefore held to the description the machine already had rather than
/// to a second one, which is the point [`crate::MachineInsts`] makes at length.
///
/// The scales are the four an x86-64 addressing mode can multiply an index by, which the encoder
/// writes into two bits of the scale index base byte.
pub static MACHINE: MachineInsts = MachineInsts {
    prefix: "x64.",
    operands: machine_operands,
    takes_imm: machine_takes_imm,
    takes_mem: machine_takes_mem,
    touches_mem: machine_touches_mem,
    calls: machine_calls,
    scales: &[1, 2, 4, 8],
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

/// Whether an instruction of that name is a call.
///
/// [`Form::Call`] and nothing else. An indirect call is the same form, which is right here: what
/// a pass asks this for is which registers are gone across the instruction, and a call through an
/// address destroys the same ones a call to a name does.
#[must_use]
fn machine_calls(name: &str) -> bool {
    form(name) == Some(Form::Call)
}

/// Whether the instruction of that name is one that copies the low bits of its source.
///
/// [`Form::Convert`] and nothing else, because that form is the machine's moves between widths:
/// every one of them reads one register at one width, writes another at another width, and
/// agrees with its source about every bit the narrower of the two has. A conversion to or from
/// the vector file is a different form, which is what keeps a float out of this.
#[must_use]
fn copies_low(name: &str) -> bool {
    form(name) == Some(Form::Convert)
}

/// What an x86-64 conditional branch becomes once the blocks are in an order.
///
/// The test is of the condition byte against itself, which is what asks whether it is zero, and
/// the two conditional jumps read the answer. `jcc_e` is taken when the byte was zero, which is
/// when the condition did not hold, so it is the one a block ends with when the arm it falls
/// through to is the one the condition is true for.
pub static BRANCH: BranchInsts = BranchInsts {
    prefix: "x64.",
    cond: "br_cond_8",
    test: "test_rr_8",
    if_true: "jcc_ne",
    if_false: "jcc_e",
    jump: "jmp",
    indirect: "jmp_reg",
    conditional: &CONDITIONAL,
    fused: &FUSED,
    moves: &MOVES,
};

/// Every jump on the condition state this machine has, which is sixteen conditions.
///
/// The ten the comparisons are described at, and the sign, the overflow and the parity each way
/// round, which only a template writes. These are the sixteen a template may write, since a jump the
/// reader gives back is one of these by name. What reads this is the layout, to tell a
/// block whose jump is already there from one that still wants a test and a jump behind it.
static CONDITIONAL: [&str; 16] = [
    "jcc_e", "jcc_ne", "jcc_l", "jcc_le", "jcc_g", "jcc_ge", "jcc_b", "jcc_be", "jcc_a", "jcc_ae",
    "jcc_s", "jcc_ns", "jcc_o", "jcc_no", "jcc_p", "jcc_np",
];

/// Every comparison the test in front of a branch can be taken off, which is all of them.
///
/// Ten conditions at four widths, against a register, against a constant and against memory. A
/// comparison sets the condition state whether or not anybody keeps the byte, so the entry for one
/// is the same comparison without the `set` and the jump on the condition the `set` was naming.
/// The two conditional jumps in an entry are a condition and its opposite, since which of them the
/// block gets is which of its arms was laid out next.
///
/// The forty against memory are here for the reason the other eighty are and one more. Without
/// them a comparison that had a load folded into it would be a comparison the layout walks past,
/// and the branch behind it would keep the byte and the test that reads it, which would make
/// folding the load a saving of one instruction and a cost of two.
static FUSED: [Fusion; 160] = [
    Fusion { set: "cmp_set_e_8", cmp: "cmp_rr_8", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_16", cmp: "cmp_rr_16", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_32", cmp: "cmp_rr_32", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_64", cmp: "cmp_rr_64", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_ne_8", cmp: "cmp_rr_8", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_16", cmp: "cmp_rr_16", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_32", cmp: "cmp_rr_32", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_64", cmp: "cmp_rr_64", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_l_8", cmp: "cmp_rr_8", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_16", cmp: "cmp_rr_16", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_32", cmp: "cmp_rr_32", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_64", cmp: "cmp_rr_64", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_le_8", cmp: "cmp_rr_8", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_16", cmp: "cmp_rr_16", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_32", cmp: "cmp_rr_32", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_64", cmp: "cmp_rr_64", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_g_8", cmp: "cmp_rr_8", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_16", cmp: "cmp_rr_16", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_32", cmp: "cmp_rr_32", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_64", cmp: "cmp_rr_64", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_ge_8", cmp: "cmp_rr_8", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_16", cmp: "cmp_rr_16", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_32", cmp: "cmp_rr_32", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_64", cmp: "cmp_rr_64", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_b_8", cmp: "cmp_rr_8", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_16", cmp: "cmp_rr_16", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_32", cmp: "cmp_rr_32", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_64", cmp: "cmp_rr_64", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_be_8", cmp: "cmp_rr_8", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_16", cmp: "cmp_rr_16", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_32", cmp: "cmp_rr_32", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_64", cmp: "cmp_rr_64", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_a_8", cmp: "cmp_rr_8", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_16", cmp: "cmp_rr_16", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_32", cmp: "cmp_rr_32", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_64", cmp: "cmp_rr_64", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_ae_8", cmp: "cmp_rr_8", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_16", cmp: "cmp_rr_16", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_32", cmp: "cmp_rr_32", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_64", cmp: "cmp_rr_64", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_e_ri_8", cmp: "cmp_ri_8", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_ri_16", cmp: "cmp_ri_16", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_ri_32", cmp: "cmp_ri_32", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_ri_64", cmp: "cmp_ri_64", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_ne_ri_8", cmp: "cmp_ri_8", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_ri_16", cmp: "cmp_ri_16", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_ri_32", cmp: "cmp_ri_32", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_ri_64", cmp: "cmp_ri_64", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_l_ri_8", cmp: "cmp_ri_8", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_ri_16", cmp: "cmp_ri_16", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_ri_32", cmp: "cmp_ri_32", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_ri_64", cmp: "cmp_ri_64", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_le_ri_8", cmp: "cmp_ri_8", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_ri_16", cmp: "cmp_ri_16", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_ri_32", cmp: "cmp_ri_32", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_ri_64", cmp: "cmp_ri_64", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_g_ri_8", cmp: "cmp_ri_8", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_ri_16", cmp: "cmp_ri_16", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_ri_32", cmp: "cmp_ri_32", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_ri_64", cmp: "cmp_ri_64", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_ge_ri_8", cmp: "cmp_ri_8", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_ri_16", cmp: "cmp_ri_16", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_ri_32", cmp: "cmp_ri_32", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_ri_64", cmp: "cmp_ri_64", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_b_ri_8", cmp: "cmp_ri_8", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_ri_16", cmp: "cmp_ri_16", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_ri_32", cmp: "cmp_ri_32", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_ri_64", cmp: "cmp_ri_64", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_be_ri_8", cmp: "cmp_ri_8", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_ri_16", cmp: "cmp_ri_16", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_ri_32", cmp: "cmp_ri_32", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_ri_64", cmp: "cmp_ri_64", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_a_ri_8", cmp: "cmp_ri_8", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_ri_16", cmp: "cmp_ri_16", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_ri_32", cmp: "cmp_ri_32", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_ri_64", cmp: "cmp_ri_64", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_ae_ri_8", cmp: "cmp_ri_8", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_ri_16", cmp: "cmp_ri_16", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_ri_32", cmp: "cmp_ri_32", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_ri_64", cmp: "cmp_ri_64", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_e_rm_8", cmp: "cmp_rm_8", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_rm_16", cmp: "cmp_rm_16", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_rm_32", cmp: "cmp_rm_32", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_rm_64", cmp: "cmp_rm_64", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_ne_rm_8", cmp: "cmp_rm_8", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_rm_16", cmp: "cmp_rm_16", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_rm_32", cmp: "cmp_rm_32", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_rm_64", cmp: "cmp_rm_64", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_l_rm_8", cmp: "cmp_rm_8", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_rm_16", cmp: "cmp_rm_16", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_rm_32", cmp: "cmp_rm_32", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_rm_64", cmp: "cmp_rm_64", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_le_rm_8", cmp: "cmp_rm_8", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_rm_16", cmp: "cmp_rm_16", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_rm_32", cmp: "cmp_rm_32", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_rm_64", cmp: "cmp_rm_64", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_g_rm_8", cmp: "cmp_rm_8", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_rm_16", cmp: "cmp_rm_16", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_rm_32", cmp: "cmp_rm_32", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_rm_64", cmp: "cmp_rm_64", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_ge_rm_8", cmp: "cmp_rm_8", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_rm_16", cmp: "cmp_rm_16", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_rm_32", cmp: "cmp_rm_32", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_rm_64", cmp: "cmp_rm_64", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_b_rm_8", cmp: "cmp_rm_8", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_rm_16", cmp: "cmp_rm_16", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_rm_32", cmp: "cmp_rm_32", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_rm_64", cmp: "cmp_rm_64", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_be_rm_8", cmp: "cmp_rm_8", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_rm_16", cmp: "cmp_rm_16", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_rm_32", cmp: "cmp_rm_32", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_rm_64", cmp: "cmp_rm_64", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_a_rm_8", cmp: "cmp_rm_8", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_rm_16", cmp: "cmp_rm_16", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_rm_32", cmp: "cmp_rm_32", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_rm_64", cmp: "cmp_rm_64", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_ae_rm_8", cmp: "cmp_rm_8", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_rm_16", cmp: "cmp_rm_16", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_rm_32", cmp: "cmp_rm_32", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_rm_64", cmp: "cmp_rm_64", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_e_mi_8", cmp: "cmp_mi_8", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_mi_16", cmp: "cmp_mi_16", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_mi_32", cmp: "cmp_mi_32", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_e_mi_64", cmp: "cmp_mi_64", if_true: "jcc_e", if_false: "jcc_ne" },
    Fusion { set: "cmp_set_ne_mi_8", cmp: "cmp_mi_8", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_mi_16", cmp: "cmp_mi_16", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_mi_32", cmp: "cmp_mi_32", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_ne_mi_64", cmp: "cmp_mi_64", if_true: "jcc_ne", if_false: "jcc_e" },
    Fusion { set: "cmp_set_l_mi_8", cmp: "cmp_mi_8", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_mi_16", cmp: "cmp_mi_16", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_mi_32", cmp: "cmp_mi_32", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_l_mi_64", cmp: "cmp_mi_64", if_true: "jcc_l", if_false: "jcc_ge" },
    Fusion { set: "cmp_set_le_mi_8", cmp: "cmp_mi_8", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_mi_16", cmp: "cmp_mi_16", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_mi_32", cmp: "cmp_mi_32", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_le_mi_64", cmp: "cmp_mi_64", if_true: "jcc_le", if_false: "jcc_g" },
    Fusion { set: "cmp_set_g_mi_8", cmp: "cmp_mi_8", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_mi_16", cmp: "cmp_mi_16", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_mi_32", cmp: "cmp_mi_32", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_g_mi_64", cmp: "cmp_mi_64", if_true: "jcc_g", if_false: "jcc_le" },
    Fusion { set: "cmp_set_ge_mi_8", cmp: "cmp_mi_8", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_mi_16", cmp: "cmp_mi_16", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_mi_32", cmp: "cmp_mi_32", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_ge_mi_64", cmp: "cmp_mi_64", if_true: "jcc_ge", if_false: "jcc_l" },
    Fusion { set: "cmp_set_b_mi_8", cmp: "cmp_mi_8", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_mi_16", cmp: "cmp_mi_16", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_mi_32", cmp: "cmp_mi_32", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_b_mi_64", cmp: "cmp_mi_64", if_true: "jcc_b", if_false: "jcc_ae" },
    Fusion { set: "cmp_set_be_mi_8", cmp: "cmp_mi_8", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_mi_16", cmp: "cmp_mi_16", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_mi_32", cmp: "cmp_mi_32", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_be_mi_64", cmp: "cmp_mi_64", if_true: "jcc_be", if_false: "jcc_a" },
    Fusion { set: "cmp_set_a_mi_8", cmp: "cmp_mi_8", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_mi_16", cmp: "cmp_mi_16", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_mi_32", cmp: "cmp_mi_32", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_a_mi_64", cmp: "cmp_mi_64", if_true: "jcc_a", if_false: "jcc_be" },
    Fusion { set: "cmp_set_ae_mi_8", cmp: "cmp_mi_8", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_mi_16", cmp: "cmp_mi_16", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_mi_32", cmp: "cmp_mi_32", if_true: "jcc_ae", if_false: "jcc_b" },
    Fusion { set: "cmp_set_ae_mi_64", cmp: "cmp_mi_64", if_true: "jcc_ae", if_false: "jcc_b" },
];

/// What a select on a comparison's answer becomes, which is the move on the condition the
/// comparison was asked about.
///
/// The ten conditions at the four widths a select has. There is no conditional move narrower than
/// sixteen bits, so the eight bit select moves thirty two bits here for the reason its own rule
/// does.
static MOVES: [Move; 40] = [
    Move { select: "test_cmov_ne_8", when: "jcc_e", cmov: "cmov_e_32" },
    Move { select: "test_cmov_ne_8", when: "jcc_ne", cmov: "cmov_ne_32" },
    Move { select: "test_cmov_ne_8", when: "jcc_l", cmov: "cmov_l_32" },
    Move { select: "test_cmov_ne_8", when: "jcc_le", cmov: "cmov_le_32" },
    Move { select: "test_cmov_ne_8", when: "jcc_g", cmov: "cmov_g_32" },
    Move { select: "test_cmov_ne_8", when: "jcc_ge", cmov: "cmov_ge_32" },
    Move { select: "test_cmov_ne_8", when: "jcc_b", cmov: "cmov_b_32" },
    Move { select: "test_cmov_ne_8", when: "jcc_be", cmov: "cmov_be_32" },
    Move { select: "test_cmov_ne_8", when: "jcc_a", cmov: "cmov_a_32" },
    Move { select: "test_cmov_ne_8", when: "jcc_ae", cmov: "cmov_ae_32" },
    Move { select: "test_cmov_ne_16", when: "jcc_e", cmov: "cmov_e_16" },
    Move { select: "test_cmov_ne_16", when: "jcc_ne", cmov: "cmov_ne_16" },
    Move { select: "test_cmov_ne_16", when: "jcc_l", cmov: "cmov_l_16" },
    Move { select: "test_cmov_ne_16", when: "jcc_le", cmov: "cmov_le_16" },
    Move { select: "test_cmov_ne_16", when: "jcc_g", cmov: "cmov_g_16" },
    Move { select: "test_cmov_ne_16", when: "jcc_ge", cmov: "cmov_ge_16" },
    Move { select: "test_cmov_ne_16", when: "jcc_b", cmov: "cmov_b_16" },
    Move { select: "test_cmov_ne_16", when: "jcc_be", cmov: "cmov_be_16" },
    Move { select: "test_cmov_ne_16", when: "jcc_a", cmov: "cmov_a_16" },
    Move { select: "test_cmov_ne_16", when: "jcc_ae", cmov: "cmov_ae_16" },
    Move { select: "test_cmov_ne_32", when: "jcc_e", cmov: "cmov_e_32" },
    Move { select: "test_cmov_ne_32", when: "jcc_ne", cmov: "cmov_ne_32" },
    Move { select: "test_cmov_ne_32", when: "jcc_l", cmov: "cmov_l_32" },
    Move { select: "test_cmov_ne_32", when: "jcc_le", cmov: "cmov_le_32" },
    Move { select: "test_cmov_ne_32", when: "jcc_g", cmov: "cmov_g_32" },
    Move { select: "test_cmov_ne_32", when: "jcc_ge", cmov: "cmov_ge_32" },
    Move { select: "test_cmov_ne_32", when: "jcc_b", cmov: "cmov_b_32" },
    Move { select: "test_cmov_ne_32", when: "jcc_be", cmov: "cmov_be_32" },
    Move { select: "test_cmov_ne_32", when: "jcc_a", cmov: "cmov_a_32" },
    Move { select: "test_cmov_ne_32", when: "jcc_ae", cmov: "cmov_ae_32" },
    Move { select: "test_cmov_ne_64", when: "jcc_e", cmov: "cmov_e_64" },
    Move { select: "test_cmov_ne_64", when: "jcc_ne", cmov: "cmov_ne_64" },
    Move { select: "test_cmov_ne_64", when: "jcc_l", cmov: "cmov_l_64" },
    Move { select: "test_cmov_ne_64", when: "jcc_le", cmov: "cmov_le_64" },
    Move { select: "test_cmov_ne_64", when: "jcc_g", cmov: "cmov_g_64" },
    Move { select: "test_cmov_ne_64", when: "jcc_ge", cmov: "cmov_ge_64" },
    Move { select: "test_cmov_ne_64", when: "jcc_b", cmov: "cmov_b_64" },
    Move { select: "test_cmov_ne_64", when: "jcc_be", cmov: "cmov_be_64" },
    Move { select: "test_cmov_ne_64", when: "jcc_a", cmov: "cmov_a_64" },
    Move { select: "test_cmov_ne_64", when: "jcc_ae", cmov: "cmov_ae_64" },
];

/// What each x86-64 instruction leaves in the condition state.
///
/// The widths come out of the assembly description next door, the same way [`BITS`] takes them,
/// because two comparisons that agree about a register's low half and not about the rest of it
/// have not asked the same question.
pub static FLAGS: FlagInsts = FlagInsts {
    prefix: "x64.",
    width: operand_width,
    writes: writes_flags,
    compares: &COMPARES,
    compares_itself,
    readers: &READERS,
    zeroing: &ZEROING,
};

/// The shorter spellings x86-64 has for the same answer.
pub static SHORT: ShortInsts = ShortInsts {
    prefix: "x64.",
    zeroing: &SHORTER_ZEROS,
    narrowing: &NARROWER_MOVES,
    testing: &ZERO_COMPARES,
    stepping: &STEPS,
    copying: &BARE_ADDRESSES,
};

/// Writing zero into a register without spelling the zero out.
///
/// `movl $0, %eax` is five bytes, one for the opcode and four for a number every one of whose bits
/// is the same. `xorl %eax, %eax` is two and the processor knows the idiom, so it is shorter and no
/// slower. The same trade at sixteen bits is four bytes against three.
///
/// Sixty-four bits is written with the thirty-two bit exclusive or, which is where the number the
/// program asked for ends up either way: writing the low half of a register on this machine clears
/// the high half rather than leaving it alone, so `xorl %eax, %eax` is a sixty-four bit zero and is
/// two bytes where `xorq %rax, %rax` is three. That is the sentence [`NARROWER_MOVES`] is about as
/// well, and it is here rather than there because there is no number on this one to check.
///
/// Eight bits is not here and the reason is arithmetic rather than a rule: `movb $0, %al` is two
/// bytes and `xorb %al, %al` is two, so the exchange buys nothing and would cost the condition
/// state for it.
static SHORTER_ZEROS: [Zeroed; 3] = [
    Zeroed { name: "mov_ri_16", into: "xor_rr_16" },
    Zeroed { name: "mov_ri_32", into: "xor_rr_32" },
    Zeroed { name: "mov_ri_64", into: "xor_rr_32" },
];

/// Writing a number into a register with the instruction that writes half of it.
///
/// Because the other half is cleared rather than left alone, so a number that is not negative and
/// fits in thirty-two bits is in the register the same way whichever of the two put it there. The
/// thirty-two bit move is the shorter: it carries no prefix byte saying how wide it is, which is one
/// byte, and a number between two to the thirty-first and two to the thirty-second is written out
/// whole by the wide move because it cannot be reached by sign extending, which is another five.
///
/// Sixteen bits is not here for the reason eight bits is not above. `movw $7, %ax` is four bytes and
/// `movl $7, %eax` is five, so the narrower instruction is the longer one, which is what the prefix
/// byte in front of a sixteen bit instruction costs.
static NARROWER_MOVES: [Narrowed; 1] =
    [Narrowed { name: "mov_ri_64", into: "mov_ri_32", writes: 32 }];

/// Asking whether a register is zero without a zero written on the instruction.
///
/// `cmpl $0, %eax` is three bytes, one for the opcode, one saying which register and one for a
/// number that is nothing. `testl %eax, %eax` is two: it names the register twice and carries no
/// number at all. The saving is one byte at every width, since the constant a comparison against
/// zero carries is the one byte a constant can be written in.
///
/// The two say the same thing and not merely nearly the same thing. A comparison subtracts zero and
/// a test keeps the bits the register already has, so the sign, the zero and the parity are those of
/// what is in the register either way. Nothing is below zero when the comparison is unsigned and a
/// subtraction of zero cannot overflow, so the carry and the overflow are clear either way too.
/// Every condition this machine can jump on reads some of those five and no others, so every one of
/// them reads the same answer behind either instruction.
///
/// All four widths are here, eight bits included, which is where this differs from the two tables
/// above: the byte the constant costs is a byte at every width and there is no width where the
/// shorter instruction needs a prefix the longer one did not.
static ZERO_COMPARES: [Tested; 4] = [
    Tested { name: "cmp_ri_8", into: "test_rr_8" },
    Tested { name: "cmp_ri_16", into: "test_rr_16" },
    Tested { name: "cmp_ri_32", into: "test_rr_32" },
    Tested { name: "cmp_ri_64", into: "test_rr_64" },
];

/// Adding one and taking one away with the number in the opcode.
///
/// `addl $1, %eax` is three bytes: the opcode, the byte saying which register, and the number.
/// `incl %eax` is two, because the number is what the opcode means rather than something written
/// after it. One byte at every width, the same saving [`ZERO_COMPARES`] makes and for the same
/// reason, which is that the byte a small constant is written in is one byte however wide the
/// registers are.
///
/// Sixteen entries, four widths by four spellings of the same two numbers. An addition of one and a
/// subtraction of minus one both add one and both become the increment, and an addition of minus
/// one and a subtraction of one both take one away and both become the decrement. All four turn up:
/// a loop counting down is written either way round in C and the middle end does not put them in a
/// normal form, since at the level above this the two are the same number of instructions.
///
/// This is the one table here whose two sides do not leave the same condition state. The addition
/// writes the carry and the increment leaves it alone, so what the pass has to find is that nothing
/// reads a carry between the instruction and the next thing that writes one. It is also the one
/// whose saving is not free: leaving part of the state alone means the next instruction to write
/// the state has to merge with what was left, which the machine does and does not do for nothing.
/// gcc writes the addition at `-O2` and the increment at `-Os`, and this compiler does the same.
static STEPS: [Stepped; 16] = [
    Stepped { name: "add_ri_8", by: 1, into: "inc_r_8" },
    Stepped { name: "add_ri_16", by: 1, into: "inc_r_16" },
    Stepped { name: "add_ri_32", by: 1, into: "inc_r_32" },
    Stepped { name: "add_ri_64", by: 1, into: "inc_r_64" },
    Stepped { name: "add_ri_8", by: -1, into: "dec_r_8" },
    Stepped { name: "add_ri_16", by: -1, into: "dec_r_16" },
    Stepped { name: "add_ri_32", by: -1, into: "dec_r_32" },
    Stepped { name: "add_ri_64", by: -1, into: "dec_r_64" },
    Stepped { name: "sub_ri_8", by: -1, into: "inc_r_8" },
    Stepped { name: "sub_ri_16", by: -1, into: "inc_r_16" },
    Stepped { name: "sub_ri_32", by: -1, into: "inc_r_32" },
    Stepped { name: "sub_ri_64", by: -1, into: "inc_r_64" },
    Stepped { name: "sub_ri_8", by: 1, into: "dec_r_8" },
    Stepped { name: "sub_ri_16", by: 1, into: "dec_r_16" },
    Stepped { name: "sub_ri_32", by: 1, into: "dec_r_32" },
    Stepped { name: "sub_ri_64", by: 1, into: "dec_r_64" },
];

/// An address that is a register, written as a move rather than as an address computation.
///
/// `leaq (%rsp), %rax` works out an address that is a base register, no index and nothing added,
/// which is the register. `movq %rsp, %rax` puts the same number in the same place. There is one
/// entry because there is one instruction here that works an address out and keeps it, and what
/// decides whether the two say the same thing is the addressing mode rather than the width or the
/// number, so nothing here is written out per width the way the tables above are.
///
/// The byte is the addressing mode's rather than the opcode's. An address counted from the stack
/// pointer needs the byte that says there is no index, which nothing else here needs and which the
/// move does not, so this is four bytes against three every time the base is that register. It is
/// also the only rewrite in this file that is worth taking for a reason other than bytes, since the
/// machine can do a move between registers by renaming and has to do an address computation with an
/// adder. gcc writes no such address computation anywhere in the SQLite amalgamation.
static BARE_ADDRESSES: [Copied; 1] = [Copied { name: "lea_64", into: "mov_rr_64" }];

/// Whether the instruction of that name leaves the condition state other than it found it.
///
/// Written as the ones that do not, because that is the list that can be checked against the
/// machine: a move, an address computation, a load, a store, a conversion between widths, a
/// constant into a register, a push, a pop, a `setcc` and a `cmovcc` are the instructions Intel's
/// description of each says nothing about the flags in, and the vector unit's arithmetic writes its
/// own status word rather than this one. A byte reversal is on the list for the same reason and is
/// the only one on it that computes something: it is the one instruction on this machine that takes
/// a register apart and puts it back and still says nothing about the state. The last two read the state and leave it alone, which is
/// what puts them in this list and in the one below it both. An alignment is here for a reason none
/// of the others is: it is not an instruction, so there is nothing for it to have done to the state.
/// Everything else writes them, and so does every name this target does
/// not have, which is what keeps a rule set that grows an opcode from quietly growing a wrong
/// answer here.
#[must_use]
/// Whether the instruction of that name compares and keeps the answer in a byte, whatever its
/// operands are.
///
/// The comparisons against memory are in here and not in [`COMPARES`]. A pass taking out a
/// comparison that was already made cannot tell that memory still holds what it held, so it is
/// not given them, but a pass asking whether the state arriving at one is read needs to know that
/// it is not, and before this every function with one in it lost every shorter zero.
fn compares_itself(name: &str) -> bool {
    matches!(form(name), Some(Form::CmpSet | Form::CmpSetRi | Form::CmpSetRm | Form::CmpSetMi))
}

fn writes_flags(name: &str) -> bool {
    let Some(shape) = form(name) else { return true };
    !matches!(
        shape,
        Form::Move
            | Form::Convert
            | Form::Lea
            | Form::Load
            | Form::Store
            | Form::StoreImm
            | Form::LoadImm
            | Form::Push
            | Form::Pop
            | Form::Set
            | Form::Cmov
            | Form::Jcc
            | Form::Jmp
            | Form::Nop
            | Form::Align
            | Form::Literal
            | Form::Landing
            | Form::Swap
            | Form::SwapHalves
            | Form::Prefetch
            | Form::StrMove
            | Form::StrMoveRep
            | Form::StrStore
            | Form::StrStoreRep
            | Form::StrLoad
            | Form::RetVal
            | Form::RetVal2
            | Form::ArgVal
            | Form::BrCond
            | Form::MoveVec
            | Form::LoadVec
            | Form::StoreVec
            | Form::AluVec
            | Form::ConvertVec
            | Form::ConvertToVec
            | Form::ConvertFromVec
            | Form::RetValVec
            | Form::RetVal2Vec
            | Form::ArgValVec
    )
}

/// Every comparison this machine makes, and what is left of one it has already made.
///
/// Ten conditions at four widths against a register and against a constant, which is eighty, and
/// then the eight that keep no answer. What a comparison asks is the name of the one that keeps
/// nothing, so the eighty and the eight meet in the middle: a program that compares and keeps the
/// byte, and then compares the same two registers and branches, is two rows here that agree.
///
/// The eighty comparisons against memory are not here and are meant not to be, which is the one
/// place this table is shorter than [`FUSED`]. What makes two rows the same question is the name
/// on the left and the registers and constants the two instructions read, and a comparison against
/// memory does not read the side it takes from memory out of a register: the address is the
/// operands it has and two addresses off one base at two displacements would come out equal.
/// Memory can also have changed between the two, which no amount of comparing operands would say.
/// Leaving them out costs a saving that is not taken and keeps the pass from taking one that is
/// not there.
static COMPARES: [Compare; 88] = [
    Compare { name: "cmp_set_e_8", asks: "cmp_rr_8", kept: Some("set_e") },
    Compare { name: "cmp_set_e_16", asks: "cmp_rr_16", kept: Some("set_e") },
    Compare { name: "cmp_set_e_32", asks: "cmp_rr_32", kept: Some("set_e") },
    Compare { name: "cmp_set_e_64", asks: "cmp_rr_64", kept: Some("set_e") },
    Compare { name: "cmp_set_ne_8", asks: "cmp_rr_8", kept: Some("set_ne") },
    Compare { name: "cmp_set_ne_16", asks: "cmp_rr_16", kept: Some("set_ne") },
    Compare { name: "cmp_set_ne_32", asks: "cmp_rr_32", kept: Some("set_ne") },
    Compare { name: "cmp_set_ne_64", asks: "cmp_rr_64", kept: Some("set_ne") },
    Compare { name: "cmp_set_l_8", asks: "cmp_rr_8", kept: Some("set_l") },
    Compare { name: "cmp_set_l_16", asks: "cmp_rr_16", kept: Some("set_l") },
    Compare { name: "cmp_set_l_32", asks: "cmp_rr_32", kept: Some("set_l") },
    Compare { name: "cmp_set_l_64", asks: "cmp_rr_64", kept: Some("set_l") },
    Compare { name: "cmp_set_le_8", asks: "cmp_rr_8", kept: Some("set_le") },
    Compare { name: "cmp_set_le_16", asks: "cmp_rr_16", kept: Some("set_le") },
    Compare { name: "cmp_set_le_32", asks: "cmp_rr_32", kept: Some("set_le") },
    Compare { name: "cmp_set_le_64", asks: "cmp_rr_64", kept: Some("set_le") },
    Compare { name: "cmp_set_g_8", asks: "cmp_rr_8", kept: Some("set_g") },
    Compare { name: "cmp_set_g_16", asks: "cmp_rr_16", kept: Some("set_g") },
    Compare { name: "cmp_set_g_32", asks: "cmp_rr_32", kept: Some("set_g") },
    Compare { name: "cmp_set_g_64", asks: "cmp_rr_64", kept: Some("set_g") },
    Compare { name: "cmp_set_ge_8", asks: "cmp_rr_8", kept: Some("set_ge") },
    Compare { name: "cmp_set_ge_16", asks: "cmp_rr_16", kept: Some("set_ge") },
    Compare { name: "cmp_set_ge_32", asks: "cmp_rr_32", kept: Some("set_ge") },
    Compare { name: "cmp_set_ge_64", asks: "cmp_rr_64", kept: Some("set_ge") },
    Compare { name: "cmp_set_b_8", asks: "cmp_rr_8", kept: Some("set_b") },
    Compare { name: "cmp_set_b_16", asks: "cmp_rr_16", kept: Some("set_b") },
    Compare { name: "cmp_set_b_32", asks: "cmp_rr_32", kept: Some("set_b") },
    Compare { name: "cmp_set_b_64", asks: "cmp_rr_64", kept: Some("set_b") },
    Compare { name: "cmp_set_be_8", asks: "cmp_rr_8", kept: Some("set_be") },
    Compare { name: "cmp_set_be_16", asks: "cmp_rr_16", kept: Some("set_be") },
    Compare { name: "cmp_set_be_32", asks: "cmp_rr_32", kept: Some("set_be") },
    Compare { name: "cmp_set_be_64", asks: "cmp_rr_64", kept: Some("set_be") },
    Compare { name: "cmp_set_a_8", asks: "cmp_rr_8", kept: Some("set_a") },
    Compare { name: "cmp_set_a_16", asks: "cmp_rr_16", kept: Some("set_a") },
    Compare { name: "cmp_set_a_32", asks: "cmp_rr_32", kept: Some("set_a") },
    Compare { name: "cmp_set_a_64", asks: "cmp_rr_64", kept: Some("set_a") },
    Compare { name: "cmp_set_ae_8", asks: "cmp_rr_8", kept: Some("set_ae") },
    Compare { name: "cmp_set_ae_16", asks: "cmp_rr_16", kept: Some("set_ae") },
    Compare { name: "cmp_set_ae_32", asks: "cmp_rr_32", kept: Some("set_ae") },
    Compare { name: "cmp_set_ae_64", asks: "cmp_rr_64", kept: Some("set_ae") },
    Compare { name: "cmp_set_e_ri_8", asks: "cmp_ri_8", kept: Some("set_e") },
    Compare { name: "cmp_set_e_ri_16", asks: "cmp_ri_16", kept: Some("set_e") },
    Compare { name: "cmp_set_e_ri_32", asks: "cmp_ri_32", kept: Some("set_e") },
    Compare { name: "cmp_set_e_ri_64", asks: "cmp_ri_64", kept: Some("set_e") },
    Compare { name: "cmp_set_ne_ri_8", asks: "cmp_ri_8", kept: Some("set_ne") },
    Compare { name: "cmp_set_ne_ri_16", asks: "cmp_ri_16", kept: Some("set_ne") },
    Compare { name: "cmp_set_ne_ri_32", asks: "cmp_ri_32", kept: Some("set_ne") },
    Compare { name: "cmp_set_ne_ri_64", asks: "cmp_ri_64", kept: Some("set_ne") },
    Compare { name: "cmp_set_l_ri_8", asks: "cmp_ri_8", kept: Some("set_l") },
    Compare { name: "cmp_set_l_ri_16", asks: "cmp_ri_16", kept: Some("set_l") },
    Compare { name: "cmp_set_l_ri_32", asks: "cmp_ri_32", kept: Some("set_l") },
    Compare { name: "cmp_set_l_ri_64", asks: "cmp_ri_64", kept: Some("set_l") },
    Compare { name: "cmp_set_le_ri_8", asks: "cmp_ri_8", kept: Some("set_le") },
    Compare { name: "cmp_set_le_ri_16", asks: "cmp_ri_16", kept: Some("set_le") },
    Compare { name: "cmp_set_le_ri_32", asks: "cmp_ri_32", kept: Some("set_le") },
    Compare { name: "cmp_set_le_ri_64", asks: "cmp_ri_64", kept: Some("set_le") },
    Compare { name: "cmp_set_g_ri_8", asks: "cmp_ri_8", kept: Some("set_g") },
    Compare { name: "cmp_set_g_ri_16", asks: "cmp_ri_16", kept: Some("set_g") },
    Compare { name: "cmp_set_g_ri_32", asks: "cmp_ri_32", kept: Some("set_g") },
    Compare { name: "cmp_set_g_ri_64", asks: "cmp_ri_64", kept: Some("set_g") },
    Compare { name: "cmp_set_ge_ri_8", asks: "cmp_ri_8", kept: Some("set_ge") },
    Compare { name: "cmp_set_ge_ri_16", asks: "cmp_ri_16", kept: Some("set_ge") },
    Compare { name: "cmp_set_ge_ri_32", asks: "cmp_ri_32", kept: Some("set_ge") },
    Compare { name: "cmp_set_ge_ri_64", asks: "cmp_ri_64", kept: Some("set_ge") },
    Compare { name: "cmp_set_b_ri_8", asks: "cmp_ri_8", kept: Some("set_b") },
    Compare { name: "cmp_set_b_ri_16", asks: "cmp_ri_16", kept: Some("set_b") },
    Compare { name: "cmp_set_b_ri_32", asks: "cmp_ri_32", kept: Some("set_b") },
    Compare { name: "cmp_set_b_ri_64", asks: "cmp_ri_64", kept: Some("set_b") },
    Compare { name: "cmp_set_be_ri_8", asks: "cmp_ri_8", kept: Some("set_be") },
    Compare { name: "cmp_set_be_ri_16", asks: "cmp_ri_16", kept: Some("set_be") },
    Compare { name: "cmp_set_be_ri_32", asks: "cmp_ri_32", kept: Some("set_be") },
    Compare { name: "cmp_set_be_ri_64", asks: "cmp_ri_64", kept: Some("set_be") },
    Compare { name: "cmp_set_a_ri_8", asks: "cmp_ri_8", kept: Some("set_a") },
    Compare { name: "cmp_set_a_ri_16", asks: "cmp_ri_16", kept: Some("set_a") },
    Compare { name: "cmp_set_a_ri_32", asks: "cmp_ri_32", kept: Some("set_a") },
    Compare { name: "cmp_set_a_ri_64", asks: "cmp_ri_64", kept: Some("set_a") },
    Compare { name: "cmp_set_ae_ri_8", asks: "cmp_ri_8", kept: Some("set_ae") },
    Compare { name: "cmp_set_ae_ri_16", asks: "cmp_ri_16", kept: Some("set_ae") },
    Compare { name: "cmp_set_ae_ri_32", asks: "cmp_ri_32", kept: Some("set_ae") },
    Compare { name: "cmp_set_ae_ri_64", asks: "cmp_ri_64", kept: Some("set_ae") },
    Compare { name: "cmp_rr_8", asks: "cmp_rr_8", kept: None },
    Compare { name: "cmp_rr_16", asks: "cmp_rr_16", kept: None },
    Compare { name: "cmp_rr_32", asks: "cmp_rr_32", kept: None },
    Compare { name: "cmp_rr_64", asks: "cmp_rr_64", kept: None },
    Compare { name: "cmp_ri_8", asks: "cmp_ri_8", kept: None },
    Compare { name: "cmp_ri_16", asks: "cmp_ri_16", kept: None },
    Compare { name: "cmp_ri_32", asks: "cmp_ri_32", kept: None },
    Compare { name: "cmp_ri_64", asks: "cmp_ri_64", kept: None },
];

/// Every instruction that reads the condition state, and which part of it each names.
///
/// A comparison that keeps a byte is in here as well as above, because the condition on the front
/// of it is a condition whoever set the bits it reads. The `setcc` with no comparison, the thirty
/// conditional moves with no comparison and the ten jumps are the rest. The two that are about the
/// carry and the zero together are filed under the carry, since an entry says which part has to be
/// right and both of theirs do.
///
/// The add with carry and the subtract with borrow are first, in the order the description next
/// door lists them, and they are the eight rows here that carry no condition in their names. What
/// they read is [`Reads::Carry`], which is the group nothing in [`ZEROING`] is good for, so they
/// can only stop the pass and never point it at the wrong bits.
static READERS: [Reader; 232] = [
    Reader { name: "adc_rr_8", reads: Reads::Carry },
    Reader { name: "adc_rr_16", reads: Reads::Carry },
    Reader { name: "adc_rr_32", reads: Reads::Carry },
    Reader { name: "adc_rr_64", reads: Reads::Carry },
    Reader { name: "sbb_rr_8", reads: Reads::Carry },
    Reader { name: "sbb_rr_16", reads: Reads::Carry },
    Reader { name: "sbb_rr_32", reads: Reads::Carry },
    Reader { name: "sbb_rr_64", reads: Reads::Carry },
    Reader { name: "adc_ri_8", reads: Reads::Carry },
    Reader { name: "adc_ri_16", reads: Reads::Carry },
    Reader { name: "adc_ri_32", reads: Reads::Carry },
    Reader { name: "adc_ri_64", reads: Reads::Carry },
    Reader { name: "sbb_ri_8", reads: Reads::Carry },
    Reader { name: "sbb_ri_16", reads: Reads::Carry },
    Reader { name: "sbb_ri_32", reads: Reads::Carry },
    Reader { name: "sbb_ri_64", reads: Reads::Carry },
    Reader { name: "cmp_set_e_8", reads: Reads::Zero },
    Reader { name: "cmp_set_e_16", reads: Reads::Zero },
    Reader { name: "cmp_set_e_32", reads: Reads::Zero },
    Reader { name: "cmp_set_e_64", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_8", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_16", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_32", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_64", reads: Reads::Zero },
    Reader { name: "cmp_set_l_8", reads: Reads::Signed },
    Reader { name: "cmp_set_l_16", reads: Reads::Signed },
    Reader { name: "cmp_set_l_32", reads: Reads::Signed },
    Reader { name: "cmp_set_l_64", reads: Reads::Signed },
    Reader { name: "cmp_set_le_8", reads: Reads::Signed },
    Reader { name: "cmp_set_le_16", reads: Reads::Signed },
    Reader { name: "cmp_set_le_32", reads: Reads::Signed },
    Reader { name: "cmp_set_le_64", reads: Reads::Signed },
    Reader { name: "cmp_set_g_8", reads: Reads::Signed },
    Reader { name: "cmp_set_g_16", reads: Reads::Signed },
    Reader { name: "cmp_set_g_32", reads: Reads::Signed },
    Reader { name: "cmp_set_g_64", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_8", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_16", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_32", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_64", reads: Reads::Signed },
    Reader { name: "cmp_set_b_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_e_ri_8", reads: Reads::Zero },
    Reader { name: "cmp_set_e_ri_16", reads: Reads::Zero },
    Reader { name: "cmp_set_e_ri_32", reads: Reads::Zero },
    Reader { name: "cmp_set_e_ri_64", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_ri_8", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_ri_16", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_ri_32", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_ri_64", reads: Reads::Zero },
    Reader { name: "cmp_set_l_ri_8", reads: Reads::Signed },
    Reader { name: "cmp_set_l_ri_16", reads: Reads::Signed },
    Reader { name: "cmp_set_l_ri_32", reads: Reads::Signed },
    Reader { name: "cmp_set_l_ri_64", reads: Reads::Signed },
    Reader { name: "cmp_set_le_ri_8", reads: Reads::Signed },
    Reader { name: "cmp_set_le_ri_16", reads: Reads::Signed },
    Reader { name: "cmp_set_le_ri_32", reads: Reads::Signed },
    Reader { name: "cmp_set_le_ri_64", reads: Reads::Signed },
    Reader { name: "cmp_set_g_ri_8", reads: Reads::Signed },
    Reader { name: "cmp_set_g_ri_16", reads: Reads::Signed },
    Reader { name: "cmp_set_g_ri_32", reads: Reads::Signed },
    Reader { name: "cmp_set_g_ri_64", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_ri_8", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_ri_16", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_ri_32", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_ri_64", reads: Reads::Signed },
    Reader { name: "cmp_set_b_ri_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_ri_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_ri_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_ri_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_ri_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_ri_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_ri_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_ri_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_ri_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_ri_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_ri_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_ri_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_ri_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_ri_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_ri_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_ri_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_e_rm_8", reads: Reads::Zero },
    Reader { name: "cmp_set_e_rm_16", reads: Reads::Zero },
    Reader { name: "cmp_set_e_rm_32", reads: Reads::Zero },
    Reader { name: "cmp_set_e_rm_64", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_rm_8", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_rm_16", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_rm_32", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_rm_64", reads: Reads::Zero },
    Reader { name: "cmp_set_l_rm_8", reads: Reads::Signed },
    Reader { name: "cmp_set_l_rm_16", reads: Reads::Signed },
    Reader { name: "cmp_set_l_rm_32", reads: Reads::Signed },
    Reader { name: "cmp_set_l_rm_64", reads: Reads::Signed },
    Reader { name: "cmp_set_le_rm_8", reads: Reads::Signed },
    Reader { name: "cmp_set_le_rm_16", reads: Reads::Signed },
    Reader { name: "cmp_set_le_rm_32", reads: Reads::Signed },
    Reader { name: "cmp_set_le_rm_64", reads: Reads::Signed },
    Reader { name: "cmp_set_g_rm_8", reads: Reads::Signed },
    Reader { name: "cmp_set_g_rm_16", reads: Reads::Signed },
    Reader { name: "cmp_set_g_rm_32", reads: Reads::Signed },
    Reader { name: "cmp_set_g_rm_64", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_rm_8", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_rm_16", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_rm_32", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_rm_64", reads: Reads::Signed },
    Reader { name: "cmp_set_b_rm_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_rm_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_rm_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_rm_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_rm_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_rm_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_rm_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_rm_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_rm_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_rm_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_rm_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_rm_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_rm_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_rm_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_rm_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_rm_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_e_mi_8", reads: Reads::Zero },
    Reader { name: "cmp_set_e_mi_16", reads: Reads::Zero },
    Reader { name: "cmp_set_e_mi_32", reads: Reads::Zero },
    Reader { name: "cmp_set_e_mi_64", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_mi_8", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_mi_16", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_mi_32", reads: Reads::Zero },
    Reader { name: "cmp_set_ne_mi_64", reads: Reads::Zero },
    Reader { name: "cmp_set_l_mi_8", reads: Reads::Signed },
    Reader { name: "cmp_set_l_mi_16", reads: Reads::Signed },
    Reader { name: "cmp_set_l_mi_32", reads: Reads::Signed },
    Reader { name: "cmp_set_l_mi_64", reads: Reads::Signed },
    Reader { name: "cmp_set_le_mi_8", reads: Reads::Signed },
    Reader { name: "cmp_set_le_mi_16", reads: Reads::Signed },
    Reader { name: "cmp_set_le_mi_32", reads: Reads::Signed },
    Reader { name: "cmp_set_le_mi_64", reads: Reads::Signed },
    Reader { name: "cmp_set_g_mi_8", reads: Reads::Signed },
    Reader { name: "cmp_set_g_mi_16", reads: Reads::Signed },
    Reader { name: "cmp_set_g_mi_32", reads: Reads::Signed },
    Reader { name: "cmp_set_g_mi_64", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_mi_8", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_mi_16", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_mi_32", reads: Reads::Signed },
    Reader { name: "cmp_set_ge_mi_64", reads: Reads::Signed },
    Reader { name: "cmp_set_b_mi_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_mi_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_mi_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_b_mi_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_mi_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_mi_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_mi_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_be_mi_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_mi_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_mi_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_mi_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_a_mi_64", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_mi_8", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_mi_16", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_mi_32", reads: Reads::Unsigned },
    Reader { name: "cmp_set_ae_mi_64", reads: Reads::Unsigned },
    Reader { name: "set_e", reads: Reads::Zero },
    Reader { name: "set_ne", reads: Reads::Zero },
    Reader { name: "set_l", reads: Reads::Signed },
    Reader { name: "set_le", reads: Reads::Signed },
    Reader { name: "set_g", reads: Reads::Signed },
    Reader { name: "set_ge", reads: Reads::Signed },
    Reader { name: "set_b", reads: Reads::Unsigned },
    Reader { name: "set_be", reads: Reads::Unsigned },
    Reader { name: "set_a", reads: Reads::Unsigned },
    Reader { name: "set_ae", reads: Reads::Unsigned },
    Reader { name: "cmov_e_16", reads: Reads::Zero },
    Reader { name: "cmov_e_32", reads: Reads::Zero },
    Reader { name: "cmov_e_64", reads: Reads::Zero },
    Reader { name: "cmov_ne_16", reads: Reads::Zero },
    Reader { name: "cmov_ne_32", reads: Reads::Zero },
    Reader { name: "cmov_ne_64", reads: Reads::Zero },
    Reader { name: "cmov_l_16", reads: Reads::Signed },
    Reader { name: "cmov_l_32", reads: Reads::Signed },
    Reader { name: "cmov_l_64", reads: Reads::Signed },
    Reader { name: "cmov_le_16", reads: Reads::Signed },
    Reader { name: "cmov_le_32", reads: Reads::Signed },
    Reader { name: "cmov_le_64", reads: Reads::Signed },
    Reader { name: "cmov_g_16", reads: Reads::Signed },
    Reader { name: "cmov_g_32", reads: Reads::Signed },
    Reader { name: "cmov_g_64", reads: Reads::Signed },
    Reader { name: "cmov_ge_16", reads: Reads::Signed },
    Reader { name: "cmov_ge_32", reads: Reads::Signed },
    Reader { name: "cmov_ge_64", reads: Reads::Signed },
    Reader { name: "cmov_b_16", reads: Reads::Unsigned },
    Reader { name: "cmov_b_32", reads: Reads::Unsigned },
    Reader { name: "cmov_b_64", reads: Reads::Unsigned },
    Reader { name: "cmov_be_16", reads: Reads::Unsigned },
    Reader { name: "cmov_be_32", reads: Reads::Unsigned },
    Reader { name: "cmov_be_64", reads: Reads::Unsigned },
    Reader { name: "cmov_a_16", reads: Reads::Unsigned },
    Reader { name: "cmov_a_32", reads: Reads::Unsigned },
    Reader { name: "cmov_a_64", reads: Reads::Unsigned },
    Reader { name: "cmov_ae_16", reads: Reads::Unsigned },
    Reader { name: "cmov_ae_32", reads: Reads::Unsigned },
    Reader { name: "cmov_ae_64", reads: Reads::Unsigned },
    Reader { name: "jcc_e", reads: Reads::Zero },
    Reader { name: "jcc_ne", reads: Reads::Zero },
    Reader { name: "jcc_l", reads: Reads::Signed },
    Reader { name: "jcc_le", reads: Reads::Signed },
    Reader { name: "jcc_g", reads: Reads::Signed },
    Reader { name: "jcc_ge", reads: Reads::Signed },
    Reader { name: "jcc_b", reads: Reads::Unsigned },
    Reader { name: "jcc_be", reads: Reads::Unsigned },
    Reader { name: "jcc_a", reads: Reads::Unsigned },
    Reader { name: "jcc_ae", reads: Reads::Unsigned },
    Reader { name: "jcc_s", reads: Reads::Bit },
    Reader { name: "jcc_ns", reads: Reads::Bit },
    Reader { name: "jcc_o", reads: Reads::Bit },
    Reader { name: "jcc_no", reads: Reads::Bit },
    Reader { name: "jcc_p", reads: Reads::Bit },
    Reader { name: "jcc_np", reads: Reads::Bit },
];

/// Every instruction that leaves behind what a comparison of what it wrote against zero leaves.
///
/// The three bitwise operations clear the carry and the overflow and set the zero and the sign
/// from the result, which is every bit a comparison against zero would have set and the same
/// values, so every condition reads them and gets the right answer. The additions, the
/// subtractions and the negation set the carry and the overflow from what really happened, which
/// is not what comparing the answer against zero would have said, so only the conditions about the
/// zero may read them.
static ZEROING: [Zeroing; 44] = [
    Zeroing { name: "and_rr_8", signed: true, unsigned: true },
    Zeroing { name: "and_rr_16", signed: true, unsigned: true },
    Zeroing { name: "and_rr_32", signed: true, unsigned: true },
    Zeroing { name: "and_rr_64", signed: true, unsigned: true },
    Zeroing { name: "and_ri_8", signed: true, unsigned: true },
    Zeroing { name: "and_ri_16", signed: true, unsigned: true },
    Zeroing { name: "and_ri_32", signed: true, unsigned: true },
    Zeroing { name: "and_ri_64", signed: true, unsigned: true },
    Zeroing { name: "or_rr_8", signed: true, unsigned: true },
    Zeroing { name: "or_rr_16", signed: true, unsigned: true },
    Zeroing { name: "or_rr_32", signed: true, unsigned: true },
    Zeroing { name: "or_rr_64", signed: true, unsigned: true },
    Zeroing { name: "or_ri_8", signed: true, unsigned: true },
    Zeroing { name: "or_ri_16", signed: true, unsigned: true },
    Zeroing { name: "or_ri_32", signed: true, unsigned: true },
    Zeroing { name: "or_ri_64", signed: true, unsigned: true },
    Zeroing { name: "xor_rr_8", signed: true, unsigned: true },
    Zeroing { name: "xor_rr_16", signed: true, unsigned: true },
    Zeroing { name: "xor_rr_32", signed: true, unsigned: true },
    Zeroing { name: "xor_rr_64", signed: true, unsigned: true },
    Zeroing { name: "xor_ri_8", signed: true, unsigned: true },
    Zeroing { name: "xor_ri_16", signed: true, unsigned: true },
    Zeroing { name: "xor_ri_32", signed: true, unsigned: true },
    Zeroing { name: "xor_ri_64", signed: true, unsigned: true },
    Zeroing { name: "add_rr_8", signed: false, unsigned: false },
    Zeroing { name: "add_rr_16", signed: false, unsigned: false },
    Zeroing { name: "add_rr_32", signed: false, unsigned: false },
    Zeroing { name: "add_rr_64", signed: false, unsigned: false },
    Zeroing { name: "add_ri_8", signed: false, unsigned: false },
    Zeroing { name: "add_ri_16", signed: false, unsigned: false },
    Zeroing { name: "add_ri_32", signed: false, unsigned: false },
    Zeroing { name: "add_ri_64", signed: false, unsigned: false },
    Zeroing { name: "sub_rr_8", signed: false, unsigned: false },
    Zeroing { name: "sub_rr_16", signed: false, unsigned: false },
    Zeroing { name: "sub_rr_32", signed: false, unsigned: false },
    Zeroing { name: "sub_rr_64", signed: false, unsigned: false },
    Zeroing { name: "sub_ri_8", signed: false, unsigned: false },
    Zeroing { name: "sub_ri_16", signed: false, unsigned: false },
    Zeroing { name: "sub_ri_32", signed: false, unsigned: false },
    Zeroing { name: "sub_ri_64", signed: false, unsigned: false },
    Zeroing { name: "neg_r_8", signed: false, unsigned: false },
    Zeroing { name: "neg_r_16", signed: false, unsigned: false },
    Zeroing { name: "neg_r_32", signed: false, unsigned: false },
    Zeroing { name: "neg_r_64", signed: false, unsigned: false },
];

static SYSV_INT_ARGS: [PhysReg; 6] = [RDI, RSI, RDX, RCX, R8, R9];
static SYSV_SSE_ARGS: [PhysReg; 8] =
    [xmm(0), xmm(1), xmm(2), xmm(3), xmm(4), xmm(5), xmm(6), xmm(7)];
static SYSV_INT_RETURNS: [PhysReg; 2] = [RAX, RDX];
static SYSV_SSE_RETURNS: [PhysReg; 2] = [xmm(0), xmm(1)];
// A `long double` comes back in `st0`, and a `_Complex long double` in `st0` and `st1`, which
// is the one return value on this target that is in neither of the other two files.
static SYSV_X87_RETURNS: [PhysReg; 2] = [st(0), st(1)];
static SYSV_INT_SAVED: [PhysReg; 6] = [RBX, RBP, R12, R13, R14, R15];
static SYSV_SSE_SAVED: [PhysReg; 0] = [];
// The nine a call may destroy first, then the five it may not. A value that dies before the
// next call should not be occupying a register somebody has to push to use.
static SYSV_INT_ORDER: [PhysReg; 14] =
    [RAX, RCX, RDX, RSI, RDI, R8, R9, R10, R11, RBX, R12, R13, R14, R15];
// In number order, which both conventions are happy with: SysV preserves none of them, and
// Windows preserves the upper ten, so counting up hands out the ones a call destroys first on
// the target where that is a distinction.
static SSE_ORDER: [PhysReg; 16] = [
    xmm(0),
    xmm(1),
    xmm(2),
    xmm(3),
    xmm(4),
    xmm(5),
    xmm(6),
    xmm(7),
    xmm(8),
    xmm(9),
    xmm(10),
    xmm(11),
    xmm(12),
    xmm(13),
    xmm(14),
    xmm(15),
];

/// Where a SysV AMD64 call puts things, per `spec/12-abi-and-runtime.md` section 12.2.
pub static SYSV: CallRegs = CallRegs {
    abi: &rucc_abi::abis::SYSV_AMD64,
    int_class: GPR,
    sse_class: XMM,
    int_args: &SYSV_INT_ARGS,
    sse_args: &SYSV_SSE_ARGS,
    shared_positions: false,
    int_returns: &SYSV_INT_RETURNS,
    sse_returns: &SYSV_SSE_RETURNS,
    x87_returns: &SYSV_X87_RETURNS,
    int_saved: &SYSV_INT_SAVED,
    sse_saved: &SYSV_SSE_SAVED,
    int_order: &SYSV_INT_ORDER,
    sse_order: &SSE_ORDER,
    stack_pointer: RSP,
    frame_pointer: RBP,
    // The pointer goes up before the frame does here, which is the order every debugger and every
    // profiler on this platform expects: the word the pointer names is the caller's copy of it, so
    // the frames are a chain that can be walked without a table.
    late_frame_pointer: false,
    vector_count: Some(RAX),
    red_zone: 128,
    shadow: 0,
    stack_align: 16,
    return_address: 8,
    word: 8,
    dwarf: &X86_64_DWARF,
    dwarf_return_address: DWARF_RETURN_ADDRESS,
    // Forty bytes into the block a thread has to itself, which is where glibc, musl and every
    // other libc on this platform put it, because the psABI's thread control block says so and
    // gcc has emitted `%fs:40` for twenty years. There is no symbol for it: glibc keeps
    // `__stack_chk_guard` out of its dynamic symbol table on this target precisely so that a
    // protected function cannot be talked into reading somebody else's copy.
    guard: Some(Guard { segment: Segment::Fs, at: 40, fail: "__stack_chk_fail" }),
    // The newer hook by default, which is what gcc has done on this platform for years and what
    // every kernel needs. `mcount` is the older one and it reads the frame pointer, so a function
    // that calls it is given one whatever the rest of the command line said.
    trace: Some(Trace { early: "__fentry__", late: "mcount", fentry: true }),
    // None, and it is the platform saying so rather than a gap. A stack on this platform grows by
    // faulting: a write anywhere below the stack pointer is a page the kernel maps on the spot, in
    // any order, so a frame taken in one step is a frame that works. The one page that is not like
    // that is the guard the kernel leaves below every stack, which is what
    // `-fstack-clash-protection` is about, and that is a flag rather than the convention.
    chkstk: None,
};

static WIN64_INT_ARGS: [PhysReg; 4] = [RCX, RDX, R8, R9];
static WIN64_SSE_ARGS: [PhysReg; 4] = [xmm(0), xmm(1), xmm(2), xmm(3)];
static WIN64_INT_RETURNS: [PhysReg; 1] = [RAX];
static WIN64_SSE_RETURNS: [PhysReg; 1] = [xmm(0)];
// Windows defines `long double` as a `double`, so nothing ever comes back on the x87 stack.
static WIN64_X87_RETURNS: [PhysReg; 0] = [];
static WIN64_INT_SAVED: [PhysReg; 8] = [RBX, RBP, RSI, RDI, R12, R13, R14, R15];
// The upper ten vector registers are preserved here and none of them are on SysV, which is the
// difference that turns a hand-written SysV routine into a Windows crash rather than an error.
static WIN64_SSE_SAVED: [PhysReg; 10] =
    [xmm(6), xmm(7), xmm(8), xmm(9), xmm(10), xmm(11), xmm(12), xmm(13), xmm(14), xmm(15)];
static WIN64_INT_ORDER: [PhysReg; 14] =
    [RAX, RCX, RDX, R8, R9, R10, R11, RBX, RSI, RDI, R12, R13, R14, R15];

/// Where a Windows x64 call puts things, per `spec/12-abi-and-runtime.md` section 12.4.
///
/// Two of these rather than one, and the only thing they disagree about is what the routine in the
/// paragraph below is called. Everything else on this platform is the same under either runtime,
/// which is the whole point of a calling convention.
pub static WIN64: CallRegs = win64(Chkstk { name: "__chkstk", size: RAX });

/// The same convention where the GNU runtime provides the routine rather than Microsoft's.
///
/// mingw-w64 has its own spelling of it, and the extra underscore is not a typing mistake: a C name
/// on this target carries no leading underscore at all, so the three in the assembly are three in
/// the symbol. Both routines do the same thing and both leave the stack pointer where they found
/// it, which is why nothing but the name changes here.
pub static MINGW64: CallRegs = win64(Chkstk { name: "___chkstk_ms", size: RAX });

/// The Windows convention, given the routine the runtime in question provides.
const fn win64(chkstk: Chkstk) -> CallRegs {
    CallRegs {
        abi: &rucc_abi::abis::WIN64,
        int_class: GPR,
        sse_class: XMM,
        int_args: &WIN64_INT_ARGS,
        sse_args: &WIN64_SSE_ARGS,
        shared_positions: true,
        int_returns: &WIN64_INT_RETURNS,
        sse_returns: &WIN64_SSE_RETURNS,
        x87_returns: &WIN64_X87_RETURNS,
        int_saved: &WIN64_INT_SAVED,
        sse_saved: &WIN64_SSE_SAVED,
        int_order: &WIN64_INT_ORDER,
        sse_order: &SSE_ORDER,
        stack_pointer: RSP,
        frame_pointer: RBP,
        // After the frame here, because the unwind record this platform reads cannot describe the
        // other order. See [`CallRegs::late_frame_pointer`].
        late_frame_pointer: true,
        vector_count: None,
        red_zone: 0,
        shadow: 32,
        stack_align: 16,
        return_address: 8,
        word: 8,
        dwarf: &X86_64_DWARF,
        dwarf_return_address: DWARF_RETURN_ADDRESS,
        // None, and not because the platform has no protector. Windows has one and it is a
        // different mechanism: the cookie is a global the loader writes, the value stored in the
        // frame is that global exclusive-ored with the frame pointer, and the check is a call to
        // `__security_check_cookie` rather than a comparison the compiler writes. Writing `None`
        // here is what makes `-fstack-protector` on a Windows target an error that says so.
        guard: None,
        // None for the same shape of reason. Windows profiles a build by having the compiler call
        // `_penter`, which is asked for by a switch of its own and takes its argument in a register
        // rather than off the stack, so it is not the hook named here under another name.
        trace: None,
        // `rax` whichever runtime provides the routine, because the routine is the same routine:
        // it reads the size out of that register, touches each page from the one the stack pointer
        // is on down to the one that many bytes below it, and comes back with both registers as it
        // found them. So the caller takes the frame afterwards, and takes it with a subtraction of
        // that same register rather than of the constant written a second time.
        chkstk: Some(chkstk),
    }
}

#[cfg(test)]
mod tests {
    use crate::Role;

    use super::*;

    /// Every register in a list, and no register twice.
    fn covers(order: &[PhysReg], count: usize) -> bool {
        let mut seen: Vec<u8> = order.iter().map(|reg| reg.number()).collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len() == order.len() && order.len() == count
    }

    #[test]
    fn the_file_numbers_registers_the_way_the_encoding_does() {
        assert_eq!(REGS.name(GPR, RAX), Some("rax"));
        assert_eq!(REGS.name(GPR, RSP), Some("rsp"));
        assert_eq!(REGS.name(GPR, R15), Some("r15"));
        assert_eq!(REGS.reg_named("rdi"), Some((GPR, RDI)));
        assert_eq!(REGS.reg_named("xmm9"), Some((XMM, xmm(9))));
        assert_eq!(REGS.reg_named("st0"), Some((X87, st(0))));
    }

    /// The psABI's table, and the four that are not where the machine put them are the point of
    /// the test. A permutation is the kind of mistake that produces a table nothing complains
    /// about and that describes the wrong registers.
    #[test]
    fn dwarf_numbers_the_first_eight_registers_in_a_different_order() {
        assert_eq!(SYSV.dwarf(GPR, RAX), Some(0));
        assert_eq!(SYSV.dwarf(GPR, RDX), Some(1));
        assert_eq!(SYSV.dwarf(GPR, RCX), Some(2));
        assert_eq!(SYSV.dwarf(GPR, RBX), Some(3));
        assert_eq!(SYSV.dwarf(GPR, RSI), Some(4));
        assert_eq!(SYSV.dwarf(GPR, RDI), Some(5));
        assert_eq!(SYSV.dwarf(GPR, RBP), Some(6));
        assert_eq!(SYSV.dwarf(GPR, RSP), Some(7));
    }

    /// The upper eight agree with the machine, the vector registers start above the return
    /// address column, and the x87 stack has no column at all.
    #[test]
    fn dwarf_numbers_the_rest_the_way_the_machine_does() {
        assert_eq!(SYSV.dwarf(GPR, R8), Some(8));
        assert_eq!(SYSV.dwarf(GPR, R15), Some(15));
        assert_eq!(SYSV.dwarf_return_address, 16);
        assert_eq!(SYSV.dwarf(XMM, xmm(0)), Some(17));
        assert_eq!(SYSV.dwarf(XMM, xmm(15)), Some(32));
        assert_eq!(SYSV.dwarf(X87, st(0)), None);
        assert_eq!(WIN64.dwarf(GPR, RCX), Some(2));
    }

    /// The other way round, which is what a table in the machine's own numbering is written from.
    /// Every one of the sixteen comes back, and the four the two orders disagree about come back as
    /// the register that started the round trip rather than as the one with the same number.
    #[test]
    fn a_dwarf_number_leads_back_to_the_register_it_was_given_to() {
        for reg in [RAX, RCX, RDX, RBX, RSP, RBP, RSI, RDI, R8, R15] {
            let number = SYSV.dwarf(GPR, reg).expect("a general purpose register has a column");
            assert_eq!(SYSV.machine(GPR, number), Some(reg));
        }
        assert_eq!(SYSV.machine(GPR, 1), Some(RDX));
        assert_eq!(SYSV.machine(GPR, 2), Some(RCX));
        assert_eq!(SYSV.machine(XMM, 17), Some(xmm(0)));
        assert_eq!(SYSV.machine(GPR, 16), None, "the return address is not a register here");
        assert_eq!(SYSV.machine(X87, 0), None, "the x87 stack has no column to come back from");
    }

    #[test]
    fn the_file_gives_no_name_to_two_registers() {
        assert_eq!(REGS.duplicate(), None);
        assert_eq!(REGS.len(GPR), 16);
        assert_eq!(REGS.len(XMM), 16);
        assert_eq!(REGS.len(X87), 8);
    }

    #[test]
    fn the_allocator_is_offered_every_register_but_the_two_the_frame_needs() {
        for convention in [&SYSV, &WIN64] {
            assert!(covers(convention.int_order, 14));
            assert!(covers(convention.sse_order, 16));
            assert!(!convention.int_order.contains(&RSP));
            assert!(!convention.int_order.contains(&RBP));
        }
    }

    #[test]
    fn a_register_a_call_destroys_is_offered_before_one_it_preserves() {
        for convention in [&SYSV, &WIN64] {
            let first_saved = convention
                .int_order
                .iter()
                .position(|&reg| convention.preserves_int(reg))
                .expect("some register in the order is preserved");
            assert!(
                convention.int_order[..first_saved]
                    .iter()
                    .all(|&reg| !convention.preserves_int(reg)),
                "the preserved registers are not one run at the end"
            );
        }
    }

    #[test]
    fn the_two_conventions_disagree_where_the_psabis_do() {
        assert_eq!(SYSV.int_args[0], RDI);
        assert_eq!(WIN64.int_args[0], RCX);
        assert!(!SYSV.preserves_int(RDI));
        assert!(WIN64.preserves_int(RDI));
        assert!(!SYSV.preserves_sse(xmm(6)));
        assert!(WIN64.preserves_sse(xmm(6)));
        assert_eq!((SYSV.red_zone, SYSV.shadow), (128, 0));
        assert_eq!((WIN64.red_zone, WIN64.shadow), (0, 32));
        assert_eq!(SYSV.vector_count, Some(RAX));
        assert_eq!(WIN64.vector_count, None);
    }

    #[test]
    fn a_long_double_comes_back_on_the_x87_stack_only_where_there_is_one() {
        assert_eq!(SYSV.x87_returns, [st(0), st(1)]);
        assert!(WIN64.x87_returns.is_empty());
    }

    #[test]
    fn the_x87_stack_is_named_and_is_not_allocated_from() {
        assert!(REGS.allocatable(GPR));
        assert!(REGS.allocatable(XMM));
        assert!(!REGS.allocatable(X87));

        // Not allocatable is not the same as not describable, and both halves have to hold for
        // this class to be worth having at all. `st0` still has a name, because a `long double`
        // comes back in it and the convention above says so.
        assert_eq!(REGS.name(X87, st(0)), Some("st0"));
    }

    #[test]
    fn a_class_has_an_allocation_order_exactly_when_it_is_allocated_from() {
        // These were two separate facts until now. A class was unavailable because no convention
        // listed an order for it, which is something you find out by reading two files and
        // noticing an absence, and the allocator's own assertion was the first thing that said
        // so out loud. Tying them means a target that adds an order for a class it also says
        // nothing allocates from is a failing test here rather than a surprise down there.
        for convention in [&SYSV, &WIN64] {
            let ordered = [
                (convention.int_class, !convention.int_order.is_empty()),
                (convention.sse_class, !convention.sse_order.is_empty()),
                (X87, false),
            ];
            for (class, has_order) in ordered {
                assert_eq!(
                    REGS.allocatable(class),
                    has_order,
                    "{} says one thing and its allocation order says another",
                    REGS.class(class).expect("a class of this file").name
                );
            }
        }
    }

    /// Every comparison a rule can select has an entry, and every entry names real instructions.
    ///
    /// The table is what the block layout reads and it reads it by name, so an entry naming an
    /// opcode nothing describes would be an entry that never matches and the only sign of it
    /// would be a test that is still there. The count is against the descriptions rather than
    /// against a number typed here, which is what makes a comparison added later without an entry
    /// a failure rather than a missed saving.
    #[test]
    fn every_comparison_is_one_the_layout_can_take_the_test_off() {
        let described = |name: &str| INSTS.iter().any(|&(opcode, _)| opcode == name);
        let sets: Vec<&str> = INSTS
            .iter()
            .filter(|&&(_, shape)| {
                matches!(shape, Form::CmpSet | Form::CmpSetRi | Form::CmpSetRm | Form::CmpSetMi)
            })
            .map(|&(opcode, _)| opcode)
            .collect();
        let entries: Vec<&str> = BRANCH.fused.iter().map(|fusion| fusion.set).collect();
        assert_eq!(entries, sets);

        for fusion in BRANCH.fused {
            assert!(
                described(fusion.cmp),
                "{} becomes {}, which is nothing",
                fusion.set,
                fusion.cmp
            );
            assert!(described(fusion.if_true), "{} is nothing", fusion.if_true);
            assert!(described(fusion.if_false), "{} is nothing", fusion.if_false);
            // The comparison an entry becomes reads what the one it came from read, without the
            // byte at the front, and the jumps read no register at all. A width that did not
            // survive the entry would be a comparison of the wrong number of bytes.
            let before = form(fusion.set).expect("a described comparison").operands();
            let after = form(fusion.cmp).expect("a described comparison").operands();
            assert_eq!(after, &before[1..], "{} and {} disagree", fusion.set, fusion.cmp);
            assert_eq!(form(fusion.if_true), Some(Form::Jcc));
            assert_eq!(form(fusion.if_false), Some(Form::Jcc));
        }
    }

    /// The two jumps in an entry are a condition and its opposite.
    ///
    /// Which of them a block ends with is which of its arms the layout put next, so a pair that
    /// was not opposite would send half the branches in the program the wrong way. Nothing about
    /// the names says they are opposite, so what this checks is that following one and then the
    /// other from every entry gets back to where it started, which is the whole of what being
    /// opposite means and is a thing a table of ten cannot accidentally satisfy.
    #[test]
    fn the_two_jumps_a_comparison_becomes_are_a_condition_and_its_opposite() {
        for fusion in BRANCH.fused {
            let back = BRANCH
                .fused
                .iter()
                .find(|other| other.if_true == fusion.if_false && other.cmp == fusion.cmp)
                .unwrap_or_else(|| panic!("{} has no opposite", fusion.if_false));
            assert_eq!(back.if_false, fusion.if_true, "{} is not an opposite", fusion.if_true);
        }
        // And the test the layout writes when there is no comparison to fold into still names
        // two of the same ten, so that a target cannot end up with one set of jumps for the
        // folded branches and another for the rest.
        let jumps: Vec<&str> = BRANCH.fused.iter().map(|fusion| fusion.if_true).collect();
        assert!(jumps.contains(&BRANCH.if_true) && jumps.contains(&BRANCH.if_false));
    }

    /// Every select has a move for every condition a comparison can be asked, and each move is
    /// the select with the byte taken off the end.
    ///
    /// A condition with no entry would be a select that keeps its test, which is a missed saving,
    /// and an entry naming the wrong condition would be a select that picks the wrong value, so
    /// the condition on the end of the move's name has to be the one on the end of the jump's.
    #[test]
    fn every_select_on_a_comparison_has_the_move_that_reads_its_condition() {
        let selects: Vec<&str> = INSTS
            .iter()
            .filter(|&&(_, shape)| shape == Form::TestCmov)
            .map(|&(opcode, _)| opcode)
            .collect();
        let mut conditions: Vec<&str> = BRANCH.fused.iter().map(|fusion| fusion.if_true).collect();
        conditions.sort_unstable();
        conditions.dedup();
        for select in &selects {
            for when in &conditions {
                let found = BRANCH
                    .moves
                    .iter()
                    .filter(|entry| entry.select == *select && entry.when == *when)
                    .count();
                assert_eq!(found, 1, "{select} on {when}");
            }
        }
        assert_eq!(BRANCH.moves.len(), selects.len() * conditions.len());
        for entry in BRANCH.moves {
            assert_eq!(form(entry.cmov), Some(Form::Cmov), "{} is not a move", entry.cmov);
            let before = form(entry.select).expect("a described select").operands();
            let after = form(entry.cmov).expect("a described move").operands();
            assert_eq!(after, &before[..before.len() - 1], "{} and {}", entry.select, entry.cmov);
            let asked = entry.when.strip_prefix("jcc_").expect("a jump on a condition");
            let read = entry.cmov.strip_prefix("cmov_").and_then(|rest| rest.rsplit_once('_'));
            assert_eq!(read.map(|(condition, _)| condition), Some(asked), "{}", entry.cmov);
        }
    }

    /// The condition on the end of an opcode's name, which is the part the tables are indexed by.
    fn condition(name: &str) -> &str {
        let name = name
            .strip_prefix("cmp_set_")
            .or_else(|| name.strip_prefix("set_"))
            .or_else(|| name.strip_prefix("cmov_"))
            .or_else(|| name.strip_prefix("jcc_"))
            .expect("an opcode with a condition in its name");
        let name = name.rsplit_once('_').map_or(name, |(front, back)| {
            if matches!(back, "8" | "16" | "32" | "64") { front } else { name }
        });
        let name = name.strip_suffix("_ri").unwrap_or(name);
        let name = name.strip_suffix("_rm").unwrap_or(name);
        name.strip_suffix("_mi").unwrap_or(name)
    }

    /// Every comparison this target has is one the pass knows what to do with.
    ///
    /// The same argument the fusion table is checked under. A comparison added later with no entry
    /// here would be one the pass walks past, and the only sign of it would be a saving that
    /// quietly did not happen, so the list is taken from the instruction descriptions rather than
    /// written out again.
    #[test]
    fn every_comparison_has_an_entry_saying_what_it_asks_and_what_is_left_of_it() {
        let described = |name: &str| INSTS.iter().any(|&(opcode, _)| opcode == name);
        let comparisons: Vec<&str> = INSTS
            .iter()
            .filter(|&&(_, shape)| {
                matches!(shape, Form::CmpSet | Form::CmpSetRi | Form::Cmp | Form::CmpRi)
            })
            .map(|&(opcode, _)| opcode)
            .collect();
        let entries: Vec<&str> = COMPARES.iter().map(|entry| entry.name).collect();
        assert_eq!(entries, comparisons);

        for entry in &COMPARES {
            assert!(described(entry.asks), "{} asks {}, which is nothing", entry.name, entry.asks);
            let operands = form(entry.name).expect("a described comparison").operands();
            match entry.kept {
                // A comparison that keeps a byte asks what the flag-only one of its width asks,
                // which is the same instruction with the byte taken off the front, and what is
                // left of it is the byte alone. Those two halves have to add back up to it or the
                // pass would be writing an instruction that reads somewhere the original did not.
                Some(kept) => {
                    assert!(described(kept), "{} becomes {}, which is nothing", entry.name, kept);
                    assert_eq!(form(kept), Some(Form::Set), "{kept} is not a byte on its own");
                    assert_eq!(form(kept).expect("a described byte").operands(), &operands[..1]);
                    let asks = form(entry.asks).expect("a described comparison").operands();
                    assert_eq!(asks, &operands[1..], "{} and {} disagree", entry.name, entry.asks);
                }
                // A comparison that keeps nothing is already the whole of what it asks, so there
                // is nothing to leave behind and nothing for the entry to point at but itself.
                None => assert_eq!(entry.asks, entry.name),
            }
        }
    }

    /// No comparison that reads memory is one the pass is told it may take out.
    ///
    /// The other side of the test above, and the one place where a missing entry is the answer
    /// rather than an oversight. Two comparisons are the same question when the entry agrees and
    /// the registers and constants agree, and neither of those says anything about where an
    /// address points or about what was written there in between, so an entry for one of these
    /// would let the pass drop a comparison that asks something else. The argument is in the table
    /// documentation and this is the line that keeps somebody from filling the gap in.
    #[test]
    fn no_comparison_that_reads_memory_is_one_the_pass_may_take_out() {
        for entry in &COMPARES {
            let shape = form(entry.name).expect("a described comparison");
            assert!(!shape.takes_mem(), "{} reads memory and has an entry", entry.name);
        }
    }

    /// A comparison against memory still counts as writing the condition state.
    ///
    /// Which is what makes leaving it out of the table above safe rather than merely unhelpful.
    /// The pass walks forward holding what the last comparison left, and an instruction it has no
    /// entry for has to be one it throws that away at, or it would go on believing a statement
    /// that a comparison in between has already written over.
    #[test]
    fn a_comparison_against_memory_is_one_the_pass_stops_at() {
        for &(name, shape) in INSTS {
            if matches!(shape, Form::CmpSetRm | Form::CmpRm | Form::CmpSetMi | Form::CmpMi) {
                assert!(COMPARES.iter().all(|entry| entry.name != name), "{name} has an entry");
                assert!(writes_flags(name), "{name} is said to leave the state alone");
            }
        }
    }

    /// Every comparison that keeps a byte reads only what it found, including the ones against
    /// memory that the comparison table leaves out, and an add with carry reads what it was left.
    #[test]
    fn a_comparison_that_keeps_a_byte_asks_what_it_reads_whatever_its_operands() {
        for &(name, shape) in INSTS {
            if matches!(shape, Form::CmpSet | Form::CmpSetRi | Form::CmpSetRm | Form::CmpSetMi) {
                assert!(FLAGS.asks_what_it_reads(name), "{name} reads what it found");
            }
        }
        assert!(FLAGS.asks_what_it_reads("cmp_set_g_rm_32"));
        assert!(FLAGS.compare("cmp_set_g_rm_32").is_none());
        assert!(!FLAGS.asks_what_it_reads("adc_rr_32"));
        assert!(!FLAGS.asks_what_it_reads("set_g"));
    }

    /// Every instruction that reads the condition state says which part of it it reads.
    ///
    /// This is the table the pass is at the mercy of. A condition missing from it is one the pass
    /// does not see, and the instruction it belongs to would be left reading the bits of whatever
    /// the pass decided to keep instead, so the list is again taken from the descriptions. That
    /// each entry names the right part is checked against the condition in the opcode's own name,
    /// which is two hundred and ten rows that cannot be hand checked and three groups that can.
    /// The eight with no condition in their names are the add with carry and the subtract with
    /// borrow, which have a group of their own and are checked by their form instead.
    #[test]
    fn every_condition_says_which_part_of_the_state_it_is_about() {
        let readers: Vec<&str> = INSTS
            .iter()
            .filter(|&&(_, shape)| {
                matches!(
                    shape,
                    Form::AluCarry
                        | Form::AluCarryI
                        | Form::CmpSet
                        | Form::CmpSetRi
                        | Form::CmpSetRm
                        | Form::CmpSetMi
                        | Form::Set
                        | Form::Cmov
                        | Form::Jcc
                )
            })
            .map(|&(opcode, _)| opcode)
            .collect();
        let entries: Vec<&str> = READERS.iter().map(|entry| entry.name).collect();
        assert_eq!(entries, readers);

        for entry in &READERS {
            if matches!(form(entry.name), Some(Form::AluCarry | Form::AluCarryI)) {
                assert_eq!(entry.reads, Reads::Carry, "{} reads a condition", entry.name);
                continue;
            }
            let wanted = match condition(entry.name) {
                "e" | "ne" => Reads::Zero,
                "l" | "le" | "g" | "ge" => Reads::Signed,
                "b" | "be" | "a" | "ae" => Reads::Unsigned,
                "s" | "ns" | "o" | "no" | "p" | "np" => Reads::Bit,
                other => panic!("{} asks about {other}, which is nothing", entry.name),
            };
            assert_eq!(entry.reads, wanted, "{} reads the wrong part", entry.name);
        }
    }

    /// Nothing that only reads the condition state also writes it.
    ///
    /// The pass looks forward from a comparison it wants to take out for everything that would end
    /// up reading what it leaves instead, and it stops at the first instruction that writes the
    /// state, because past that point what is there is not its business. A byte, a move or a jump
    /// wrongly counted as a writer would stop that walk early and leave a condition behind it
    /// unaccounted for, which is the one way this pass could be wrong rather than merely unhelpful.
    /// The add with carry and the subtract with borrow are the entries here that read the state and
    /// write it both, and they are why the pass counts what an instruction reads before it asks
    /// whether it writes rather than after.
    #[test]
    fn a_byte_a_move_or_a_jump_leaves_the_condition_state_where_it_found_it() {
        for entry in &READERS {
            if matches!(form(entry.name), Some(Form::Set | Form::Cmov | Form::Jcc)) {
                assert!(!writes_flags(entry.name), "{} is said to write the state", entry.name);
            } else {
                assert!(writes_flags(entry.name), "{} leaves the state alone", entry.name);
            }
        }
    }

    /// Every instruction said to leave a comparison behind is arithmetic that writes one register.
    ///
    /// What the entry claims is that the state after it is the state after comparing the register
    /// it wrote against zero, which only means anything if there is exactly one such register and
    /// the description says how wide it is. The three kinds the doc argues are not safe are named
    /// again here, because the argument for leaving them out lives in prose and this is the part
    /// of it a change to the table would have to get past.
    #[test]
    fn what_leaves_a_comparison_behind_is_arithmetic_with_one_answer() {
        for entry in &ZEROING {
            let shape = form(entry.name).expect("a described instruction");
            assert!(
                matches!(shape, Form::AluRr | Form::AluRi | Form::UnaryR),
                "{} is not arithmetic",
                entry.name
            );
            assert!(writes_flags(entry.name), "{} leaves the state alone", entry.name);
            let written: Vec<u8> = shape
                .operands()
                .iter()
                .enumerate()
                .filter(|(_, operand)| matches!(operand.role, Role::Def | Role::EarlyDef))
                .map(|(at, _)| u8::try_from(at).expect("an operand index"))
                .collect();
            assert_eq!(written.len(), 1, "{} does not write one register", entry.name);
            assert!(
                operand_width(entry.name, written[0]).is_some(),
                "{} writes a register of no stated width",
                entry.name
            );
            let name = entry.name;
            assert!(
                !name.starts_with("shl") && !name.starts_with("shr") && !name.starts_with("sar"),
                "{name} is a shift, and a shift by zero leaves the state alone"
            );
            assert!(!name.starts_with("imul"), "{name} leaves the zero bit undefined");
            assert!(
                !name.starts_with("inc") && !name.starts_with("dec"),
                "{name} leaves the carry alone"
            );
        }
    }
}
