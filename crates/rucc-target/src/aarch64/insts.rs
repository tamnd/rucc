//! What each AArch64 machine instruction does with its operands.
//!
//! Design: `spec/10-backend.md` sections 10.1 and 10.2.
//!
//! The same kind of table as `rucc_target::x86_64::INSTS`, for the same reader: the allocator,
//! which has to know what each operand of an opcode is read or written as and where it is allowed
//! to be, and the machine level passes, which ask the same table whether a shape they propose is
//! one this machine has. The name is the one the machine IR holds without the `a64.` in front.
//!
//! The machine makes this table a good deal shorter than the x86 one. Every instruction here
//! writes a register the program chose and reads registers the program chose, so there is no
//! two-address form and no operand pinned to a register because the instruction insists on it.
//! The only fixed registers are the ones the calling convention names, which are the values
//! arriving and leaving in [`Form::ArgVal`] and [`Form::RetVal`] and their neighbours.
//!
//! # Widths
//!
//! Two, 32 and 64 bits, because those are the two widths the integer instructions have. A value
//! narrower than 32 bits lives in a 32 bit register, and what makes its upper bits right is the
//! lowering, which widens with [`Form::Convert`] where the answer depends on them. The loads and
//! stores are the exception and have 8 and 16 bit forms, since how much of memory is touched is the
//! whole difference between them.
//!
//! # What a form is not
//!
//! It is not a promise that the opcode is one instruction. A comparison that keeps its answer is a
//! `cmp` and a `cset`, and the address of a symbol is an `adrp` and an `add`. What each opcode is
//! written as is `crate::aarch64::text`, which is where the encoder and the listing both read it.
//!
//! Nothing here mentions flags, for the reason the x86 table gives: a comparison and what reads it
//! are one opcode until the layout folds a branch into the comparison in front of it, and the
//! allocator never sees the condition state.

use crate::aarch64::{FPR, GPR, v, x};
use crate::machine::Address as Amode;
use crate::operand::{Constraint, OperandDesc};

use Form::{
    Address, Alu, AluI, ArgVal, ArgValFp, Barrier, BrCond, Call, Cmp, CmpI, CmpSet, CmpSetI,
    Convert, Csel, FAlu, FCmp, FCmpSet, FConvert, FMove, FUnary, FpToInt, Insert, IntToFp, Jcc,
    Jump, JumpAway, JumpReg, Lea, Load, LoadFp, LoadImm, Move, MulAdd, Nop, Pop, PopPair, Probe,
    Push, PushPair, Ret, RetVal, RetVal2, RetVal2Fp, RetValFp, Select, Set, Store, StoreFp, Test,
    Trap, Unary,
};

