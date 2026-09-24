//! What each AArch64 machine instruction is in assembly, and what it hands the encoder.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1.
//!
//! The same arrangement as `rucc_target::x86_64::written`: an opcode is a list of instructions and
//! each argument of each instruction names the operand it wants by index. What is different is the
//! last step. The x86 description hands its arguments to a writer and to an encoder that each know
//! how to spell them, and this one turns every argument into the [`Value`] the encoder takes with
//! [`fill`], so the object writer hands the values to [`encode`](crate::aarch64::encode) and `-S`
//! hands the same values to [`write`](crate::aarch64::write). What is printed is then what was
//! encoded, and not only what the table says ought to be.
//!
//! # Operand order
//!
//! The order the machine reads them, destination first, which is also the order the operand vector
//! holds them in. So an argument names its operand by index for the other reasons the x86 table
//! gives: an instruction may name one operand twice, a choice names its two sources the other way
//! round, and an instruction in the middle of an opcode may name none of them.
//!
//! # Register 31
//!
//! The register file numbers the stack pointer 31, and an instruction that writes 31 into a
//! register field means the stack pointer in some places and the zero register in others. [`fill`]
//! reads an allocated 31 as the stack pointer, since the zero register is not one the allocator
//! hands out, and the encoder refuses the stack pointer where the machine has no way to say it.

use crate::aarch64::encode::{
    Addr, Arrangement, Cond, Mode, Offset, Operator, Scalar, Shift, Value, Width,
};
use crate::aarch64::read::system_field;

use Arg::{
    Barrier, Base, Disp, Fp, GotPage, GotSlot, Imm, Label, Lit, Low, Mem, Page, Pop, Push, Reg,
    Symbol, Thread, Through, TprelHi, TprelLo, Vector,
};
use Scalar::{D, Q, S};
use Width::{W, X};

/// One argument of one instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    /// The general register the operand at that index was given, at that width.
    Reg(u8, Width),
    /// The register in the other file the operand at that index was given, at that size.
    Fp(u8, Scalar),
    /// All sixteen bytes of the register in the other file the operand at that index was given.
    Vector(u8),
    /// The instruction's constant.
    Imm,
    /// A constant that is part of the opcode rather than of the instruction.
    Lit(i64),
    /// A shift that is part of the opcode.
    Shift(Shift, u8),
    /// A condition that is part of the opcode.
    Cond(Cond),
    /// A barrier option that is part of the opcode.
    Barrier(u8),
    /// The instruction's addressing mode.
    Mem,
    /// The base register of the instruction's addressing mode, as a register on its own.
    Base,
    /// The constant of the instruction's addressing mode, as a constant on its own.
    Disp,
    /// The page the instruction's symbol is on.
    Page,
    /// The low twelve bits of the instruction's symbol.
    Low,
    /// The page the global offset table slot of the instruction's symbol is on.
    GotPage,
    /// The global offset table slot of the instruction's symbol, reached from the page in the
    /// register the operand at that index was given.
    GotSlot(u8),
    /// The high twelve bits of the offset of the instruction's symbol from the thread pointer.
    TprelHi,
    /// The low twelve bits of the same offset.
    TprelLo,
    /// The register that holds the thread pointer.
    Thread,
    /// Sixteen bytes below the stack pointer, moving the stack pointer there first.
    Push,
    /// The stack pointer, moving it sixteen bytes up afterwards.
    Pop,
    /// The instruction's symbol, as a branch or a call reaches it.
    Symbol,
    /// The block a branch goes to.
    Label,
    /// The first register the instruction reads, which is what a call through a register reads.
    Through,
}

/// One machine instruction an opcode is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    /// The mnemonic, as GNU as reads it.
    pub mnemonic: &'static str,
    /// The arguments, in the order GNU as reads them.
    pub args: &'static [Arg],
}

const fn spell(mnemonic: &'static str, args: &'static [Arg]) -> Written {
    Written { mnemonic, args }
}

