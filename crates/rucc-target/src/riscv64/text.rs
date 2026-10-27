//! What each RISC-V machine instruction is in assembly, and what it hands the encoder.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1.
//!
//! The arrangement of `rucc_target::aarch64::written`: an opcode is a list of instructions, each
//! argument names the operand it wants by index, and [`fill`] turns every argument into the
//! [`Value`] the encoder takes, so `-S` and the object writer are handed the same values. The
//! order is the order GNU as reads them, destination first, which is the order the operand vector
//! holds them in.
//!
//! A register argument carries how many bits of the register the opcode reads or writes, which is
//! not in the text, since every integer register is named the same at every width. It is what
//! [`operand_width`] answers, and it is the width of the value rather than of the register: the
//! source of `sext.w` is thirty two bits and the source of a byte's widening is eight.

use std::sync::LazyLock;

use crate::named;
use crate::riscv64::encode::{Addr, Offset, Operator, Round, Value};

use Arg::{Base, Disp, Fixed, Fp, Imm, ImmPlus, Label, Lit, Mem, Near, Reg, Sp, Symbol, Through};

/// A byte.
const B: u8 = 8;
/// A half, sixteen bits.
const H: u8 = 16;
/// A word, thirty two bits.
const W: u8 = 32;
/// The whole register.
const X: u8 = 64;

/// The stack pointer, the thread pointer and the zero register by number.
const SP: u8 = 2;
const TP: u8 = 4;
const ZERO: u8 = 0;

/// One argument of one instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    /// The integer register the operand at that index was given, and how many bits of it count.
    Reg(u8, u8),
    /// The floating point register the operand at that index was given.
    Fp(u8),
    /// An integer register that is part of the opcode rather than one the allocator chose.
    Fixed(u8),
    /// The instruction's constant.
    Imm,
    /// The instruction's constant with that added, for a shift that moves the bits up first.
    ImmPlus(i64),
    /// A constant that is part of the opcode rather than of the instruction.
    Lit(i64),
    /// The instruction's addressing mode.
    Mem,
    /// The base register of the instruction's addressing mode, as a register on its own.
    Base,
    /// The constant of the instruction's addressing mode, as a constant on its own.
    Disp,
    /// That many bytes above the stack pointer, which is where a push and a pop put things.
    Sp(i64),
    /// The instruction's symbol, as a call or an address reaches it.
    Symbol,
    /// The block a branch goes to.
    Label,
    /// The instruction that many places away in the same opcode, counted in instructions, which is
    /// where a branch inside one opcode goes. Every instruction here is four bytes, so it is a
    /// distance the encoder takes as it is and the listing writes as one from `.`.
    Near(i8),
    /// The first register the instruction reads, which is what a call through a register reads.
    Through,
    /// How a floating point instruction rounds, when it is not the default.
    Round(Round),
    /// The accesses a `fence` orders on either side, as the encoding's four bits.
    Order(u8),
}

/// One machine instruction an opcode is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    /// The mnemonic, as GNU as reads it, or one of the encoder's own pseudo instructions.
    pub mnemonic: &'static str,
    /// The arguments, in the order GNU as reads them.
    pub args: &'static [Arg],
}

const fn spell(mnemonic: &'static str, args: &'static [Arg]) -> Written {
    Written { mnemonic, args }
}

const RRR_W: &[Arg] = &[Reg(0, W), Reg(1, W), Reg(2, W)];
const RRR_X: &[Arg] = &[Reg(0, X), Reg(1, X), Reg(2, X)];
const RRI_W: &[Arg] = &[Reg(0, W), Reg(1, W), Imm];
const RRI_X: &[Arg] = &[Reg(0, X), Reg(1, X), Imm];
const RR_W: &[Arg] = &[Reg(0, W), Reg(1, W)];
// A word that comes out of an instruction of the whole register, `and` or `mv` or `not`, is only
// sign extended if what went in was, so those read every bit of their sources.
const RRR_WX: &[Arg] = &[Reg(0, W), Reg(1, X), Reg(2, X)];
const RRI_WX: &[Arg] = &[Reg(0, W), Reg(1, X), Imm];
const RR_WX: &[Arg] = &[Reg(0, W), Reg(1, X)];
const RR_X: &[Arg] = &[Reg(0, X), Reg(1, X)];
const FFF: &[Arg] = &[Fp(0), Fp(1), Fp(2)];
const FF: &[Arg] = &[Fp(0), Fp(1)];
/// A comparison of two floating point registers, and the same the other way round.
const RFF: &[Arg] = &[Reg(0, X), Fp(1), Fp(2)];
const RFF_SWAPPED: &[Arg] = &[Reg(0, X), Fp(2), Fp(1)];
/// The answer of a comparison turned round, which is what makes the opposite of one.
const FLIP: Written = spell("xori", &[Reg(0, X), Reg(0, X), Lit(1)]);