/// The operand vector one machine instruction has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// A destination and a constant the one instruction can build, which is `mov`.
    LoadImm,
    /// Sixteen more bits of a constant put into a register that already holds the rest, which is
    /// `movk`. The register is read and written, so the destination is the source.
    Insert,
    /// A destination and two sources.
    Alu,
    /// A destination, a source and a constant the instruction can hold.
    AluI,
    /// A destination and three sources, the product of the first two added to or taken from the
    /// third.
    MulAdd,
    /// A destination and one source.
    Unary,
    /// A destination and one source that it widens, which copies the low bits of the source.
    Convert,
    /// A destination and a copy of one source.
    Move,
    /// Two sources compared, keeping nothing but the condition state.
    Cmp,
    /// A source compared with a constant, keeping nothing but the condition state.
    CmpI,
    /// A source compared with zero, which is what a branch on a byte is written in front of.
    Test,
    /// A destination that is one when the comparison of the two sources held and zero otherwise.
    CmpSet,
    /// The same with a constant for the second source.
    CmpSetI,
    /// A destination, two sources and a byte, which picks the second source when the byte is not
    /// zero. The byte is last, so that taking it off leaves [`Form::Csel`].
    Select,
    /// A destination and two sources, picked between on a condition some comparison left.
    Csel,
    /// A destination that is one when a condition some comparison left holds, which is what is
    /// left of a [`Form::CmpSet`] once the comparison in front of it has been found to be made.
    Set,
    /// A destination and an addressing mode whose address it computes and does not read.
    Lea,
    /// A destination and a symbol whose address it computes.
    Address,
    /// A destination and an addressing mode it reads.
    Load,
    /// A source and an addressing mode it writes to.
    Store,
    /// [`Form::Load`] into the other register file.
    LoadFp,
    /// [`Form::Store`] out of the other register file.
    StoreFp,
    /// A destination and two sources in the other register file.
    FAlu,
    /// A destination and one source in the other register file.
    FUnary,
    /// A copy within the other register file.
    FMove,
    /// One floating point width to the other.
    FConvert,
    /// A general register into the other file, as a number or as bits.
    IntToFp,
    /// The other file into a general register, as a number or as bits.
    FpToInt,
    /// Two floating point sources compared, keeping nothing but the condition state.
    FCmp,
    /// A destination that is one when the comparison of two floating point sources held.
    FCmpSet,
    /// The value a function returns, in the register the convention returns it in.
    RetVal,
    /// The second half of a value returned in two registers.
    RetVal2,
    /// [`Form::RetVal`] in the other register file.
    RetValFp,
    /// [`Form::RetVal2`] in the other register file.
    RetVal2Fp,
    /// A value arriving in a register, which the lowering pins to the register it arrives in.
    ArgVal,
    /// [`Form::ArgVal`] in the other register file.
    ArgValFp,
    /// The branch a rule selects, reading the byte it branches on. The layout replaces it.
    BrCond,
    /// A branch to a block.
    Jump,
    /// A branch to a block when a condition holds.
    Jcc,
    /// A branch to a symbol, which is how a tail call leaves.
    JumpAway,
    /// A branch to the address in a register.
    JumpReg,
    /// A call, to a symbol or through the first register it reads.
    Call,
    /// The return.
    Ret,
    /// A register stored below the stack pointer, moving the pointer down.
    Push,
    /// A register loaded from the stack pointer, moving the pointer up.
    Pop,
    /// Two registers stored below the stack pointer, the first at the lower address, moving the
    /// pointer down past both.
    PushPair,
    /// Two registers loaded from the stack pointer, the first from the lower address, moving the
    /// pointer up past both.
    PopPair,
    /// A page of the stack written without anything being put there, which is an address and a
    /// constant the machine has no use for and the prologue hands over anyway.
    Probe,
    /// Nothing.
    Nop,
    /// A stop the program cannot step past.
    Trap,
    /// A fence between the memory accesses before it and the ones after it.
    Barrier,
}

static ONE_WRITTEN: [OperandDesc; 1] = [OperandDesc::write(GPR)];
static TWO_WRITTEN: [OperandDesc; 2] = [OperandDesc::write(GPR), OperandDesc::write(GPR)];
static INSERT: [OperandDesc; 2] =
    [OperandDesc::write(GPR).with(Constraint::Reuse(1)), OperandDesc::read(GPR)];
static ONE_TO_ONE: [OperandDesc; 2] = [OperandDesc::write(GPR), OperandDesc::read(GPR)];
static TWO_TO_ONE: [OperandDesc; 3] =
    [OperandDesc::write(GPR), OperandDesc::read(GPR), OperandDesc::read(GPR)];