static TEXT: &[(&str, &[Written])] = &[
    // Constants. A constant that is not one `mov` writes is a `mov` of one sixteen bit piece and a
    // `movk` for each piece after it.
    ("mov_ri_32", &[spell("mov", &[Reg(0, W), Imm])]),
    ("mov_ri_64", &[spell("mov", &[Reg(0, X), Imm])]),
    ("movk_ri_16_32", &[spell("movk", &[Reg(0, W), Imm, Arg::Shift(Shift::Lsl, 16)])]),
    ("movk_ri_16_64", &[spell("movk", &[Reg(0, X), Imm, Arg::Shift(Shift::Lsl, 16)])]),
    ("movk_ri_32_64", &[spell("movk", &[Reg(0, X), Imm, Arg::Shift(Shift::Lsl, 32)])]),
    ("movk_ri_48_64", &[spell("movk", &[Reg(0, X), Imm, Arg::Shift(Shift::Lsl, 48)])]),
    // Copies.
    ("mov_rr_32", &[spell("mov", &[Reg(0, W), Reg(1, W)])]),
    ("mov_rr_64", &[spell("mov", &[Reg(0, X), Reg(1, X)])]),
    // Arithmetic on two registers.
    ("add_rr_32", &[spell("add", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("add_rr_64", &[spell("add", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("sub_rr_32", &[spell("sub", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("sub_rr_64", &[spell("sub", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("and_rr_32", &[spell("and", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("and_rr_64", &[spell("and", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("orr_rr_32", &[spell("orr", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("orr_rr_64", &[spell("orr", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("eor_rr_32", &[spell("eor", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("eor_rr_64", &[spell("eor", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("bic_rr_32", &[spell("bic", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("bic_rr_64", &[spell("bic", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("orn_rr_32", &[spell("orn", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("orn_rr_64", &[spell("orn", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("mul_rr_32", &[spell("mul", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("mul_rr_64", &[spell("mul", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("sdiv_rr_32", &[spell("sdiv", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("sdiv_rr_64", &[spell("sdiv", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("udiv_rr_32", &[spell("udiv", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("udiv_rr_64", &[spell("udiv", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("lsl_rr_32", &[spell("lslv", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("lsl_rr_64", &[spell("lslv", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("lsr_rr_32", &[spell("lsrv", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("lsr_rr_64", &[spell("lsrv", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("asr_rr_32", &[spell("asrv", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("asr_rr_64", &[spell("asrv", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("ror_rr_32", &[spell("rorv", &[Reg(0, W), Reg(1, W), Reg(2, W)])]),
    ("ror_rr_64", &[spell("rorv", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("smulh_rr_64", &[spell("smulh", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    ("umulh_rr_64", &[spell("umulh", &[Reg(0, X), Reg(1, X), Reg(2, X)])]),
    // Arithmetic on a register and a constant. The constant is one the instruction can hold, which
    // for `add` and `sub` is twelve bits and for the three logical operations is a bitmask.
    ("add_ri_32", &[spell("add", &[Reg(0, W), Reg(1, W), Imm])]),
    ("add_ri_64", &[spell("add", &[Reg(0, X), Reg(1, X), Imm])]),
    ("sub_ri_32", &[spell("sub", &[Reg(0, W), Reg(1, W), Imm])]),
    ("sub_ri_64", &[spell("sub", &[Reg(0, X), Reg(1, X), Imm])]),
    ("and_ri_32", &[spell("and", &[Reg(0, W), Reg(1, W), Imm])]),
    ("and_ri_64", &[spell("and", &[Reg(0, X), Reg(1, X), Imm])]),
    ("orr_ri_32", &[spell("orr", &[Reg(0, W), Reg(1, W), Imm])]),
    ("orr_ri_64", &[spell("orr", &[Reg(0, X), Reg(1, X), Imm])]),
    ("eor_ri_32", &[spell("eor", &[Reg(0, W), Reg(1, W), Imm])]),
    ("eor_ri_64", &[spell("eor", &[Reg(0, X), Reg(1, X), Imm])]),
    ("lsl_ri_32", &[spell("lsl", &[Reg(0, W), Reg(1, W), Imm])]),
    ("lsl_ri_64", &[spell("lsl", &[Reg(0, X), Reg(1, X), Imm])]),
    ("lsr_ri_32", &[spell("lsr", &[Reg(0, W), Reg(1, W), Imm])]),
    ("lsr_ri_64", &[spell("lsr", &[Reg(0, X), Reg(1, X), Imm])]),
    ("asr_ri_32", &[spell("asr", &[Reg(0, W), Reg(1, W), Imm])]),
    ("asr_ri_64", &[spell("asr", &[Reg(0, X), Reg(1, X), Imm])]),
    ("ror_ri_32", &[spell("ror", &[Reg(0, W), Reg(1, W), Imm])]),
    ("ror_ri_64", &[spell("ror", &[Reg(0, X), Reg(1, X), Imm])]),
    // Multiply and add, and multiply and subtract, which is how a remainder is taken: the quotient
    // times the divisor, subtracted from the dividend.
    ("madd_rrr_32", &[spell("madd", &[Reg(0, W), Reg(1, W), Reg(2, W), Reg(3, W)])]),
    ("madd_rrr_64", &[spell("madd", &[Reg(0, X), Reg(1, X), Reg(2, X), Reg(3, X)])]),
    ("msub_rrr_32", &[spell("msub", &[Reg(0, W), Reg(1, W), Reg(2, W), Reg(3, W)])]),
    ("msub_rrr_64", &[spell("msub", &[Reg(0, X), Reg(1, X), Reg(2, X), Reg(3, X)])]),
    // One register to one register.
    ("neg_r_32", &[spell("neg", &[Reg(0, W), Reg(1, W)])]),
    ("neg_r_64", &[spell("neg", &[Reg(0, X), Reg(1, X)])]),
    ("mvn_r_32", &[spell("mvn", &[Reg(0, W), Reg(1, W)])]),
    ("mvn_r_64", &[spell("mvn", &[Reg(0, X), Reg(1, X)])]),
    ("clz_r_32", &[spell("clz", &[Reg(0, W), Reg(1, W)])]),
    ("clz_r_64", &[spell("clz", &[Reg(0, X), Reg(1, X)])]),
    ("rbit_r_32", &[spell("rbit", &[Reg(0, W), Reg(1, W)])]),
    ("rbit_r_64", &[spell("rbit", &[Reg(0, X), Reg(1, X)])]),
    ("rev_r_32", &[spell("rev", &[Reg(0, W), Reg(1, W)])]),
    ("rev_r_64", &[spell("rev", &[Reg(0, X), Reg(1, X)])]),
    ("rev16_r_32", &[spell("rev16", &[Reg(0, W), Reg(1, W)])]),
    // Widening. Each reads the low bits of its source and agrees with it about every one of them,
    // which is what `copies_low` asks.
    ("sxtb_32", &[spell("sxtb", &[Reg(0, W), Reg(1, W)])]),
    ("sxtb_64", &[spell("sxtb", &[Reg(0, X), Reg(1, W)])]),
    ("sxth_32", &[spell("sxth", &[Reg(0, W), Reg(1, W)])]),
    ("sxth_64", &[spell("sxth", &[Reg(0, X), Reg(1, W)])]),
    ("uxtb_32", &[spell("uxtb", &[Reg(0, W), Reg(1, W)])]),
    ("uxth_32", &[spell("uxth", &[Reg(0, W), Reg(1, W)])]),
    ("sxtw_64", &[spell("sxtw", &[Reg(0, X), Reg(1, W)])]),
    ("uxtw_64", &[spell("mov", &[Reg(0, W), Reg(1, W)])]),
    // Comparisons that keep nothing but the condition state, which is what a branch on one is
    // folded into.
    ("cmp_rr_32", &[spell("cmp", &[Reg(0, W), Reg(1, W)])]),
    ("cmp_ri_32", &[spell("cmp", &[Reg(0, W), Imm])]),
    ("test_32", &[spell("cmp", &[Reg(0, W), Lit(0)])]),
    ("cmp_rr_64", &[spell("cmp", &[Reg(0, X), Reg(1, X)])]),
    ("cmp_ri_64", &[spell("cmp", &[Reg(0, X), Imm])]),
    ("test_64", &[spell("cmp", &[Reg(0, X), Lit(0)])]),
    // Comparisons that keep the answer, which is `cset` after the comparison.
    (
        "cmp_set_eq_32",
        &[spell("cmp", &[Reg(1, W), Reg(2, W)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Eq)])],
    ),
    (
        "cmp_set_eq_64",
        &[spell("cmp", &[Reg(1, X), Reg(2, X)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Eq)])],
    ),
    (
        "cmp_set_ne_32",
        &[spell("cmp", &[Reg(1, W), Reg(2, W)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ne)])],
    ),
    (
        "cmp_set_ne_64",
        &[spell("cmp", &[Reg(1, X), Reg(2, X)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ne)])],
    ),
    (
        "cmp_set_lt_32",
        &[spell("cmp", &[Reg(1, W), Reg(2, W)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Lt)])],
    ),
    (
        "cmp_set_lt_64",
        &[spell("cmp", &[Reg(1, X), Reg(2, X)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Lt)])],
    ),
    (
        "cmp_set_le_32",
        &[spell("cmp", &[Reg(1, W), Reg(2, W)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Le)])],
    ),
    (
        "cmp_set_le_64",
        &[spell("cmp", &[Reg(1, X), Reg(2, X)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Le)])],
    ),
    (
        "cmp_set_gt_32",
        &[spell("cmp", &[Reg(1, W), Reg(2, W)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Gt)])],
    ),
    (
        "cmp_set_gt_64",
        &[spell("cmp", &[Reg(1, X), Reg(2, X)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Gt)])],
    ),
    (
        "cmp_set_ge_32",
        &[spell("cmp", &[Reg(1, W), Reg(2, W)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ge)])],
    ),
    (
        "cmp_set_ge_64",
        &[spell("cmp", &[Reg(1, X), Reg(2, X)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ge)])],
    ),
    (
        "cmp_set_lo_32",
        &[spell("cmp", &[Reg(1, W), Reg(2, W)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Lo)])],
    ),
    (
        "cmp_set_lo_64",
        &[spell("cmp", &[Reg(1, X), Reg(2, X)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Lo)])],
    ),
    (
        "cmp_set_ls_32",
        &[spell("cmp", &[Reg(1, W), Reg(2, W)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ls)])],
    ),
    (
        "cmp_set_ls_64",
        &[spell("cmp", &[Reg(1, X), Reg(2, X)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ls)])],
    ),
    (
        "cmp_set_hi_32",
        &[spell("cmp", &[Reg(1, W), Reg(2, W)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Hi)])],
    ),
    (
        "cmp_set_hi_64",
        &[spell("cmp", &[Reg(1, X), Reg(2, X)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Hi)])],
    ),
    (
        "cmp_set_hs_32",
        &[spell("cmp", &[Reg(1, W), Reg(2, W)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Hs)])],
    ),
    (
        "cmp_set_hs_64",
        &[spell("cmp", &[Reg(1, X), Reg(2, X)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Hs)])],
    ),
    (
        "cmp_set_eq_ri_32",
        &[spell("cmp", &[Reg(1, W), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Eq)])],
    ),
    (
        "cmp_set_eq_ri_64",
        &[spell("cmp", &[Reg(1, X), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Eq)])],
    ),
    (
        "cmp_set_ne_ri_32",
        &[spell("cmp", &[Reg(1, W), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ne)])],
    ),
    (
        "cmp_set_ne_ri_64",
        &[spell("cmp", &[Reg(1, X), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ne)])],
    ),
    (
        "cmp_set_lt_ri_32",
        &[spell("cmp", &[Reg(1, W), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Lt)])],
    ),
    (
        "cmp_set_lt_ri_64",
        &[spell("cmp", &[Reg(1, X), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Lt)])],
    ),
    (
        "cmp_set_le_ri_32",
        &[spell("cmp", &[Reg(1, W), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Le)])],
    ),
    (
        "cmp_set_le_ri_64",
        &[spell("cmp", &[Reg(1, X), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Le)])],
    ),
    (
        "cmp_set_gt_ri_32",
        &[spell("cmp", &[Reg(1, W), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Gt)])],
    ),
    (
        "cmp_set_gt_ri_64",
        &[spell("cmp", &[Reg(1, X), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Gt)])],
    ),
    (
        "cmp_set_ge_ri_32",
        &[spell("cmp", &[Reg(1, W), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ge)])],
    ),
    (
        "cmp_set_ge_ri_64",
        &[spell("cmp", &[Reg(1, X), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ge)])],
    ),
    (
        "cmp_set_lo_ri_32",
        &[spell("cmp", &[Reg(1, W), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Lo)])],
    ),
    (
        "cmp_set_lo_ri_64",
        &[spell("cmp", &[Reg(1, X), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Lo)])],
    ),
    (
        "cmp_set_ls_ri_32",
        &[spell("cmp", &[Reg(1, W), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ls)])],
    ),
    (
        "cmp_set_ls_ri_64",
        &[spell("cmp", &[Reg(1, X), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ls)])],
    ),
    (
        "cmp_set_hi_ri_32",
        &[spell("cmp", &[Reg(1, W), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Hi)])],
    ),
    (
        "cmp_set_hi_ri_64",
        &[spell("cmp", &[Reg(1, X), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Hi)])],
    ),
    (
        "cmp_set_hs_ri_32",
        &[spell("cmp", &[Reg(1, W), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Hs)])],
    ),
    (
        "cmp_set_hs_ri_64",
        &[spell("cmp", &[Reg(1, X), Imm]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Hs)])],
    ),
    // A choice on a byte, which is the second source when the byte is not zero and the first when
    // it is, and the same choice straight off a condition some comparison left.
    (
        "sel_32",
        &[
            spell("cmp", &[Reg(3, W), Lit(0)]),
            spell("csel", &[Reg(0, W), Reg(2, W), Reg(1, W), Arg::Cond(Cond::Ne)]),
        ],
    ),
    (
        "sel_64",
        &[
            spell("cmp", &[Reg(3, W), Lit(0)]),
            spell("csel", &[Reg(0, X), Reg(2, X), Reg(1, X), Arg::Cond(Cond::Ne)]),
        ],
    ),
    ("csel_eq_32", &[spell("csel", &[Reg(0, W), Reg(2, W), Reg(1, W), Arg::Cond(Cond::Eq)])]),
    ("csel_eq_64", &[spell("csel", &[Reg(0, X), Reg(2, X), Reg(1, X), Arg::Cond(Cond::Eq)])]),
    ("csel_ne_32", &[spell("csel", &[Reg(0, W), Reg(2, W), Reg(1, W), Arg::Cond(Cond::Ne)])]),
    ("csel_ne_64", &[spell("csel", &[Reg(0, X), Reg(2, X), Reg(1, X), Arg::Cond(Cond::Ne)])]),
    ("csel_lt_32", &[spell("csel", &[Reg(0, W), Reg(2, W), Reg(1, W), Arg::Cond(Cond::Lt)])]),
    ("csel_lt_64", &[spell("csel", &[Reg(0, X), Reg(2, X), Reg(1, X), Arg::Cond(Cond::Lt)])]),
    ("csel_le_32", &[spell("csel", &[Reg(0, W), Reg(2, W), Reg(1, W), Arg::Cond(Cond::Le)])]),
    ("csel_le_64", &[spell("csel", &[Reg(0, X), Reg(2, X), Reg(1, X), Arg::Cond(Cond::Le)])]),
    ("csel_gt_32", &[spell("csel", &[Reg(0, W), Reg(2, W), Reg(1, W), Arg::Cond(Cond::Gt)])]),
    ("csel_gt_64", &[spell("csel", &[Reg(0, X), Reg(2, X), Reg(1, X), Arg::Cond(Cond::Gt)])]),
    ("csel_ge_32", &[spell("csel", &[Reg(0, W), Reg(2, W), Reg(1, W), Arg::Cond(Cond::Ge)])]),
    ("csel_ge_64", &[spell("csel", &[Reg(0, X), Reg(2, X), Reg(1, X), Arg::Cond(Cond::Ge)])]),
    ("csel_lo_32", &[spell("csel", &[Reg(0, W), Reg(2, W), Reg(1, W), Arg::Cond(Cond::Lo)])]),
    ("csel_lo_64", &[spell("csel", &[Reg(0, X), Reg(2, X), Reg(1, X), Arg::Cond(Cond::Lo)])]),
    ("csel_ls_32", &[spell("csel", &[Reg(0, W), Reg(2, W), Reg(1, W), Arg::Cond(Cond::Ls)])]),
    ("csel_ls_64", &[spell("csel", &[Reg(0, X), Reg(2, X), Reg(1, X), Arg::Cond(Cond::Ls)])]),
    ("csel_hi_32", &[spell("csel", &[Reg(0, W), Reg(2, W), Reg(1, W), Arg::Cond(Cond::Hi)])]),
    ("csel_hi_64", &[spell("csel", &[Reg(0, X), Reg(2, X), Reg(1, X), Arg::Cond(Cond::Hi)])]),
    ("csel_hs_32", &[spell("csel", &[Reg(0, W), Reg(2, W), Reg(1, W), Arg::Cond(Cond::Hs)])]),
    ("csel_hs_64", &[spell("csel", &[Reg(0, X), Reg(2, X), Reg(1, X), Arg::Cond(Cond::Hs)])]),
    // Addresses. A base and a constant is one `add`, and a symbol is the page it is on and then the
    // offset into that page.
    ("lea_64", &[spell("add", &[Reg(0, X), Base, Disp])]),
    ("addr_64", &[spell("adrp", &[Reg(0, X), Page]), spell("add", &[Reg(0, X), Reg(0, X), Low])]),
    ("got_64", &[spell("adrp", &[Reg(0, X), GotPage]), spell("ldr", &[Reg(0, X), GotSlot(0)])]),
    (
        "tls_64",
        &[
            spell("mrs", &[Reg(0, X), Thread]),
            spell("add", &[Reg(0, X), Reg(0, X), TprelHi, Arg::Shift(Shift::Lsl, 12)]),
            spell("add", &[Reg(0, X), Reg(0, X), TprelLo]),
        ],
    ),
    // Loads, which write a whole register whatever they read.
    ("ldr_8", &[spell("ldrb", &[Reg(0, W), Mem])]),
    ("ldr_16", &[spell("ldrh", &[Reg(0, W), Mem])]),
    ("ldr_32", &[spell("ldr", &[Reg(0, W), Mem])]),
    ("ldr_64", &[spell("ldr", &[Reg(0, X), Mem])]),
    ("ldrs_8_32", &[spell("ldrsb", &[Reg(0, W), Mem])]),
    ("ldrs_8_64", &[spell("ldrsb", &[Reg(0, X), Mem])]),
    ("ldrs_16_32", &[spell("ldrsh", &[Reg(0, W), Mem])]),
    ("ldrs_16_64", &[spell("ldrsh", &[Reg(0, X), Mem])]),
    ("ldrs_32_64", &[spell("ldrsw", &[Reg(0, X), Mem])]),
    // Stores.
    ("str_8", &[spell("strb", &[Reg(0, W), Mem])]),
    ("str_16", &[spell("strh", &[Reg(0, W), Mem])]),
    ("str_32", &[spell("str", &[Reg(0, W), Mem])]),
    ("str_64", &[spell("str", &[Reg(0, X), Mem])]),
    // Loads and stores of the other register file.
    ("ldr_f32", &[spell("ldr", &[Fp(0, S), Mem])]),
    ("ldr_f64", &[spell("ldr", &[Fp(0, D), Mem])]),
    ("ldr_f128", &[spell("ldr", &[Fp(0, Q), Mem])]),
    ("str_f32", &[spell("str", &[Fp(0, S), Mem])]),
    ("str_f64", &[spell("str", &[Fp(0, D), Mem])]),
    ("str_f128", &[spell("str", &[Fp(0, Q), Mem])]),
    // Floating point arithmetic.
    ("fadd_f32", &[spell("fadd", &[Fp(0, S), Fp(1, S), Fp(2, S)])]),
    ("fadd_f64", &[spell("fadd", &[Fp(0, D), Fp(1, D), Fp(2, D)])]),
    ("fsub_f32", &[spell("fsub", &[Fp(0, S), Fp(1, S), Fp(2, S)])]),
    ("fsub_f64", &[spell("fsub", &[Fp(0, D), Fp(1, D), Fp(2, D)])]),
    ("fmul_f32", &[spell("fmul", &[Fp(0, S), Fp(1, S), Fp(2, S)])]),
    ("fmul_f64", &[spell("fmul", &[Fp(0, D), Fp(1, D), Fp(2, D)])]),
    ("fdiv_f32", &[spell("fdiv", &[Fp(0, S), Fp(1, S), Fp(2, S)])]),
    ("fdiv_f64", &[spell("fdiv", &[Fp(0, D), Fp(1, D), Fp(2, D)])]),
    ("fmin_f32", &[spell("fmin", &[Fp(0, S), Fp(1, S), Fp(2, S)])]),
    ("fmin_f64", &[spell("fmin", &[Fp(0, D), Fp(1, D), Fp(2, D)])]),
    ("fmax_f32", &[spell("fmax", &[Fp(0, S), Fp(1, S), Fp(2, S)])]),
    ("fmax_f64", &[spell("fmax", &[Fp(0, D), Fp(1, D), Fp(2, D)])]),
    ("fminnm_f32", &[spell("fminnm", &[Fp(0, S), Fp(1, S), Fp(2, S)])]),
    ("fminnm_f64", &[spell("fminnm", &[Fp(0, D), Fp(1, D), Fp(2, D)])]),
    ("fmaxnm_f32", &[spell("fmaxnm", &[Fp(0, S), Fp(1, S), Fp(2, S)])]),
    ("fmaxnm_f64", &[spell("fmaxnm", &[Fp(0, D), Fp(1, D), Fp(2, D)])]),
    ("fneg_f32", &[spell("fneg", &[Fp(0, S), Fp(1, S)])]),
    ("fneg_f64", &[spell("fneg", &[Fp(0, D), Fp(1, D)])]),
    ("fabs_f32", &[spell("fabs", &[Fp(0, S), Fp(1, S)])]),
    ("fabs_f64", &[spell("fabs", &[Fp(0, D), Fp(1, D)])]),
    ("fsqrt_f32", &[spell("fsqrt", &[Fp(0, S), Fp(1, S)])]),
    ("fsqrt_f64", &[spell("fsqrt", &[Fp(0, D), Fp(1, D)])]),
    ("frintz_f32", &[spell("frintz", &[Fp(0, S), Fp(1, S)])]),
    ("frintz_f64", &[spell("frintz", &[Fp(0, D), Fp(1, D)])]),
    ("frintm_f32", &[spell("frintm", &[Fp(0, S), Fp(1, S)])]),
    ("frintm_f64", &[spell("frintm", &[Fp(0, D), Fp(1, D)])]),
    ("frintp_f32", &[spell("frintp", &[Fp(0, S), Fp(1, S)])]),
    ("frintp_f64", &[spell("frintp", &[Fp(0, D), Fp(1, D)])]),
    ("frinta_f32", &[spell("frinta", &[Fp(0, S), Fp(1, S)])]),
    ("frinta_f64", &[spell("frinta", &[Fp(0, D), Fp(1, D)])]),
    ("frintn_f32", &[spell("frintn", &[Fp(0, S), Fp(1, S)])]),
    ("frintn_f64", &[spell("frintn", &[Fp(0, D), Fp(1, D)])]),
    ("frintx_f32", &[spell("frintx", &[Fp(0, S), Fp(1, S)])]),
    ("frintx_f64", &[spell("frintx", &[Fp(0, D), Fp(1, D)])]),
    ("frinti_f32", &[spell("frinti", &[Fp(0, S), Fp(1, S)])]),
    ("frinti_f64", &[spell("frinti", &[Fp(0, D), Fp(1, D)])]),
    // Copies within the other register file. A 128 bit value is copied as sixteen bytes, since
    // there is no scalar move that wide.
    ("fmov_rr_f32", &[spell("fmov", &[Fp(0, S), Fp(1, S)])]),
    ("fmov_rr_f64", &[spell("fmov", &[Fp(0, D), Fp(1, D)])]),
    ("mov_rr_f128", &[spell("mov", &[Vector(0), Vector(1)])]),
    // Between the two floating point widths.
    ("fcvt_f64_f32", &[spell("fcvt", &[Fp(0, S), Fp(1, D)])]),
    ("fcvt_f32_f64", &[spell("fcvt", &[Fp(0, D), Fp(1, S)])]),
    // From an integer to a float, and back, rounding toward zero the way C does. The name is the
    // source and then the destination.
    ("scvtf_32_f32", &[spell("scvtf", &[Fp(0, S), Reg(1, W)])]),
    ("scvtf_32_f64", &[spell("scvtf", &[Fp(0, D), Reg(1, W)])]),
    ("scvtf_64_f32", &[spell("scvtf", &[Fp(0, S), Reg(1, X)])]),
    ("scvtf_64_f64", &[spell("scvtf", &[Fp(0, D), Reg(1, X)])]),
    ("ucvtf_32_f32", &[spell("ucvtf", &[Fp(0, S), Reg(1, W)])]),
    ("ucvtf_32_f64", &[spell("ucvtf", &[Fp(0, D), Reg(1, W)])]),
    ("ucvtf_64_f32", &[spell("ucvtf", &[Fp(0, S), Reg(1, X)])]),
    ("ucvtf_64_f64", &[spell("ucvtf", &[Fp(0, D), Reg(1, X)])]),
    ("fcvtzs_f32_32", &[spell("fcvtzs", &[Reg(0, W), Fp(1, S)])]),
    ("fcvtzs_f32_64", &[spell("fcvtzs", &[Reg(0, X), Fp(1, S)])]),
    ("fcvtzs_f64_32", &[spell("fcvtzs", &[Reg(0, W), Fp(1, D)])]),
    ("fcvtzs_f64_64", &[spell("fcvtzs", &[Reg(0, X), Fp(1, D)])]),
    ("fcvtzu_f32_32", &[spell("fcvtzu", &[Reg(0, W), Fp(1, S)])]),
    ("fcvtzu_f32_64", &[spell("fcvtzu", &[Reg(0, X), Fp(1, S)])]),
    ("fcvtzu_f64_32", &[spell("fcvtzu", &[Reg(0, W), Fp(1, D)])]),
    ("fcvtzu_f64_64", &[spell("fcvtzu", &[Reg(0, X), Fp(1, D)])]),
    // The bits of a register moved across to the other file unchanged, which is how a float
    // constant is built and how a union is read.
    ("fmov_to_f32", &[spell("fmov", &[Fp(0, S), Reg(1, W)])]),
    ("fmov_to_f64", &[spell("fmov", &[Fp(0, D), Reg(1, X)])]),
    ("fmov_from_f32", &[spell("fmov", &[Reg(0, W), Fp(1, S)])]),
    ("fmov_from_f64", &[spell("fmov", &[Reg(0, X), Fp(1, D)])]),
    // Floating point comparisons. The conditions are chosen so that every ordered one is false when
    // either side is not a number and `ne` is true, which is what C asks of each of them.
    ("fcmp_f32", &[spell("fcmp", &[Fp(0, S), Fp(1, S)])]),
    ("fcmp_f64", &[spell("fcmp", &[Fp(0, D), Fp(1, D)])]),
    (
        "fcmp_set_eq_f32",
        &[spell("fcmp", &[Fp(1, S), Fp(2, S)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Eq)])],
    ),
    (
        "fcmp_set_eq_f64",
        &[spell("fcmp", &[Fp(1, D), Fp(2, D)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Eq)])],
    ),
    (
        "fcmp_set_ne_f32",
        &[spell("fcmp", &[Fp(1, S), Fp(2, S)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ne)])],
    ),
    (
        "fcmp_set_ne_f64",
        &[spell("fcmp", &[Fp(1, D), Fp(2, D)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ne)])],
    ),
    (
        "fcmp_set_lt_f32",
        &[spell("fcmp", &[Fp(1, S), Fp(2, S)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Mi)])],
    ),
    (
        "fcmp_set_lt_f64",
        &[spell("fcmp", &[Fp(1, D), Fp(2, D)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Mi)])],
    ),
    (
        "fcmp_set_le_f32",
        &[spell("fcmp", &[Fp(1, S), Fp(2, S)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ls)])],
    ),
    (
        "fcmp_set_le_f64",
        &[spell("fcmp", &[Fp(1, D), Fp(2, D)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ls)])],
    ),
    (
        "fcmp_set_gt_f32",
        &[spell("fcmp", &[Fp(1, S), Fp(2, S)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Gt)])],
    ),
    (
        "fcmp_set_gt_f64",
        &[spell("fcmp", &[Fp(1, D), Fp(2, D)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Gt)])],
    ),
    (
        "fcmp_set_ge_f32",
        &[spell("fcmp", &[Fp(1, S), Fp(2, S)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ge)])],
    ),
    (
        "fcmp_set_ge_f64",
        &[spell("fcmp", &[Fp(1, D), Fp(2, D)]), spell("cset", &[Reg(0, W), Arg::Cond(Cond::Ge)])],
    ),
    // Values arriving and leaving in the registers the convention names. None is an instruction,
    // and each is here so the allocator knows which register the value has to be in.
    ("ret_val_32", &[]),
    ("ret_val_64", &[]),
    ("ret_val_f32", &[]),
    ("ret_val_f64", &[]),
    ("ret_val_f128", &[]),
    ("ret_val2_32", &[]),
    ("ret_val2_64", &[]),
    ("ret_val2_f32", &[]),
    ("ret_val2_f64", &[]),
    ("ret_val2_f128", &[]),
    ("arg_val_32", &[]),
    ("arg_val_64", &[]),
    ("arg_val_f32", &[]),
    ("arg_val_f64", &[]),
    ("arg_val_f128", &[]),
    ("br_cond_32", &[]),
    // Branches, calls and the return.
    ("b", &[spell("b", &[Label])]),
    ("b_eq", &[spell("b.eq", &[Label])]),
    ("b_ne", &[spell("b.ne", &[Label])]),
    ("b_lt", &[spell("b.lt", &[Label])]),
    ("b_le", &[spell("b.le", &[Label])]),
    ("b_gt", &[spell("b.gt", &[Label])]),
    ("b_ge", &[spell("b.ge", &[Label])]),
    ("b_lo", &[spell("b.lo", &[Label])]),
    ("b_ls", &[spell("b.ls", &[Label])]),
    ("b_hi", &[spell("b.hi", &[Label])]),
    ("b_hs", &[spell("b.hs", &[Label])]),
    ("b_mi", &[spell("b.mi", &[Label])]),
    ("b_pl", &[spell("b.pl", &[Label])]),
    ("b_vs", &[spell("b.vs", &[Label])]),
    ("b_vc", &[spell("b.vc", &[Label])]),
    ("b_away", &[spell("b", &[Symbol])]),
    ("br", &[spell("br", &[Reg(0, X)])]),
    ("bl", &[spell("bl", &[Symbol])]),
    ("blr", &[spell("blr", &[Through])]),
    ("ret", &[spell("ret", &[])]),
    // A register saved and restored sixteen bytes at a time, since the stack pointer has to stay a
    // multiple of sixteen whenever it is used to reach memory.
    ("push_64", &[spell("str", &[Reg(0, X), Push])]),
    ("pop_64", &[spell("ldr", &[Reg(0, X), Pop])]),
    // Everything else.
    ("nop", &[spell("nop", &[])]),
    ("trap", &[spell("brk", &[Lit(1)])]),
    ("fence", &[spell("dmb", &[Barrier(0b1011)])]),
];

/// The instructions the opcode of that name is written as.
///
/// `None` for a name this target does not have, and an empty slice for one that is an opcode and
/// not an instruction, which are two different answers.
#[must_use]
pub fn written(name: &str) -> Option<&'static [Written]> {
    TEXT.iter().find(|(known, _)| *known == name).map(|&(_, insts)| insts)
}

/// What an allocated instruction has that its arguments are filled in from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Operands<'a> {
    /// The number of the register each operand was given, in the operand's own class.
    pub regs: &'a [u8],
    /// Where the operands the instruction reads start, which is after every one it writes.
    pub reads: usize,
    /// The instruction's constant, or zero for one that has none.
    pub imm: i64,
    /// The instruction's addressing mode, with its registers already allocated.
    pub mem: Option<Addr>,
}

/// Why an argument could not be filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// The argument names an operand the instruction does not have.
    Operand(u8),
    /// The argument is part of an addressing mode and the instruction has none.
    Mem,
}

/// The fifteen bits `mrs` carries for `tpidr_el0`, which is where the thread pointer is.
const TPIDR_EL0: u16 = system_field([3, 3, 13, 0, 2]);

/// One argument as the value [`encode`](crate::aarch64::encode) and
/// [`write`](crate::aarch64::write) both take.
///
/// A symbol or a block comes back as [`Value::Symbol`] and which one it is, and where it is, is
/// the caller's to keep: the encoder answers with the relocation that fills it in.
///
/// # Errors
///
/// When the argument names an operand or an addressing mode the instruction does not have, which
/// is a table that disagrees with the form it is written against.
pub fn fill(arg: Arg, with: &Operands<'_>) -> Result<Value, Missing> {
    let reg = |at: u8| with.regs.get(usize::from(at)).copied().ok_or(Missing::Operand(at));
    let gpr = |width: Width, number: u8| {
        if number == 31 { Value::Sp(width) } else { Value::Gpr(width, number) }
    };
    Ok(match arg {
        Reg(at, width) => gpr(width, reg(at)?),
        Fp(at, scalar) => Value::Fp(scalar, reg(at)?),
        Vector(at) => Value::Vector(Arrangement::B16, reg(at)?),
        Imm => Value::Imm(with.imm),
        Lit(imm) => Value::Imm(imm),
        Arg::Shift(shift, amount) => Value::Shift(shift, amount),
        Arg::Cond(cond) => Value::Cond(cond),
        Barrier(option) => Value::Barrier(option),
        Mem => Value::Mem(with.mem.ok_or(Missing::Mem)?),
        Base => gpr(X, with.mem.ok_or(Missing::Mem)?.base),
        Disp => match with.mem.ok_or(Missing::Mem)?.offset {
            Offset::Imm(imm) => Value::Imm(imm),
            _ => return Err(Missing::Mem),
        },
        Page | Symbol | Label => Value::Symbol(Operator::Plain),
        Low => Value::Symbol(Operator::Lo12),
        GotPage => Value::Symbol(Operator::Got),
        GotSlot(at) => Value::Mem(Addr {
            base: reg(at)?,
            offset: Offset::Symbol(Operator::GotLo12),
            mode: Mode::Offset,
        }),
        TprelHi => Value::Symbol(Operator::TprelHi12),
        TprelLo => Value::Symbol(Operator::TprelLo12Nc),
        Thread => Value::System(TPIDR_EL0),
        Push => Value::Mem(Addr { base: 31, offset: Offset::Imm(-16), mode: Mode::Pre }),
        Pop => Value::Mem(Addr { base: 31, offset: Offset::Imm(16), mode: Mode::Post }),
        Through => {
            let at = u8::try_from(with.reads).map_err(|_| Missing::Operand(u8::MAX))?;
            gpr(X, reg(at)?)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aarch64::insts::{Form, INSTS, form};
    use crate::aarch64::{FPR, GPR, encode, write};
    use crate::operand::Role;

    #[test]
    fn every_opcode_is_described_and_written_and_nothing_else_is() {
        for &(name, _) in INSTS {
            assert!(written(name).is_some(), "{name} has a form and is not written");
        }
        for &(name, _) in TEXT {
            assert!(form(name).is_some(), "{name} is written and has no form");
        }
    }

    #[test]
    fn every_argument_names_an_operand_of_the_file_it_is_written_as() {
        for &(name, form) in INSTS {
            let operands = form.operands();
            for inst in written(name).unwrap() {
                for arg in inst.args {
                    let (at, class) = match *arg {
                        Reg(at, _) | GotSlot(at) => (at, GPR),
                        Fp(at, _) | Vector(at) => (at, FPR),
                        Mem | Base | Disp => {
                            assert!(form.takes_mem(), "{name} names an address it has not got");
                            continue;
                        }
                        Imm => {
                            assert!(form.takes_imm(), "{name} names a constant it has not got");
                            continue;
                        }
                        _ => continue,
                    };
                    let operand = operands.get(usize::from(at));
                    assert_eq!(operand.map(|op| op.class), Some(class), "{name} operand {at}");
                }
            }
        }
    }

    /// The operands an instruction of that form is filled in from in the test below: every
    /// register a different one and none of them the stack pointer, and a constant and an address
    /// every instruction here can hold.
    ///
    /// Four registers whatever the form says, because a call's operands are the arguments it
    /// passes and those are the lowering's to add rather than the form's.
    fn sample(form: Form, regs: &mut Vec<u8>) -> Operands<'_> {
        regs.clear();
        regs.extend(1..=4);
        let reads = form.operands().iter().take_while(|op| op.role != Role::Use).count();
        let mem = Addr { base: 9, offset: Offset::Imm(16), mode: Mode::Offset };
        Operands { regs, reads, imm: 1, mem: Some(mem) }
    }

    #[test]
    fn every_instruction_every_opcode_is_written_as_is_one_the_encoder_writes() {
        let mut wrong = Vec::new();
        let mut regs = Vec::new();
        for &(name, form) in INSTS {
            let with = sample(form, &mut regs);
            for inst in written(name).unwrap() {
                let values: Vec<Value> =
                    inst.args.iter().map(|&arg| fill(arg, &with).unwrap()).collect();
                if let Err(e) = encode(inst.mnemonic, &values) {
                    wrong
                        .push(format!("{name}: {}: {e}", write(inst.mnemonic, &values, Some("s"))));
                }
            }
        }
        assert!(wrong.is_empty(), "{} do not encode:\n{}", wrong.len(), wrong.join("\n"));
    }

    #[test]
    fn every_instruction_is_the_word_gnu_as_writes_for_the_line_it_is_listed_as() {
        let mut want = include_str!("listing.txt").lines().filter(|line| !line.starts_with('#'));
        let mut wrong = Vec::new();
        let mut regs = Vec::new();
        for &(name, form) in INSTS {
            let with = sample(form, &mut regs);
            for inst in written(name).unwrap() {
                let values: Vec<Value> =
                    inst.args.iter().map(|&arg| fill(arg, &with).unwrap()).collect();
                let word = encode(inst.mnemonic, &values).unwrap().word;
                let got =
                    format!("{word:08x}\t{}\t{name}", write(inst.mnemonic, &values, Some("s")));
                match want.next() {
                    Some(line) if line == got => {}
                    line => wrong.push(format!("{got}, GNU as: {line:?}")),
                }
            }
        }
        assert_eq!(want.next(), None, "listing.txt has lines no opcode is written as");
        assert!(wrong.is_empty(), "{} lines differ:\n{}", wrong.len(), wrong.join("\n"));
    }

    fn listing(name: &str, with: &Operands<'_>) -> Vec<String> {
        written(name)
            .unwrap()
            .iter()
            .map(|inst| {
                let values: Vec<Value> =
                    inst.args.iter().map(|&arg| fill(arg, with).unwrap()).collect();
                write(inst.mnemonic, &values, Some("s"))
            })
            .collect()
    }

    #[test]
    fn an_opcode_is_listed_as_the_instructions_a_reader_expects() {
        let with = Operands { regs: &[0, 1, 2, 3], reads: 1, imm: 42, mem: None };
        assert_eq!(listing("cmp_set_lt_64", &with), ["cmp x1, x2", "cset w0, lt"]);
        assert_eq!(listing("fcmp_set_lt_f64", &with), ["fcmp d1, d2", "cset w0, mi"]);
        assert_eq!(listing("sel_32", &with), ["cmp w3, #0", "csel w0, w2, w1, ne"]);
        assert_eq!(listing("movk_ri_32_64", &with), ["movk x0, #42, lsl #32"]);
        assert_eq!(listing("addr_64", &with), ["adrp x0, s", "add x0, x0, :lo12:s"]);
        assert_eq!(listing("got_64", &with), ["adrp x0, :got:s", "ldr x0, [x0, :got_lo12:s]"]);
        assert_eq!(
            listing("tls_64", &with),
            [
                "mrs x0, tpidr_el0",
                "add x0, x0, :tprel_hi12:s, lsl #12",
                "add x0, x0, :tprel_lo12_nc:s"
            ]
        );
        assert_eq!(listing("blr", &with), ["blr x1"]);
        assert_eq!(listing("push_64", &with), ["str x0, [sp, #-16]!"]);
        assert_eq!(listing("pop_64", &with), ["ldr x0, [sp], #16"]);
        assert!(listing("arg_val_64", &with).is_empty());
    }

    #[test]
    fn the_stack_pointer_is_written_where_the_machine_has_a_way_to_say_it() {
        let sp = Operands {
            regs: &[31, 31, 9],
            reads: 1,
            imm: 32,
            mem: Some(Addr { base: 31, offset: Offset::Imm(24), mode: Mode::Offset }),
        };
        assert_eq!(listing("add_ri_64", &sp), ["add sp, sp, #32"]);
        assert_eq!(listing("sub_ri_64", &sp), ["sub sp, sp, #32"]);
        assert_eq!(listing("mov_rr_64", &sp), ["mov sp, sp"]);
        assert_eq!(listing("lea_64", &sp), ["add sp, sp, #24"]);
        for name in ["add_ri_64", "sub_ri_64", "sub_rr_64", "mov_rr_64", "lea_64"] {
            for inst in written(name).unwrap() {
                let values: Vec<Value> =
                    inst.args.iter().map(|&arg| fill(arg, &sp).unwrap()).collect();
                assert!(encode(inst.mnemonic, &values).is_ok(), "{name} with the stack pointer");
            }
        }
        let load = Operands { regs: &[9], ..sp };
        assert_eq!(listing("ldr_64", &load), ["ldr x9, [sp, #24]"]);
        assert_eq!(listing("str_f64", &load), ["str d9, [sp, #24]"]);
    }

    #[test]
    fn the_thread_pointer_is_tpidr_el0() {
        let line = crate::aarch64::read("mrs x0, tpidr_el0").unwrap();
        assert_eq!(line.values[1], Value::System(TPIDR_EL0));
    }
}