static TEXT: &[(&str, &[Written])] = &[
    // Constants.
    ("li_32", &[spell("li", &[Reg(0, W), Imm])]),
    ("li_64", &[spell("li", &[Reg(0, X), Imm])]),
    // Copies.
    ("mv_rr_32", &[spell("mv", RR_WX)]),
    ("mv_rr_64", &[spell("mv", RR_X)]),
    // Arithmetic on two registers.
    ("add_rr_32", &[spell("addw", RRR_W)]),
    ("add_rr_64", &[spell("add", RRR_X)]),
    ("sub_rr_32", &[spell("subw", RRR_W)]),
    ("sub_rr_64", &[spell("sub", RRR_X)]),
    ("and_rr_32", &[spell("and", RRR_WX)]),
    ("and_rr_64", &[spell("and", RRR_X)]),
    ("or_rr_32", &[spell("or", RRR_WX)]),
    ("or_rr_64", &[spell("or", RRR_X)]),
    ("xor_rr_32", &[spell("xor", RRR_WX)]),
    ("xor_rr_64", &[spell("xor", RRR_X)]),
    ("mul_rr_32", &[spell("mulw", RRR_W)]),
    ("mul_rr_64", &[spell("mul", RRR_X)]),
    ("div_rr_32", &[spell("divw", RRR_W)]),
    ("div_rr_64", &[spell("div", RRR_X)]),
    ("divu_rr_32", &[spell("divuw", RRR_W)]),
    ("divu_rr_64", &[spell("divu", RRR_X)]),
    ("rem_rr_32", &[spell("remw", RRR_W)]),
    ("rem_rr_64", &[spell("rem", RRR_X)]),
    ("remu_rr_32", &[spell("remuw", RRR_W)]),
    ("remu_rr_64", &[spell("remu", RRR_X)]),
    ("sll_rr_32", &[spell("sllw", RRR_W)]),
    ("sll_rr_64", &[spell("sll", RRR_X)]),
    ("srl_rr_32", &[spell("srlw", RRR_W)]),
    ("srl_rr_64", &[spell("srl", RRR_X)]),
    ("sra_rr_32", &[spell("sraw", RRR_W)]),
    ("sra_rr_64", &[spell("sra", RRR_X)]),
    ("mulh_rr_64", &[spell("mulh", RRR_X)]),
    ("mulhu_rr_64", &[spell("mulhu", RRR_X)]),
    // Arithmetic on a register and a constant.
    ("add_ri_32", &[spell("addiw", RRI_W)]),
    ("add_ri_64", &[spell("addi", RRI_X)]),
    ("and_ri_32", &[spell("andi", RRI_WX)]),
    ("and_ri_64", &[spell("andi", RRI_X)]),
    ("or_ri_32", &[spell("ori", RRI_WX)]),
    ("or_ri_64", &[spell("ori", RRI_X)]),
    ("xor_ri_32", &[spell("xori", RRI_WX)]),
    ("xor_ri_64", &[spell("xori", RRI_X)]),
    ("sll_ri_32", &[spell("slliw", RRI_W)]),
    ("sll_ri_64", &[spell("slli", RRI_X)]),
    ("srl_ri_32", &[spell("srliw", RRI_W)]),
    ("srl_ri_64", &[spell("srli", RRI_X)]),
    ("sra_ri_32", &[spell("sraiw", RRI_W)]),
    ("sra_ri_64", &[spell("srai", RRI_X)]),
    (
        "srl_ri_8",
        &[
            spell("slli", &[Reg(0, W), Reg(1, B), Lit(56)]),
            spell("srli", &[Reg(0, W), Reg(0, W), ImmPlus(56)]),
        ],
    ),
    (
        "srl_ri_16",
        &[
            spell("slli", &[Reg(0, W), Reg(1, H), Lit(48)]),
            spell("srli", &[Reg(0, W), Reg(0, W), ImmPlus(48)]),
        ],
    ),
    (
        "sra_ri_8",
        &[
            spell("slli", &[Reg(0, W), Reg(1, B), Lit(56)]),
            spell("srai", &[Reg(0, W), Reg(0, W), ImmPlus(56)]),
        ],
    ),
    (
        "sra_ri_16",
        &[
            spell("slli", &[Reg(0, W), Reg(1, H), Lit(48)]),
            spell("srai", &[Reg(0, W), Reg(0, W), ImmPlus(48)]),
        ],
    ),
    // One register to one register.
    ("neg_r_32", &[spell("negw", RR_W)]),
    ("neg_r_64", &[spell("neg", RR_X)]),
    ("not_r_32", &[spell("not", RR_WX)]),
    ("not_r_64", &[spell("not", RR_X)]),
    // Widening. RV64GC has no instruction for a byte or a half, so those move the bits to the
    // top of the register and back down.
    (
        "sextb_16",
        &[
            spell("slli", &[Reg(0, H), Reg(1, B), Lit(56)]),
            spell("srai", &[Reg(0, H), Reg(0, H), Lit(56)]),
        ],
    ),
    (
        "sextb_32",
        &[
            spell("slli", &[Reg(0, W), Reg(1, B), Lit(56)]),
            spell("srai", &[Reg(0, W), Reg(0, W), Lit(56)]),
        ],
    ),
    (
        "sextb_64",
        &[
            spell("slli", &[Reg(0, X), Reg(1, B), Lit(56)]),
            spell("srai", &[Reg(0, X), Reg(0, X), Lit(56)]),
        ],
    ),
    (
        "sexth_32",
        &[
            spell("slli", &[Reg(0, W), Reg(1, H), Lit(48)]),
            spell("srai", &[Reg(0, W), Reg(0, W), Lit(48)]),
        ],
    ),
    (
        "sexth_64",
        &[
            spell("slli", &[Reg(0, X), Reg(1, H), Lit(48)]),
            spell("srai", &[Reg(0, X), Reg(0, X), Lit(48)]),
        ],
    ),
    ("sextw_64", &[spell("sext.w", &[Reg(0, X), Reg(1, W)])]),
    ("zextb_16", &[spell("andi", &[Reg(0, H), Reg(1, B), Lit(255)])]),
    ("zextb_32", &[spell("andi", &[Reg(0, W), Reg(1, B), Lit(255)])]),
    ("zextb_64", &[spell("andi", &[Reg(0, X), Reg(1, B), Lit(255)])]),
    (
        "zexth_32",
        &[
            spell("slli", &[Reg(0, W), Reg(1, H), Lit(48)]),
            spell("srli", &[Reg(0, W), Reg(0, W), Lit(48)]),
        ],
    ),
    (
        "zexth_64",
        &[
            spell("slli", &[Reg(0, X), Reg(1, H), Lit(48)]),
            spell("srli", &[Reg(0, X), Reg(0, X), Lit(48)]),
        ],
    ),
    (
        "zextw_64",
        &[
            spell("slli", &[Reg(0, X), Reg(1, W), Lit(32)]),
            spell("srli", &[Reg(0, X), Reg(0, X), Lit(32)]),
        ],
    ),
    ("bit_to_8", &[spell("andi", &[Reg(0, B), Reg(1, B), Lit(1)])]),
    ("bit_to_16", &[spell("andi", &[Reg(0, H), Reg(1, B), Lit(1)])]),
    ("bit_to_32", &[spell("andi", &[Reg(0, W), Reg(1, B), Lit(1)])]),
    ("bit_to_64", &[spell("andi", &[Reg(0, X), Reg(1, B), Lit(1)])]),
    // Narrowing. Above a byte or a half nothing is promised, so those are a copy. A word is kept
    // sign extended, which is what every reader of one on this machine counts on.
    ("low_8", &[spell("mv", &[Reg(0, B), Reg(1, B)])]),
    ("low_16", &[spell("mv", &[Reg(0, H), Reg(1, H)])]),
    ("low_32", &[spell("sext.w", &[Reg(0, W), Reg(1, W)])]),
    ("bit_of_32", &[spell("andi", &[Reg(0, W), Reg(1, W), Imm])]),
    ("bit_of_64", &[spell("andi", &[Reg(0, W), Reg(1, X), Imm])]),
    // Comparisons that keep their answer. Equality is whether the difference is zero, which for
    // thirty two bits is the `w` subtraction, whose answer is zero exactly when the low halves
    // agree. An answer that is the opposite of `slt` is `slt` with the bit turned over.
    (
        "cmp_set_eq_32",
        &[
            spell("subw", &[Reg(0, X), Reg(1, W), Reg(2, W)]),
            spell("seqz", &[Reg(0, X), Reg(0, X)]),
        ],
    ),
    ("cmp_set_eq_64", &[spell("sub", RRR_X), spell("seqz", RR_X0)]),
    (
        "cmp_set_ne_32",
        &[
            spell("subw", &[Reg(0, X), Reg(1, W), Reg(2, W)]),
            spell("snez", &[Reg(0, X), Reg(0, X)]),
        ],
    ),
    ("cmp_set_ne_64", &[spell("sub", RRR_X), spell("snez", RR_X0)]),
    ("cmp_set_lt_64", &[spell("slt", RRR_X)]),
    ("cmp_set_le_64", &[spell("slt", RRR_X_SWAPPED), FLIP]),
    ("cmp_set_gt_64", &[spell("slt", RRR_X_SWAPPED)]),
    ("cmp_set_ge_64", &[spell("slt", RRR_X), FLIP]),
    ("cmp_set_lo_64", &[spell("sltu", RRR_X)]),
    ("cmp_set_ls_64", &[spell("sltu", RRR_X_SWAPPED), FLIP]),
    ("cmp_set_hi_64", &[spell("sltu", RRR_X_SWAPPED)]),
    ("cmp_set_hs_64", &[spell("sltu", RRR_X), FLIP]),
    // A thirty two bit value is kept sign extended in its register, so the comparisons of the
    // whole register give the answer for the low half, signed and unsigned. They read the whole
    // register, so a `sext.w` in front of one has to stay even when nothing reads above the word.
    ("cmp_set_lt_32", &[spell("slt", RRR_X)]),
    ("cmp_set_le_32", &[spell("slt", RRR_X_SWAPPED), FLIP]),
    ("cmp_set_gt_32", &[spell("slt", RRR_X_SWAPPED)]),
    ("cmp_set_ge_32", &[spell("slt", RRR_X), FLIP]),
    ("cmp_set_lo_32", &[spell("sltu", RRR_X)]),
    ("cmp_set_ls_32", &[spell("sltu", RRR_X_SWAPPED), FLIP]),
    ("cmp_set_hi_32", &[spell("sltu", RRR_X_SWAPPED)]),
    ("cmp_set_hs_32", &[spell("sltu", RRR_X), FLIP]),
    // The second source when the byte is not zero, and the first when it is.
    (
        "sel_32",
        &[
            spell("mv", &[Reg(0, W), Reg(2, X)]),
            spell("bnez", &[Reg(3, B), Near(2)]),
            spell("mv", &[Reg(0, W), Reg(1, X)]),
        ],
    ),
    (
        "sel_64",
        &[
            spell("mv", &[Reg(0, X), Reg(2, X)]),
            spell("bnez", &[Reg(3, B), Near(2)]),
            spell("mv", &[Reg(0, X), Reg(1, X)]),
        ],
    ),
    // Not an instruction this machine has, and so not one the encoder writes. See the header of
    // `crate::riscv64::insts`.
    ("test", &[spell("test", &[Reg(0, B)])]),
    // Addresses.
    ("lea_64", &[spell("addi", &[Reg(0, X), Base, Disp])]),
    ("lea_far_64", &[spell("lea.far", &[Reg(0, X), Mem])]),
    ("addr_64", &[spell("lla", &[Reg(0, X), Symbol])]),
    ("got_64", &[spell("la", &[Reg(0, X), Symbol])]),
    ("gottprel_64", &[spell("la.tls.ie", &[Reg(0, X), Symbol])]),
    ("thread_64", &[spell("mv", &[Reg(0, X), Fixed(TP)])]),
    // Loads. A load of thirty two bits sign extends, which is the right answer for an `int` and a
    // harmless one for anything that reads only the low half.
    ("ld_8", &[spell("lbu", &[Reg(0, X), Mem])]),
    ("ld_bit", &[spell("lbu", &[Reg(0, B), Mem])]),
    ("ld_16", &[spell("lhu", &[Reg(0, X), Mem])]),
    ("ld_32", &[spell("lw", &[Reg(0, W), Mem])]),
    ("ld_64", &[spell("ld", &[Reg(0, X), Mem])]),
    ("lds_8", &[spell("lb", &[Reg(0, X), Mem])]),
    ("lds_16", &[spell("lh", &[Reg(0, X), Mem])]),
    ("lds_32", &[spell("lw", &[Reg(0, X), Mem])]),
    ("ldu_32", &[spell("lwu", &[Reg(0, X), Mem])]),
    // Stores.
    ("st_8", &[spell("sb", &[Reg(0, B), Mem])]),
    ("st_bit", &[spell("sb", &[Reg(0, B), Mem])]),
    ("st_16", &[spell("sh", &[Reg(0, H), Mem])]),
    ("st_32", &[spell("sw", &[Reg(0, W), Mem])]),
    ("st_64", &[spell("sd", &[Reg(0, X), Mem])]),
    ("ld_f32", &[spell("flw", &[Fp(0), Mem])]),
    ("ld_f64", &[spell("fld", &[Fp(0), Mem])]),
    ("st_f32", &[spell("fsw", &[Fp(0), Mem])]),
    ("st_f64", &[spell("fsd", &[Fp(0), Mem])]),
    // Floating point arithmetic.
    ("fadd_f32", &[spell("fadd.s", FFF)]),
    ("fadd_f64", &[spell("fadd.d", FFF)]),
    ("fsub_f32", &[spell("fsub.s", FFF)]),
    ("fsub_f64", &[spell("fsub.d", FFF)]),
    ("fmul_f32", &[spell("fmul.s", FFF)]),
    ("fmul_f64", &[spell("fmul.d", FFF)]),
    ("fdiv_f32", &[spell("fdiv.s", FFF)]),
    ("fdiv_f64", &[spell("fdiv.d", FFF)]),
    ("fmin_f32", &[spell("fmin.s", FFF)]),
    ("fmin_f64", &[spell("fmin.d", FFF)]),
    ("fmax_f32", &[spell("fmax.s", FFF)]),
    ("fmax_f64", &[spell("fmax.d", FFF)]),
    ("fmadd_f32", &[spell("fmadd.s", &[Fp(0), Fp(1), Fp(2), Fp(3)])]),
    ("fmadd_f64", &[spell("fmadd.d", &[Fp(0), Fp(1), Fp(2), Fp(3)])]),
    ("fsqrt_f32", &[spell("fsqrt.s", FF)]),
    ("fsqrt_f64", &[spell("fsqrt.d", FF)]),
    ("fneg_f32", &[spell("fneg.s", FF)]),
    ("fneg_f64", &[spell("fneg.d", FF)]),
    ("fabs_f32", &[spell("fabs.s", FF)]),
    ("fabs_f64", &[spell("fabs.d", FF)]),
    ("fmv_f32", &[spell("fmv.s", FF)]),
    ("fmv_f64", &[spell("fmv.d", FF)]),
    ("fzero_f64", &[spell("fmv.d.x", &[Fp(0), Fixed(0)])]),
    ("fcvt_f32_f64", &[spell("fcvt.d.s", FF)]),
    ("fcvt_f64_f32", &[spell("fcvt.s.d", FF)]),
    ("scvtf_32_f32", &[spell("fcvt.s.w", &[Fp(0), Reg(1, W)])]),
    ("scvtf_32_f64", &[spell("fcvt.d.w", &[Fp(0), Reg(1, W)])]),
    ("scvtf_64_f32", &[spell("fcvt.s.l", &[Fp(0), Reg(1, X)])]),
    ("scvtf_64_f64", &[spell("fcvt.d.l", &[Fp(0), Reg(1, X)])]),
    ("ucvtf_32_f32", &[spell("fcvt.s.wu", &[Fp(0), Reg(1, W)])]),
    ("ucvtf_32_f64", &[spell("fcvt.d.wu", &[Fp(0), Reg(1, W)])]),
    ("ucvtf_64_f32", &[spell("fcvt.s.lu", &[Fp(0), Reg(1, X)])]),
    ("ucvtf_64_f64", &[spell("fcvt.d.lu", &[Fp(0), Reg(1, X)])]),
    ("fcvtzs_f32_32", &[spell("fcvt.w.s", &[Reg(0, W), Fp(1), Arg::Round(Round::Rtz)])]),
    ("fcvtzs_f32_64", &[spell("fcvt.l.s", &[Reg(0, X), Fp(1), Arg::Round(Round::Rtz)])]),
    ("fcvtzs_f64_32", &[spell("fcvt.w.d", &[Reg(0, W), Fp(1), Arg::Round(Round::Rtz)])]),
    ("fcvtzs_f64_64", &[spell("fcvt.l.d", &[Reg(0, X), Fp(1), Arg::Round(Round::Rtz)])]),
    ("fcvtzu_f32_32", &[spell("fcvt.wu.s", &[Reg(0, W), Fp(1), Arg::Round(Round::Rtz)])]),
    ("fcvtzu_f32_64", &[spell("fcvt.lu.s", &[Reg(0, X), Fp(1), Arg::Round(Round::Rtz)])]),
    ("fcvtzu_f64_32", &[spell("fcvt.wu.d", &[Reg(0, W), Fp(1), Arg::Round(Round::Rtz)])]),
    ("fcvtzu_f64_64", &[spell("fcvt.lu.d", &[Reg(0, X), Fp(1), Arg::Round(Round::Rtz)])]),
    ("fmov_to_f32", &[spell("fmv.w.x", &[Fp(0), Reg(1, W)])]),
    ("fmov_to_f64", &[spell("fmv.d.x", &[Fp(0), Reg(1, X)])]),
    ("fmov_from_f32", &[spell("fmv.x.w", &[Reg(0, W), Fp(1)])]),
    ("fmov_from_f64", &[spell("fmv.x.d", &[Reg(0, X), Fp(1)])]),
    // Floating point comparisons. Greater is less the other way round, and each unordered one is
    // the opposite of the ordered one it is not: `a < b` unordered is not `a >= b`.
    ("fcmp_set_eq_f32", &[spell("feq.s", RFF)]),
    ("fcmp_set_eq_f64", &[spell("feq.d", RFF)]),
    ("fcmp_set_ne_f32", &[spell("feq.s", RFF), FLIP]),
    ("fcmp_set_ne_f64", &[spell("feq.d", RFF), FLIP]),
    ("fcmp_set_lt_f32", &[spell("flt.s", RFF)]),
    ("fcmp_set_lt_f64", &[spell("flt.d", RFF)]),
    ("fcmp_set_le_f32", &[spell("fle.s", RFF)]),
    ("fcmp_set_le_f64", &[spell("fle.d", RFF)]),
    ("fcmp_set_gt_f32", &[spell("flt.s", RFF_SWAPPED)]),
    ("fcmp_set_gt_f64", &[spell("flt.d", RFF_SWAPPED)]),
    ("fcmp_set_ge_f32", &[spell("fle.s", RFF_SWAPPED)]),
    ("fcmp_set_ge_f64", &[spell("fle.d", RFF_SWAPPED)]),
    ("fcmp_set_ult_f32", &[spell("fle.s", RFF_SWAPPED), FLIP]),
    ("fcmp_set_ult_f64", &[spell("fle.d", RFF_SWAPPED), FLIP]),
    ("fcmp_set_ule_f32", &[spell("flt.s", RFF_SWAPPED), FLIP]),
    ("fcmp_set_ule_f64", &[spell("flt.d", RFF_SWAPPED), FLIP]),
    ("fcmp_set_ugt_f32", &[spell("fle.s", RFF), FLIP]),
    ("fcmp_set_ugt_f64", &[spell("fle.d", RFF), FLIP]),
    ("fcmp_set_uge_f32", &[spell("flt.s", RFF), FLIP]),
    ("fcmp_set_uge_f64", &[spell("flt.d", RFF), FLIP]),
    // Values arriving and leaving, which are written as nothing.
    ("ret_val_32", &[]),
    ("ret_val_64", &[]),
    ("ret_val_f32", &[]),
    ("ret_val_f64", &[]),
    ("ret_val2_32", &[]),
    ("ret_val2_64", &[]),
    ("ret_val2_f32", &[]),
    ("ret_val2_f64", &[]),
    ("arg_val_32", &[]),
    ("arg_val_64", &[]),
    ("arg_val_f32", &[]),
    ("arg_val_f64", &[]),
    ("br_cond_32", &[]),
    // Branches, calls and the return. The two jumps on a test are not instructions this machine
    // has, for the reason the test is not.
    ("j", &[spell("j", &[Label])]),
    ("b_eq", &[spell("b.eq", &[Label])]),
    ("b_ne", &[spell("b.ne", &[Label])]),
    ("beqz", &[spell("beqz", &[Reg(0, X), Label])]),
    ("bnez", &[spell("bnez", &[Reg(0, X), Label])]),
    ("tail", &[spell("tail", &[Symbol])]),
    ("jr", &[spell("jr", &[Reg(0, X)])]),
    ("call", &[spell("call", &[Symbol])]),
    ("call_reg", &[spell("jalr", &[Through])]),
    ("ret", &[spell("ret", &[])]),
    // The stack, sixteen bytes at a time.
    (
        "push_64",
        &[spell("addi", &[Fixed(SP), Fixed(SP), Lit(-16)]), spell("sd", &[Reg(0, X), Sp(0)])],
    ),
    (
        "pop_64",
        &[spell("ld", &[Reg(0, X), Sp(0)]), spell("addi", &[Fixed(SP), Fixed(SP), Lit(16)])],
    ),
    (
        "push_pair_64",
        &[
            spell("addi", &[Fixed(SP), Fixed(SP), Lit(-16)]),
            spell("sd", &[Reg(0, X), Sp(0)]),
            spell("sd", &[Reg(1, X), Sp(8)]),
        ],
    ),
    (
        "pop_pair_64",
        &[
            spell("ld", &[Reg(0, X), Sp(0)]),
            spell("ld", &[Reg(1, X), Sp(8)]),
            spell("addi", &[Fixed(SP), Fixed(SP), Lit(16)]),
        ],
    ),
    ("add_far_64", &[spell("addi.far", RRI_X)]),
    ("sub_far_64", &[spell("subi.far", RRI_X)]),
    ("align_sp_64", &[spell("andi", RRI_X)]),
    ("probe_64", &[spell("sd", &[Fixed(ZERO), Mem])]),
    ("nop", &[spell("nop", &[])]),
    ("trap", &[spell("ebreak", &[])]),
    // Reads and writes before it ordered with reads and writes after it, which is what a C
    // fence asks for.
    ("fence", &[spell("fence", &[Arg::Order(3), Arg::Order(3)])]),
    // Nothing from the table. The text it carries is the program's and the writer puts it down.
    ("template", &[]),
];