static THREE_TO_ONE: [OperandDesc; 4] = [
    OperandDesc::write(GPR),
    OperandDesc::read(GPR),
    OperandDesc::read(GPR),
    OperandDesc::read(GPR),
];
static TWO_READ: [OperandDesc; 2] = [OperandDesc::read(GPR), OperandDesc::read(GPR)];
static ONE_READ: [OperandDesc; 1] = [OperandDesc::read(GPR)];
static ONE_WRITTEN_FP: [OperandDesc; 1] = [OperandDesc::write(FPR)];
static ONE_READ_FP: [OperandDesc; 1] = [OperandDesc::read(FPR)];
static FP_TO_FP: [OperandDesc; 2] = [OperandDesc::write(FPR), OperandDesc::read(FPR)];
static TWO_FP_TO_FP: [OperandDesc; 3] =
    [OperandDesc::write(FPR), OperandDesc::read(FPR), OperandDesc::read(FPR)];
static GPR_TO_FP: [OperandDesc; 2] = [OperandDesc::write(FPR), OperandDesc::read(GPR)];
static FP_TO_GPR: [OperandDesc; 2] = [OperandDesc::write(GPR), OperandDesc::read(FPR)];
static TWO_FP_READ: [OperandDesc; 2] = [OperandDesc::read(FPR), OperandDesc::read(FPR)];
static TWO_FP_TO_ONE: [OperandDesc; 3] =
    [OperandDesc::write(GPR), OperandDesc::read(FPR), OperandDesc::read(FPR)];
static RET_VAL: [OperandDesc; 1] = [OperandDesc::read(GPR).with(Constraint::Fixed(x(0)))];
static RET_VAL_2: [OperandDesc; 1] = [OperandDesc::read(GPR).with(Constraint::Fixed(x(1)))];
static RET_VAL_FP: [OperandDesc; 1] = [OperandDesc::read(FPR).with(Constraint::Fixed(v(0)))];
static RET_VAL_2_FP: [OperandDesc; 1] = [OperandDesc::read(FPR).with(Constraint::Fixed(v(1)))];
static NONE: [OperandDesc; 0] = [];

impl Form {
    /// The operands of an instruction of this form, the ones it writes before the ones it reads.
    ///
    /// The registers an addressing mode names are not here, for the reason they are not in the
    /// x86 table: `rucc_mir::InstBuilder::mem` puts them in the vector and the addressing mode
    /// holds their positions.
    #[must_use]
    pub fn operands(self) -> &'static [OperandDesc] {
        match self {
            LoadImm | Lea | Address | Load | ArgVal | Set => &ONE_WRITTEN,
            Insert => &INSERT,
            AluI | Unary | Convert | Move | CmpSetI => &ONE_TO_ONE,
            Alu | CmpSet | Csel => &TWO_TO_ONE,
            MulAdd | Select => &THREE_TO_ONE,
            Cmp | PushPair => &TWO_READ,
            CmpI | Test | Store | BrCond | JumpReg | Push => &ONE_READ,
            Pop => &ONE_WRITTEN,
            PopPair => &TWO_WRITTEN,
            LoadFp | ArgValFp => &ONE_WRITTEN_FP,
            StoreFp => &ONE_READ_FP,
            FUnary | FMove | FConvert => &FP_TO_FP,
            FAlu => &TWO_FP_TO_FP,
            IntToFp => &GPR_TO_FP,
            FpToInt => &FP_TO_GPR,
            FCmp => &TWO_FP_READ,
            FCmpSet => &TWO_FP_TO_ONE,
            RetVal => &RET_VAL,
            RetVal2 => &RET_VAL_2,
            RetValFp => &RET_VAL_FP,
            RetVal2Fp => &RET_VAL_2_FP,
            Jump | Jcc | JumpAway | Call | Ret | Nop | Trap | Barrier | Probe => &NONE,
        }
    }

    /// Whether an instruction of this form carries an immediate.
    #[must_use]
    pub fn takes_imm(self) -> bool {
        matches!(self, LoadImm | Insert | AluI | CmpI | CmpSetI | Probe)
    }

    /// Whether an instruction of this form carries an addressing mode.
    #[must_use]
    pub fn takes_mem(self) -> bool {
        matches!(self, Lea | Load | Store | LoadFp | StoreFp | Probe)
    }

    /// Whether an instruction of this form reads or writes memory.
    ///
    /// [`Lea`] is not on the list and [`Push`], [`Pop`], [`Call`] and [`Ret`] are, for the reasons
    /// `rucc_target::x86_64::Form::touches_mem` gives. [`Address`] is not either: the address of a
    /// symbol in the global offset table is read out of the table, but the table does not change
    /// while the program runs, so nothing moved past the read can change what it reads.
    #[must_use]
    pub fn touches_mem(self) -> bool {
        matches!(
            self,
            Load | Store
                | LoadFp
                | StoreFp
                | Push
                | Pop
                | PushPair
                | PopPair
                | Probe
                | Call
                | Ret
                | Barrier
        )
    }
}

