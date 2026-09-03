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

mod insts;

pub use crate::x86_64::insts::{Form, INSTS, form};

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
    ClassInfo { name: "gpr", bits: 64, regs: &GPR_NAMES },
    ClassInfo { name: "xmm", bits: 128, regs: &XMM_NAMES },
    // Eighty bits of value in a register the machine addresses as a stack rather than by
    // number, which is why nothing allocates from this class. It is described because a
    // `long double` comes back in `st0` and something has to be able to say so.
    ClassInfo { name: "x87", bits: 80, regs: &X87_NAMES },
];

/// Every register x86-64 has.
pub static REGS: RegFile = RegFile::new(&CLASSES);

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
    int_args: &SYSV_INT_ARGS,
    sse_args: &SYSV_SSE_ARGS,
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
    int_args: &WIN64_INT_ARGS,
    sse_args: &WIN64_SSE_ARGS,
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
}