const RR_X0: &[Arg] = &[Reg(0, X), Reg(0, X)];
const RRR_X_SWAPPED: &[Arg] = &[Reg(0, X), Reg(2, X), Reg(1, X)];

/// The instructions the opcode of that name is written as.
///
/// `None` for a name this target does not have, and an empty slice for one that is an opcode and
/// not an instruction, which are two different answers.
#[must_use]
pub fn written(name: &str) -> Option<&'static [Written]> {
    static AT: LazyLock<named::Names<usize>> = LazyLock::new(|| named::index(TEXT));
    AT.get(name).map(|&at| TEXT[at].1)
}

/// How many bits of the operand at that index the opcode of that name names, or `None` for all
/// of it.
///
/// `None` for an operand no instruction of the opcode names as an integer register, and for every
/// operand of a name this target does not have. The widest where one operand is named more than
/// once, since that is how much of it something reads. This is [`crate::BitInsts::width`] for
/// this target.
#[must_use]
pub fn operand_width(name: &str, operand: u8) -> Option<u32> {
    let mut found: Option<u32> = None;
    for inst in written(name)? {
        for arg in inst.args {
            let bits = match *arg {
                Reg(at, bits) if at == operand => u32::from(bits),
                Fp(at) if at == operand => return None,
                _ => continue,
            };
            found = Some(found.map_or(bits, |had: u32| had.max(bits)));
        }
    }
    found
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

/// One argument as the value [`encode`](crate::riscv64::encode) and
/// [`write`](crate::riscv64::write) both take.
///
/// A symbol or a block comes back as [`Value::Symbol`], and which one it is is the caller's to
/// keep, as on AArch64.
///
/// # Errors
///
/// When the argument names an operand or an addressing mode the instruction does not have, which
/// is a table that disagrees with the form it is written against.
pub fn fill(arg: Arg, with: &Operands<'_>) -> Result<Value, Missing> {
    let reg = |at: u8| with.regs.get(usize::from(at)).copied().ok_or(Missing::Operand(at));
    let mem = || with.mem.ok_or(Missing::Mem);
    Ok(match arg {
        Reg(at, _) => Value::Gpr(reg(at)?),
        Fp(at) => Value::Fpr(reg(at)?),
        Fixed(number) => Value::Gpr(number),
        Imm => Value::Imm(with.imm),
        ImmPlus(more) => Value::Imm(with.imm.saturating_add(more)),
        Lit(imm) => Value::Imm(imm),
        Mem => Value::Mem(mem()?),
        Base => Value::Gpr(mem()?.base),
        Disp => match mem()?.offset {
            Offset::Imm(imm) => Value::Imm(imm),
            Offset::Symbol(operator) => Value::Symbol(operator),
        },
        Sp(offset) => Value::Mem(Addr { base: SP, offset: Offset::Imm(offset) }),
        Symbol | Label => Value::Symbol(Operator::Plain),
        Near(count) => Value::Imm(i64::from(count) * 4),
        Through => {
            let at = u8::try_from(with.reads).map_err(|_| Missing::Operand(u8::MAX))?;
            Value::Gpr(reg(at)?)
        }
        Arg::Round(round) => Value::Round(round),
        Arg::Order(order) => Value::Order(order),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operand::Role;
    use crate::riscv64::insts::{Form, INSTS, form};
    use crate::riscv64::{FPR, GPR, encode, write};

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
                        Reg(at, _) => (at, GPR),
                        Fp(at) => (at, FPR),
                        Mem | Base | Disp => {
                            assert!(form.takes_mem(), "{name} names an address it has not got");
                            continue;
                        }
                        Imm | ImmPlus(_) => {
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

    /// Operands to fill an instruction of that form in from: every register a different one and
    /// none of them a register with a job, a constant every instruction here can hold, and an
    /// address with a small offset.
    fn sample(form: Form, regs: &mut Vec<u8>) -> Operands<'_> {
        regs.clear();
        regs.extend(10..=14);
        let reads = form.operands().iter().take_while(|op| op.role != Role::Use).count();
        let mem = Addr { base: 8, offset: Offset::Imm(16) };
        Operands { regs, reads, imm: 1, mem: Some(mem) }
    }

    #[test]
    fn every_instruction_every_opcode_is_written_as_is_one_the_encoder_writes() {
        let mut wrong = Vec::new();
        let mut regs = Vec::new();
        for &(name, form) in INSTS {
            // The test and the two jumps on it, which the layout folds into `beqz` and `bnez`
            // every time, and which are spelled so that the encoder refuses one left over.
            if matches!(form, Form::Test | Form::Jcc) {
                continue;
            }
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
    fn a_leftover_test_or_jump_on_one_is_refused() {
        let mut regs = Vec::new();
        for name in ["test", "b_eq", "b_ne"] {
            let with = sample(form(name).unwrap(), &mut regs);
            let inst = written(name).unwrap()[0];
            let values: Vec<Value> =
                inst.args.iter().map(|&arg| fill(arg, &with).unwrap()).collect();
            assert!(encode(inst.mnemonic, &values).is_err(), "{name}");
        }
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
        let with = Operands { regs: &[10, 11, 12, 13], reads: 1, imm: 42, mem: None };
        assert_eq!(listing("cmp_set_eq_32", &with), ["subw a0, a1, a2", "seqz a0, a0"]);
        assert_eq!(listing("cmp_set_le_64", &with), ["slt a0, a2, a1", "xori a0, a0, 1"]);
        assert_eq!(listing("fcmp_set_ult_f64", &with), ["fle.d a0, fa2, fa1", "xori a0, a0, 1"]);
        assert_eq!(listing("sel_64", &with), ["mv a0, a2", "bnez a3, .+8", "mv a0, a1"]);
        let shift = Operands { imm: 3, ..with };
        assert_eq!(listing("sra_ri_8", &shift), ["slli a0, a1, 56", "srai a0, a0, 59"]);
        assert_eq!(listing("addr_64", &with), ["lla a0, s"]);
        assert_eq!(listing("got_64", &with), ["la a0, s"]);
        assert_eq!(listing("gottprel_64", &with), ["la.tls.ie a0, s"]);
        assert_eq!(listing("thread_64", &with), ["mv a0, tp"]);
        assert_eq!(listing("call", &with), ["call s"]);
        assert_eq!(listing("call_reg", &with), ["jalr a1"]);
        assert_eq!(listing("fcvtzs_f64_32", &with), ["fcvt.w.d a0, fa1, rtz"]);
        assert_eq!(listing("push_64", &with), ["addi sp, sp, -16", "sd a0, 0(sp)"]);
        assert_eq!(
            listing("pop_pair_64", &with),
            ["ld a0, 0(sp)", "ld a1, 8(sp)", "addi sp, sp, 16"]
        );
        assert_eq!(listing("fence", &with), ["fence rw, rw"]);
        assert_eq!(listing("li_64", &with), ["li a0, 42"]);
        let far = Operands { regs: &[2, 2], reads: 1, imm: 8192, mem: None };
        assert_eq!(listing("sub_far_64", &far), ["li t5, 8192\n\tsub sp, sp, t5"]);
        let near = Operands { imm: 64, ..far };
        assert_eq!(listing("add_far_64", &near), ["addi sp, sp, 64"]);
        assert_eq!(listing("sub_far_64", &near), ["addi sp, sp, -64"]);
        let slot = Operands {
            regs: &[10],
            reads: 1,
            imm: 0,
            mem: Some(Addr { base: 8, offset: Offset::Imm(-24) }),
        };
        assert_eq!(listing("ld_64", &slot), ["ld a0, -24(s0)"]);
        assert_eq!(listing("lea_64", &slot), ["addi a0, s0, -24"]);
        let low =
            Operands { mem: Some(Addr { base: 10, offset: Offset::Symbol(Operator::Lo) }), ..slot };
        assert_eq!(listing("lds_32", &low), ["lw a0, %lo(s)(a0)"]);
        let touched = Operands {
            regs: &[],
            reads: 0,
            imm: 0,
            mem: Some(Addr { base: 2, offset: Offset::Imm(0) }),
        };
        assert_eq!(listing("probe_64", &touched), ["sd zero, 0(sp)"]);
        assert!(listing("arg_val_64", &with).is_empty());
    }

    #[test]
    fn the_width_of_an_operand_is_the_width_of_the_value_in_it() {
        assert_eq!(operand_width("sextb_64", 1), Some(8));
        assert_eq!(operand_width("sextb_64", 0), Some(64));
        assert_eq!(operand_width("sextw_64", 1), Some(32));
        assert_eq!(operand_width("add_rr_32", 2), Some(32));
        assert_eq!(operand_width("cmp_set_lt_32", 1), Some(64));
        assert_eq!(operand_width("and_rr_32", 2), Some(64));
        assert_eq!(operand_width("fadd_f64", 0), None);
        assert_eq!(operand_width("nothing", 0), None);
    }
}