pub static INSTS: &[(&str, Form)] = &[
    // Constants. A constant that is not one `mov` writes is a `mov` of one sixteen bit piece and a
    // `movk` for each piece after it.
    ("mov_ri_32", LoadImm),
    ("mov_ri_64", LoadImm),
    ("movk_ri_16_32", Insert),
    ("movk_ri_16_64", Insert),
    ("movk_ri_32_64", Insert),
    ("movk_ri_48_64", Insert),
    // Copies.
    ("mov_rr_32", Move),
    ("mov_rr_64", Move),
    // Arithmetic on two registers.
    ("add_rr_32", Alu),
    ("add_rr_64", Alu),
    ("sub_rr_32", Alu),
    ("sub_rr_64", Alu),
    ("and_rr_32", Alu),
    ("and_rr_64", Alu),
    ("orr_rr_32", Alu),
    ("orr_rr_64", Alu),
    ("eor_rr_32", Alu),
    ("eor_rr_64", Alu),
    ("bic_rr_32", Alu),
    ("bic_rr_64", Alu),
    ("orn_rr_32", Alu),
    ("orn_rr_64", Alu),
    ("mul_rr_32", Alu),
    ("mul_rr_64", Alu),
    ("sdiv_rr_32", Alu),
    ("sdiv_rr_64", Alu),
    ("udiv_rr_32", Alu),
    ("udiv_rr_64", Alu),
    ("lsl_rr_32", Alu),
    ("lsl_rr_64", Alu),
    ("lsr_rr_32", Alu),
    ("lsr_rr_64", Alu),
    ("asr_rr_32", Alu),
    ("asr_rr_64", Alu),
    ("ror_rr_32", Alu),
    ("ror_rr_64", Alu),
    ("smulh_rr_64", Alu),
    ("umulh_rr_64", Alu),
    // Arithmetic on a register and a constant. The constant is one the instruction can hold, which
    // for `add` and `sub` is twelve bits and for the three logical operations is a bitmask.
    ("add_ri_32", AluI),
    ("add_ri_64", AluI),
    ("sub_ri_32", AluI),
    ("sub_ri_64", AluI),
    ("and_ri_32", AluI),
    ("and_ri_64", AluI),
    ("orr_ri_32", AluI),
    ("orr_ri_64", AluI),
    ("eor_ri_32", AluI),
    ("eor_ri_64", AluI),
    ("lsl_ri_32", AluI),
    ("lsl_ri_64", AluI),
    ("lsr_ri_32", AluI),
    ("lsr_ri_64", AluI),
    ("asr_ri_32", AluI),
    ("asr_ri_64", AluI),
    ("ror_ri_32", AluI),
    ("ror_ri_64", AluI),
    // Multiply and add, and multiply and subtract, which is how a remainder is taken: the quotient
    // times the divisor, subtracted from the dividend.
    ("madd_rrr_32", MulAdd),
    ("madd_rrr_64", MulAdd),
    ("msub_rrr_32", MulAdd),
    ("msub_rrr_64", MulAdd),
    // One register to one register.
    ("neg_r_32", Unary),
    ("neg_r_64", Unary),
    ("mvn_r_32", Unary),
    ("mvn_r_64", Unary),
    ("clz_r_32", Unary),
    ("clz_r_64", Unary),
    ("rbit_r_32", Unary),
    ("rbit_r_64", Unary),
    ("rev_r_32", Unary),
    ("rev_r_64", Unary),
    ("rev16_r_32", Unary),
    // Widening. Each reads the low bits of its source and agrees with it about every one of them,
    // which is what `copies_low` asks.
    ("sxtb_16", Convert),
    ("sxtb_32", Convert),
    ("sxtb_64", Convert),
    ("sxth_32", Convert),
    ("sxth_64", Convert),
    ("uxtb_16", Convert),
    ("uxtb_32", Convert),
    ("uxtb_64", Convert),
    ("uxth_32", Convert),
    ("uxth_64", Convert),
    ("sxtw_64", Convert),
    ("uxtw_64", Convert),
    // Widening one bit, which keeps that bit and clears everything above it. Not a `Convert`,
    // because the bits above the one it keeps are not copied from anywhere.
    ("bit_to_8", Unary),
    ("bit_to_16", Unary),
    ("bit_to_32", Unary),
    ("bit_to_64", Unary),
    // Narrowing, which keeps the low bits of a register. The one bit case clears the rest.
    ("low_8", Convert),
    ("low_16", Convert),
    ("low_32", Convert),
    ("bit_of_32", AluI),
    ("bit_of_64", AluI),
    // Comparisons that keep nothing but the condition state, which is what a branch on one is
    // folded into.
    ("cmp_rr_32", Cmp),
    ("cmp_ri_32", CmpI),
    ("test_32", Test),
    ("cmp_rr_64", Cmp),
    ("cmp_ri_64", CmpI),
    ("test_64", Test),
    // Comparisons that keep the answer, which is `cset` after the comparison.
    ("cmp_set_eq_32", CmpSet),
    ("cmp_set_eq_64", CmpSet),
    ("cmp_set_ne_32", CmpSet),
    ("cmp_set_ne_64", CmpSet),
    ("cmp_set_lt_32", CmpSet),
    ("cmp_set_lt_64", CmpSet),
    ("cmp_set_le_32", CmpSet),
    ("cmp_set_le_64", CmpSet),
    ("cmp_set_gt_32", CmpSet),
    ("cmp_set_gt_64", CmpSet),
    ("cmp_set_ge_32", CmpSet),
    ("cmp_set_ge_64", CmpSet),
    ("cmp_set_lo_32", CmpSet),
    ("cmp_set_lo_64", CmpSet),
    ("cmp_set_ls_32", CmpSet),
    ("cmp_set_ls_64", CmpSet),
    ("cmp_set_hi_32", CmpSet),
    ("cmp_set_hi_64", CmpSet),
    ("cmp_set_hs_32", CmpSet),
    ("cmp_set_hs_64", CmpSet),
    ("cmp_set_eq_ri_32", CmpSetI),
    ("cmp_set_eq_ri_64", CmpSetI),
    ("cmp_set_ne_ri_32", CmpSetI),
    ("cmp_set_ne_ri_64", CmpSetI),
    ("cmp_set_lt_ri_32", CmpSetI),
    ("cmp_set_lt_ri_64", CmpSetI),
    ("cmp_set_le_ri_32", CmpSetI),
    ("cmp_set_le_ri_64", CmpSetI),
    ("cmp_set_gt_ri_32", CmpSetI),
    ("cmp_set_gt_ri_64", CmpSetI),
    ("cmp_set_ge_ri_32", CmpSetI),
    ("cmp_set_ge_ri_64", CmpSetI),
    ("cmp_set_lo_ri_32", CmpSetI),
    ("cmp_set_lo_ri_64", CmpSetI),
    ("cmp_set_ls_ri_32", CmpSetI),
    ("cmp_set_ls_ri_64", CmpSetI),
    ("cmp_set_hi_ri_32", CmpSetI),
    ("cmp_set_hi_ri_64", CmpSetI),
    ("cmp_set_hs_ri_32", CmpSetI),
    ("cmp_set_hs_ri_64", CmpSetI),
    // A choice on a byte, which is the second source when the byte is not zero and the first when
    // it is, and the same choice straight off a condition some comparison left.
    ("sel_32", Select),
    ("sel_64", Select),
    ("csel_eq_32", Csel),
    ("csel_eq_64", Csel),
    ("csel_ne_32", Csel),
    ("csel_ne_64", Csel),
    ("csel_lt_32", Csel),
    ("csel_lt_64", Csel),
    ("csel_le_32", Csel),
    ("csel_le_64", Csel),
    ("csel_gt_32", Csel),
    ("csel_gt_64", Csel),
    ("csel_ge_32", Csel),
    ("csel_ge_64", Csel),
    ("csel_lo_32", Csel),
    ("csel_lo_64", Csel),
    ("csel_ls_32", Csel),
    ("csel_ls_64", Csel),
    ("csel_hi_32", Csel),
    ("csel_hi_64", Csel),
    ("csel_hs_32", Csel),
    ("csel_hs_64", Csel),
    // A condition kept on its own, which is what a comparison that keeps its answer becomes when
    // the comparison has already been made. Always thirty two bits, since the answer is a one or
    // a zero and writing a `w` register clears the rest.
    ("cset_eq", Set),
    ("cset_ne", Set),
    ("cset_lt", Set),
    ("cset_le", Set),
    ("cset_gt", Set),
    ("cset_ge", Set),
    ("cset_lo", Set),
    ("cset_ls", Set),
    ("cset_hi", Set),
    ("cset_hs", Set),
    ("cset_mi", Set),
    // Addresses. A base and a constant is one `add`, and a symbol is the page it is on and then the
    // offset into that page.
    ("lea_64", Lea),
    ("addr_64", Address),
    // The address of a block or a jump table of this function, which is a fixed distance from the
    // code and carried in the addressing mode, the way x86-64 carries it in a `lea`.
    ("adr_64", Lea),
    ("got_64", Address),
    ("tls_64", Address),
    // Loads, which write a whole register whatever they read.
    ("ldr_8", Load),
    ("ldr_16", Load),
    ("ldr_32", Load),
    ("ldr_64", Load),
    ("ldrs_8_32", Load),
    ("ldrs_8_64", Load),
    ("ldrs_16_32", Load),
    ("ldrs_16_64", Load),
    ("ldrs_32_64", Load),
    // Stores.
    ("str_8", Store),
    ("str_16", Store),
    ("str_32", Store),
    ("str_64", Store),
    // Loads and stores of the other register file.
    ("ldr_f32", LoadFp),
    ("ldr_f64", LoadFp),
    ("ldr_f128", LoadFp),
    ("str_f32", StoreFp),
    ("str_f64", StoreFp),
    ("str_f128", StoreFp),
    // Floating point arithmetic.
    ("fadd_f32", FAlu),
    ("fadd_f64", FAlu),
    ("fsub_f32", FAlu),
    ("fsub_f64", FAlu),
    ("fmul_f32", FAlu),
    ("fmul_f64", FAlu),
    ("fdiv_f32", FAlu),
    ("fdiv_f64", FAlu),
    ("fmin_f32", FAlu),
    ("fmin_f64", FAlu),
    ("fmax_f32", FAlu),
    ("fmax_f64", FAlu),
    ("fminnm_f32", FAlu),
    ("fminnm_f64", FAlu),
    ("fmaxnm_f32", FAlu),
    ("fmaxnm_f64", FAlu),
    ("fneg_f32", FUnary),
    ("fneg_f64", FUnary),
    ("fabs_f32", FUnary),
    ("fabs_f64", FUnary),
    ("fsqrt_f32", FUnary),
    ("fsqrt_f64", FUnary),
    ("frintz_f32", FUnary),
    ("frintz_f64", FUnary),
    ("frintm_f32", FUnary),
    ("frintm_f64", FUnary),
    ("frintp_f32", FUnary),
    ("frintp_f64", FUnary),
    ("frinta_f32", FUnary),
    ("frinta_f64", FUnary),
    ("frintn_f32", FUnary),
    ("frintn_f64", FUnary),
    ("frintx_f32", FUnary),
    ("frintx_f64", FUnary),
    ("frinti_f32", FUnary),
    ("frinti_f64", FUnary),
    // Copies within the other register file. A 128 bit value is copied as sixteen bytes, since
    // there is no scalar move that wide.
    ("fmov_rr_f32", FMove),
    ("fmov_rr_f64", FMove),
    ("mov_rr_f128", FMove),
    // Between the two floating point widths.
    ("fcvt_f64_f32", FConvert),
    ("fcvt_f32_f64", FConvert),
    // From an integer to a float, and back, rounding toward zero the way C does. The name is the
    // source and then the destination.
    ("scvtf_32_f32", IntToFp),
    ("scvtf_32_f64", IntToFp),
    ("scvtf_64_f32", IntToFp),
    ("scvtf_64_f64", IntToFp),
    ("ucvtf_32_f32", IntToFp),
    ("ucvtf_32_f64", IntToFp),
    ("ucvtf_64_f32", IntToFp),
    ("ucvtf_64_f64", IntToFp),
    ("fcvtzs_f32_32", FpToInt),
    ("fcvtzs_f32_64", FpToInt),
    ("fcvtzs_f64_32", FpToInt),
    ("fcvtzs_f64_64", FpToInt),
    ("fcvtzu_f32_32", FpToInt),
    ("fcvtzu_f32_64", FpToInt),
    ("fcvtzu_f64_32", FpToInt),
    ("fcvtzu_f64_64", FpToInt),
    // The bits of a register moved across to the other file unchanged, which is how a float
    // constant is built and how a union is read.
    ("fmov_to_f32", IntToFp),
    ("fmov_to_f64", IntToFp),
    ("fmov_from_f32", FpToInt),
    ("fmov_from_f64", FpToInt),
    // Floating point comparisons. The conditions are chosen so that every ordered one is false when
    // either side is not a number and `ne` is true, which is what C asks of each of them.
    ("fcmp_f32", FCmp),
    ("fcmp_f64", FCmp),
    ("fcmp_set_eq_f32", FCmpSet),
    ("fcmp_set_eq_f64", FCmpSet),
    ("fcmp_set_ne_f32", FCmpSet),
    ("fcmp_set_ne_f64", FCmpSet),
    ("fcmp_set_lt_f32", FCmpSet),
    ("fcmp_set_lt_f64", FCmpSet),
    ("fcmp_set_le_f32", FCmpSet),
    ("fcmp_set_le_f64", FCmpSet),
    ("fcmp_set_gt_f32", FCmpSet),
    ("fcmp_set_gt_f64", FCmpSet),
    ("fcmp_set_ge_f32", FCmpSet),
    ("fcmp_set_ge_f64", FCmpSet),
    // Values arriving and leaving in the registers the convention names. None is an instruction,
    // and each is here so the allocator knows which register the value has to be in.
    ("ret_val_32", RetVal),
    ("ret_val_64", RetVal),
    ("ret_val_f32", RetValFp),
    ("ret_val_f64", RetValFp),
    ("ret_val_f128", RetValFp),
    ("ret_val2_32", RetVal2),
    ("ret_val2_64", RetVal2),
    ("ret_val2_f32", RetVal2Fp),
    ("ret_val2_f64", RetVal2Fp),
    ("ret_val2_f128", RetVal2Fp),
    ("arg_val_32", ArgVal),
    ("arg_val_64", ArgVal),
    ("arg_val_f32", ArgValFp),
    ("arg_val_f64", ArgValFp),
    ("arg_val_f128", ArgValFp),
    ("br_cond_32", BrCond),
    // Branches, calls and the return.
    ("b", Jump),
    ("b_eq", Jcc),
    ("b_ne", Jcc),
    ("b_lt", Jcc),
    ("b_le", Jcc),
    ("b_gt", Jcc),
    ("b_ge", Jcc),
    ("b_lo", Jcc),
    ("b_ls", Jcc),
    ("b_hi", Jcc),
    ("b_hs", Jcc),
    ("b_mi", Jcc),
    ("b_pl", Jcc),
    ("b_vs", Jcc),
    ("b_vc", Jcc),
    ("b_away", JumpAway),
    ("br", JumpReg),
    ("bl", Call),
    ("blr", Call),
    ("ret", Ret),
    // A register saved and restored sixteen bytes at a time, since the stack pointer has to stay a
    // multiple of sixteen whenever it is used to reach memory.
    ("push_64", Push),
    ("pop_64", Pop),
    ("push_pair_64", PushPair),
    ("pop_pair_64", PopPair),
    // The two a prologue writes that no rule selects. A frame aligned harder than sixteen goes
    // through `x16`, since the one instruction that can write the stack pointer with a mask cannot
    // read it, and a page is touched by storing the zero register to it.
    ("align_sp_64", AluI),
    ("probe_64", Probe),
    // Everything else.
    ("nop", Nop),
    ("trap", Trap),
    ("fence", Barrier),
];

