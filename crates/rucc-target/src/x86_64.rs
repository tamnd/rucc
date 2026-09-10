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
mod text;

pub use crate::x86_64::encode::{
    Addr, Encoding, Error, Fields, Fits, Holes, ImmSize, Kind, Size, Value, encode, encoding,
};
pub use crate::x86_64::insts::{ADDRESSES, Address, Form, INSTS, address, form};
pub use crate::x86_64::text::{Arg, Width, Written, gpr_name, written};

use crate::branch::{BranchInsts, Fusion};
use crate::frame::{ClassMoves, FrameInsts};
use crate::regs::{CallRegs, ClassInfo, PhysReg, RegClass, RegFile};

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
    align: "and_ri_64",
    lea: "lea_64",
    ret: "ret",
};

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
    fused: &FUSED,
};

/// Every comparison the test in front of a branch can be taken off, which is all of them.
///
/// Ten conditions at four widths, against a register and against a constant. A comparison sets
/// the condition state whether or not anybody keeps the byte, so the entry for one is the same
/// comparison without the `set` and the jump on the condition the `set` was naming. The two
/// conditional jumps in an entry are a condition and its opposite, since which of them the block
/// gets is which of its arms was laid out next.
static FUSED: [Fusion; 80] = [
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
    vector_count: Some(RAX),
    red_zone: 128,
    shadow: 0,
    stack_align: 16,
    return_address: 8,
    word: 8,
    dwarf: &X86_64_DWARF,
    dwarf_return_address: DWARF_RETURN_ADDRESS,
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
pub static WIN64: CallRegs = CallRegs {
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
    vector_count: None,
    red_zone: 0,
    shadow: 32,
    stack_align: 16,
    return_address: 8,
    word: 8,
    dwarf: &X86_64_DWARF,
    dwarf_return_address: DWARF_RETURN_ADDRESS,
};

#[cfg(test)]
mod tests {
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
            .filter(|&&(_, shape)| matches!(shape, Form::CmpSet | Form::CmpSetRi))
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
}