/// The form of the opcode of that name, or `None` for a name this target does not have.
///
/// The name is written the way the machine IR holds it, so `add_rr_32` rather than
/// `a64.add_rr_32`.
#[must_use]
pub fn form(name: &str) -> Option<Form> {
    INSTS.iter().find(|(known, _)| *known == name).map(|&(_, form)| form)
}

/// Every address constructor the AArch64 rule set can write, and what its arguments are.
///
/// Two of the four the x86 rules have. A load or a store here takes a base and a small constant
/// or a base and a register, and the second one is not written yet, so an index with a scale is
/// something a rule computes into a register first.
pub static ADDRESSES: &[(&str, Amode)] =
    &[("amode_base", Amode::Base), ("amode_base_offset", Amode::BaseOffset)];

/// The address constructor of that name, or `None` for a name that is not one.
#[must_use]
pub fn address(name: &str) -> Option<Amode> {
    ADDRESSES.iter().find(|(known, _)| *known == name).map(|&(_, kind)| kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_opcode_is_described_twice() {
        for (at, (name, _)) in INSTS.iter().enumerate() {
            assert!(
                INSTS[at + 1..].iter().all(|(other, _)| other != name),
                "{name} is in the table twice"
            );
        }
    }

    #[test]
    fn a_form_that_carries_a_constant_or_an_address_has_a_register_to_go_with_it() {
        // Save the probe, which writes the page its address names and is handed a constant it has
        // no use for, because the prologue writes every target's probe the same way.
        for &(name, form) in INSTS {
            if (form.takes_imm() || form.takes_mem()) && form != Probe {
                assert!(!form.operands().is_empty(), "{name} has nothing to put its answer in");
            }
        }
        assert_eq!(form("lea_64"), Some(Lea));
        assert!(!Lea.touches_mem());
        assert!(Load.touches_mem() && Push.touches_mem());
        assert_eq!(form("x64.add_rr_64"), None);
    }

    #[test]
    fn the_only_fixed_registers_are_the_ones_the_convention_names() {
        for &(name, form) in INSTS {
            let fixed =
                form.operands().iter().any(|op| matches!(op.constraint, Constraint::Fixed(_)));
            assert_eq!(fixed, name.starts_with("ret_val"), "{name}");
        }
    }
}
