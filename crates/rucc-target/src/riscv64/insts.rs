//! What each RISC-V machine instruction does with its operands.
//!
//! Design: `spec/10-backend.md` sections 10.1 and 10.2.
//!
//! The same kind of table as `rucc_target::aarch64::INSTS`, for the same readers: the allocator,
//! which has to know what each operand of an opcode is read or written as, and the machine level
//! passes, which ask whether a shape they propose is one this machine has. The name is the one the
//! machine IR holds without the `rv.` in front.
//!
//! # Widths
//!
//! The registers are sixty four bits and the instructions with a `w` in their name work on the low
//! thirty two and sign extend the answer, which is what a 32 bit opcode here is written as when
//! the answer depends on the upper bits. The rest of the 32 bit opcodes are the 64 bit instruction,
//! since the low thirty two bits of an `and` or an `add` do not depend on the bits above them. A
//! comparison does, so the 32 bit comparisons other than equality are left to the rules, which
//! widen both sides first.
//!
//! # No condition state
//!
//! The machine has no flags. A comparison writes its answer to a register and a branch compares
//! two registers itself, so the test and the two jumps on it that the layout writes for a branch
//! on a byte are here only for the layout to fold into `beqz` and `bnez`, which it always does.
//! They are written as mnemonics the encoder refuses, so one that is left over is an error rather
//! than a missing branch.
//!
//! # Nothing inside an opcode uses a scratch register
//!
//! `t5` and `t6` are where the spill code reloads an operand, so an opcode that used either inside
//! its own text could find one holding its input. Whatever needs a second register for a moment is
//! two opcodes in the rules instead. The one exception is the frame's far constant, which reads
//! nothing but the stack pointer.

use std::sync::LazyLock;

use crate::machine::Address as Amode;
use crate::named;
use crate::operand::{Constraint, OperandDesc};
use crate::riscv64::{FPR, GPR, f, x};

use Form::{
    Address, Alu, AluI, Amo, AmoNegated, ArgVal, ArgValFp, Barrier, BrCond, Call, CmpSet,
    CompareSwap, CompareSwapMasked, Convert, FAlu, FCmpSet, FConvert, FMove, FMulAdd, FUnary,
    FpToInt, IntToFp, Jcc, Jump, JumpAway, JumpReg, JumpZero, Lea, Load, LoadFp, LoadImm, Move,
    Nop, Pop, PopPair, Probe, Push, PushPair, Ret, RetVal, RetVal2, RetVal2Fp, RetValFp, Select,
    Store, StoreFp, Template, Test, Trap, Unary,
};

/// The operand vector one machine instruction has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// A destination and a constant, which is `li` and as many instructions as the constant takes.
    LoadImm,
    /// A destination and two sources.
    Alu,
    /// A destination, a source and a constant the instruction can hold.
    AluI,
    /// A destination and one source.
    Unary,
    /// A destination and one source that it widens, which copies the low bits of the source.
    Convert,
    /// A destination and a copy of one source.
    Move,
    /// A source compared with zero, which the layout writes in front of a branch on a byte and
    /// then folds into the branch.
    Test,
    /// A destination that is one when the comparison of the two sources held and zero otherwise.
    CmpSet,
    /// A destination, two sources and a byte, which picks the second source when the byte is not
    /// zero. The destination is written before the byte and the first source are read, so it is
    /// written early.
    Select,
    /// A destination and an addressing mode whose address it computes and does not read.
    Lea,
    /// A destination and a symbol whose address it computes, or the thread pointer.
    Address,
    /// A destination and an addressing mode it reads.
    Load,
    /// A source and an addressing mode it writes to.
    Store,
    /// [`Form::Load`] into the floating point file.
    LoadFp,
    /// [`Form::Store`] out of the floating point file.
    StoreFp,
    /// A destination and two sources in the floating point file.
    FAlu,
    /// A destination and three sources in the floating point file, the product of the first two
    /// with the third added and one rounding at the end.
    FMulAdd,
    /// A destination and one source in the floating point file.
    FUnary,
    /// A copy within the floating point file.
    FMove,
    /// One floating point width to the other.
    FConvert,
    /// An integer register into the floating point file, as a number or as bits.
    IntToFp,
    /// The floating point file into an integer register, as a number or as bits.
    FpToInt,
    /// A destination that is one when the comparison of two floating point sources held.
    FCmpSet,
    /// The value a function returns, in the register the convention returns it in.
    RetVal,
    /// The second half of a value returned in two registers.
    RetVal2,
    /// [`Form::RetVal`] in the floating point file.
    RetValFp,
    /// [`Form::RetVal2`] in the floating point file.
    RetVal2Fp,
    /// A value arriving in a register, which the lowering pins to the register it arrives in.
    ArgVal,
    /// [`Form::ArgVal`] in the floating point file.
    ArgValFp,
    /// The branch a rule selects, reading the byte it branches on. The layout replaces it.
    BrCond,
    /// A branch to a block.
    Jump,
    /// A branch to a block on the condition a [`Form::Test`] left, which the layout folds away.
    Jcc,
    /// A branch to a block when the register it reads is zero, or when it is not.
    JumpZero,
    /// A branch to a symbol, which is how a tail call leaves.
    JumpAway,
    /// A branch to the address in a register.
    JumpReg,
    /// A call, to a symbol or through the first register it reads.
    Call,
    /// The return.
    Ret,
    /// A register stored below the stack pointer, moving the pointer down by sixteen.
    Push,
    /// A register loaded from the stack pointer, moving the pointer up by sixteen.
    Pop,
    /// Two registers stored below the stack pointer, the first at the lower address.
    PushPair,
    /// Two registers loaded from the stack pointer, the first from the lower address.
    PopPair,
    /// A page of the stack written without anything being put there.
    Probe,
    /// Nothing.
    Nop,
    /// A stop the program cannot step past.
    Trap,
    /// A fence between the memory accesses before it and the ones after it.
    Barrier,
    /// An `amo` instruction. What was there, the register holding the address and the value the
    /// operation is done with.
    Amo,
    /// An `amoadd` of the negated value, which is a subtraction. The same operands as
    /// [`Form::Amo`], and what was there is written early, since the negation is put in it before
    /// the address is read.
    AmoNegated,
    /// A compare and exchange as a loop of `lr` and `sc`. What was there, whether the exchange
    /// happened, the address, the value expected and the value put there.
    ///
    /// The two it writes are written early, because the loop writes both before it reads the
    /// address and the two values for the last time. The yes or no is the status `sc` left until
    /// the loop ends, and the answer after it.
    CompareSwap,
    /// The same for a byte or a half, which `lr` and `sc` do not come in, as the loop on the word
    /// around it with the rest of the word kept. What was there, whether the exchange happened,
    /// four more the loop works in, the address, the value expected and the value put there.
    ///
    /// The four are the word's address, the mask that picks the byte out of it, and the value
    /// expected and the value put there moved to where the byte is in the word. All six are
    /// written early for the reason the two of [`Form::CompareSwap`] are.
    CompareSwapMasked,
    /// An `asm` template kept as the text the program wrote. See
    /// `crate::aarch64::Form::Template`.
    Template,
}

static ONE_WRITTEN: [OperandDesc; 1] = [OperandDesc::write(GPR)];
static TWO_WRITTEN: [OperandDesc; 2] = [OperandDesc::write(GPR), OperandDesc::write(GPR)];
static ONE_TO_ONE: [OperandDesc; 2] = [OperandDesc::write(GPR), OperandDesc::read(GPR)];
static TWO_TO_ONE: [OperandDesc; 3] =
    [OperandDesc::write(GPR), OperandDesc::read(GPR), OperandDesc::read(GPR)];
static SELECT: [OperandDesc; 4] = [
    OperandDesc::write_early(GPR),
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
static THREE_FP_TO_FP: [OperandDesc; 4] = [
    OperandDesc::write(FPR),
    OperandDesc::read(FPR),
    OperandDesc::read(FPR),
    OperandDesc::read(FPR),
];
static GPR_TO_FP: [OperandDesc; 2] = [OperandDesc::write(FPR), OperandDesc::read(GPR)];
static FP_TO_GPR: [OperandDesc; 2] = [OperandDesc::write(GPR), OperandDesc::read(FPR)];
static TWO_FP_TO_ONE: [OperandDesc; 3] =
    [OperandDesc::write(GPR), OperandDesc::read(FPR), OperandDesc::read(FPR)];
static RET_VAL: [OperandDesc; 1] = [OperandDesc::read(GPR).with(Constraint::Fixed(x(10)))];
static RET_VAL_2: [OperandDesc; 1] = [OperandDesc::read(GPR).with(Constraint::Fixed(x(11)))];
static RET_VAL_FP: [OperandDesc; 1] = [OperandDesc::read(FPR).with(Constraint::Fixed(f(10)))];
static RET_VAL_2_FP: [OperandDesc; 1] = [OperandDesc::read(FPR).with(Constraint::Fixed(f(11)))];
static AMO: [OperandDesc; 3] =
    [OperandDesc::write(GPR), OperandDesc::read(GPR), OperandDesc::read(GPR)];
static AMO_NEGATED: [OperandDesc; 3] =
    [OperandDesc::write_early(GPR), OperandDesc::read(GPR), OperandDesc::read(GPR)];
static COMPARE_SWAP: [OperandDesc; 5] = [
    OperandDesc::write_early(GPR),
    OperandDesc::write_early(GPR),
    OperandDesc::read(GPR),
    OperandDesc::read(GPR),
    OperandDesc::read(GPR),
];
static COMPARE_SWAP_MASKED: [OperandDesc; 9] = [
    OperandDesc::write_early(GPR),
    OperandDesc::write_early(GPR),
    OperandDesc::write_early(GPR),
    OperandDesc::write_early(GPR),
    OperandDesc::write_early(GPR),
    OperandDesc::write_early(GPR),
    OperandDesc::read(GPR),
    OperandDesc::read(GPR),
    OperandDesc::read(GPR),
];
static NONE: [OperandDesc; 0] = [];

impl Form {
    /// The operands of an instruction of this form, the ones it writes before the ones it reads.
    ///
    /// The registers an addressing mode names are not here, for the reason they are not in the
    /// AArch64 table.
    #[must_use]
    pub fn operands(self) -> &'static [OperandDesc] {
        match self {
            LoadImm | Lea | Address | Load | ArgVal | Pop => &ONE_WRITTEN,
            AluI | Unary | Convert | Move => &ONE_TO_ONE,
            Alu | CmpSet => &TWO_TO_ONE,
            Select => &SELECT,
            PushPair => &TWO_READ,
            PopPair => &TWO_WRITTEN,
            Test | Store | BrCond | JumpReg | JumpZero | Push => &ONE_READ,
            LoadFp | ArgValFp => &ONE_WRITTEN_FP,
            StoreFp => &ONE_READ_FP,
            FUnary | FMove | FConvert => &FP_TO_FP,
            FAlu => &TWO_FP_TO_FP,
            FMulAdd => &THREE_FP_TO_FP,
            IntToFp => &GPR_TO_FP,
            FpToInt => &FP_TO_GPR,
            FCmpSet => &TWO_FP_TO_ONE,
            RetVal => &RET_VAL,
            RetVal2 => &RET_VAL_2,
            RetValFp => &RET_VAL_FP,
            RetVal2Fp => &RET_VAL_2_FP,
            Amo => &AMO,
            AmoNegated => &AMO_NEGATED,
            CompareSwap => &COMPARE_SWAP,
            CompareSwapMasked => &COMPARE_SWAP_MASKED,
            Jump | Jcc | JumpAway | Call | Ret | Nop | Trap | Barrier | Probe | Template => &NONE,
        }
    }

    /// Whether an instruction of this form carries an immediate.
    #[must_use]
    pub fn takes_imm(self) -> bool {
        matches!(self, LoadImm | AluI | Probe)
    }

    /// Whether an instruction of this form carries an addressing mode.
    #[must_use]
    pub fn takes_mem(self) -> bool {
        matches!(self, Lea | Load | Store | LoadFp | StoreFp | Probe)
    }

    /// Whether an instruction of this form reads or writes memory.
    ///
    /// The same list as AArch64's for the forms the two share, for the reasons
    /// `crate::aarch64::Form::touches_mem` gives.
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
                | Amo
                | AmoNegated
                | CompareSwap
                | CompareSwapMasked
                | Template
        )
    }
}

pub static INSTS: &[(&str, Form)] = &[
    // Constants, which `li` builds in as many instructions as each takes.
    ("li_32", LoadImm),
    ("li_64", LoadImm),
    // Copies.
    ("mv_rr_32", Move),
    ("mv_rr_64", Move),
    // Arithmetic on two registers. The 32 bit forms are the `w` instructions where the low bits of
    // the answer depend on more than the low bits of the sources, and the 64 bit ones where not.
    ("add_rr_32", Alu),
    ("add_rr_64", Alu),
    ("sub_rr_32", Alu),
    ("sub_rr_64", Alu),
    ("and_rr_32", Alu),
    ("and_rr_64", Alu),
    ("or_rr_32", Alu),
    ("or_rr_64", Alu),
    ("xor_rr_32", Alu),
    ("xor_rr_64", Alu),
    ("mul_rr_32", Alu),
    ("mul_rr_64", Alu),
    ("div_rr_32", Alu),
    ("div_rr_64", Alu),
    ("divu_rr_32", Alu),
    ("divu_rr_64", Alu),
    ("rem_rr_32", Alu),
    ("rem_rr_64", Alu),
    ("remu_rr_32", Alu),
    ("remu_rr_64", Alu),
    ("sll_rr_32", Alu),
    ("sll_rr_64", Alu),
    ("srl_rr_32", Alu),
    ("srl_rr_64", Alu),
    ("sra_rr_32", Alu),
    ("sra_rr_64", Alu),
    ("mulh_rr_64", Alu),
    ("mulhu_rr_64", Alu),
    // Arithmetic on a register and a twelve bit constant, and the shifts by a constant below the
    // width.
    ("add_ri_32", AluI),
    ("add_ri_64", AluI),
    ("and_ri_32", AluI),
    ("and_ri_64", AluI),
    ("or_ri_32", AluI),
    ("or_ri_64", AluI),
    ("xor_ri_32", AluI),
    ("xor_ri_64", AluI),
    ("sll_ri_32", AluI),
    ("sll_ri_64", AluI),
    ("srl_ri_32", AluI),
    ("srl_ri_64", AluI),
    ("sra_ri_32", AluI),
    ("sra_ri_64", AluI),
    // A right shift of a byte or a half, which takes the bits above it off first so that what
    // comes down from above is zeros or copies of its sign.
    ("srl_ri_8", AluI),
    ("srl_ri_16", AluI),
    ("sra_ri_8", AluI),
    ("sra_ri_16", AluI),
    // One register to one register.
    ("neg_r_32", Unary),
    ("neg_r_64", Unary),
    ("not_r_32", Unary),
    ("not_r_64", Unary),
    // Widening. Each reads the low bits of its source and agrees with it about every one of them.
    ("sextb_16", Convert),
    ("sextb_32", Convert),
    ("sextb_64", Convert),
    ("sexth_32", Convert),
    ("sexth_64", Convert),
    ("sextw_64", Convert),
    ("zextb_16", Convert),
    ("zextb_32", Convert),
    ("zextb_64", Convert),
    ("zexth_32", Convert),
    ("zexth_64", Convert),
    ("zextw_64", Convert),
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
    // Comparisons that keep their answer. Equality at both widths, and the rest at sixty four,
    // named after the AArch64 conditions so that the two rule files read alike.
    ("cmp_set_eq_32", CmpSet),
    ("cmp_set_eq_64", CmpSet),
    ("cmp_set_ne_32", CmpSet),
    ("cmp_set_ne_64", CmpSet),
    ("cmp_set_lt_64", CmpSet),
    ("cmp_set_le_64", CmpSet),
    ("cmp_set_gt_64", CmpSet),
    ("cmp_set_ge_64", CmpSet),
    ("cmp_set_lo_64", CmpSet),
    ("cmp_set_ls_64", CmpSet),
    ("cmp_set_hi_64", CmpSet),
    ("cmp_set_hs_64", CmpSet),
    ("cmp_set_lt_32", CmpSet),
    ("cmp_set_le_32", CmpSet),
    ("cmp_set_gt_32", CmpSet),
    ("cmp_set_ge_32", CmpSet),
    ("cmp_set_lo_32", CmpSet),
    ("cmp_set_ls_32", CmpSet),
    ("cmp_set_hi_32", CmpSet),
    ("cmp_set_hs_32", CmpSet),
    // A choice between two registers on a byte, which is a copy and a branch around a second one.
    ("sel_32", Select),
    ("sel_64", Select),
    // The test the layout writes in front of a branch on a byte, which it folds into the branch.
    ("test", Test),
    // Addresses: a base and a constant, a symbol near enough to reach from the code, a symbol
    // through the global offset table, a thread-local variable's offset from the thread pointer
    // through the table, and the thread pointer.
    ("lea_64", Lea),
    ("lea_far_64", Lea),
    ("addr_64", Address),
    ("got_64", Address),
    ("gottprel_64", Address),
    ("thread_64", Address),
    // Loads, unsigned and signed.
    ("ld_8", Load),
    ("ld_bit", Load),
    ("ld_16", Load),
    ("ld_32", Load),
    ("ld_64", Load),
    ("lds_8", Load),
    ("lds_16", Load),
    ("lds_32", Load),
    ("ldu_32", Load),
    // Stores.
    ("st_8", Store),
    ("st_bit", Store),
    ("st_16", Store),
    ("st_32", Store),
    ("st_64", Store),
    // The floating point loads and stores.
    ("ld_f32", LoadFp),
    ("ld_f64", LoadFp),
    ("st_f32", StoreFp),
    ("st_f64", StoreFp),
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
    ("fmadd_f32", FMulAdd),
    ("fmadd_f64", FMulAdd),
    ("fsqrt_f32", FUnary),
    ("fsqrt_f64", FUnary),
    ("fneg_f32", FUnary),
    ("fneg_f64", FUnary),
    ("fabs_f32", FUnary),
    ("fabs_f64", FUnary),
    ("fmv_f32", FMove),
    ("fmv_f64", FMove),
    // A float register cleared in front of a `ret` under `-fzero-call-used-regs=`, which writes
    // the register and reads nothing, so the second operand is the same register and not spelled.
    ("fzero_f64", FMove),
    // Between the two floating point widths.
    ("fcvt_f32_f64", FConvert),
    ("fcvt_f64_f32", FConvert),
    // From an integer to a float, and back, rounding toward zero the way C does. The names are the
    // AArch64 ones, the source and then the destination.
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
    // The bits of a register moved across to the other file unchanged.
    ("fmov_to_f32", IntToFp),
    ("fmov_to_f64", IntToFp),
    ("fmov_from_f32", FpToInt),
    ("fmov_from_f64", FpToInt),
    // Floating point comparisons. `feq`, `flt` and `fle` are false when either side is not a
    // number, which is what C asks of every one but `!=`, and that one and the unordered ones are
    // the opposite of an ordered one. `uno`, `ord`, `one` and `ueq` take two comparisons and are
    // left to the rules.
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
    ("fcmp_set_ult_f32", FCmpSet),
    ("fcmp_set_ult_f64", FCmpSet),
    ("fcmp_set_ule_f32", FCmpSet),
    ("fcmp_set_ule_f64", FCmpSet),
    ("fcmp_set_ugt_f32", FCmpSet),
    ("fcmp_set_ugt_f64", FCmpSet),
    ("fcmp_set_uge_f32", FCmpSet),
    ("fcmp_set_uge_f64", FCmpSet),
    // Values arriving and leaving in the registers the convention names. None is an instruction.
    ("ret_val_32", RetVal),
    ("ret_val_64", RetVal),
    ("ret_val_f32", RetValFp),
    ("ret_val_f64", RetValFp),
    ("ret_val2_32", RetVal2),
    ("ret_val2_64", RetVal2),
    ("ret_val2_f32", RetVal2Fp),
    ("ret_val2_f64", RetVal2Fp),
    ("arg_val_32", ArgVal),
    ("arg_val_64", ArgVal),
    ("arg_val_f32", ArgValFp),
    ("arg_val_f64", ArgValFp),
    ("br_cond_32", BrCond),
    // Branches, calls and the return.
    ("j", Jump),
    ("b_eq", Jcc),
    ("b_ne", Jcc),
    ("beqz", JumpZero),
    ("bnez", JumpZero),
    ("tail", JumpAway),
    ("jr", JumpReg),
    ("call", Call),
    ("call_reg", Call),
    ("ret", Ret),
    // A register saved and restored sixteen bytes at a time, which keeps the stack pointer a
    // multiple of sixteen the way the convention asks.
    ("push_64", Push),
    ("pop_64", Pop),
    ("push_pair_64", PushPair),
    ("pop_pair_64", PopPair),
    // What a prologue writes that no rule selects: the stack pointer moved by any amount, a frame
    // aligned harder than sixteen, and a page touched by storing the zero register to it.
    ("add_far_64", AluI),
    ("sub_far_64", AluI),
    ("align_sp_64", AluI),
    ("probe_64", Probe),
    // Everything else.
    ("nop", Nop),
    ("trap", Trap),
    ("fence", Barrier),
    ("fence_acquire", Barrier),
    ("fence_release", Barrier),
    // The atomics. A read modify write at a word or a doubleword is one `amo` instruction, and a
    // compare and exchange is a loop of `lr` and `sc`, each at the strongest ordering, which is
    // what gcc writes there and is never wrong at a weaker one. A byte or a half has neither, so
    // its compare and exchange is the masked loop on the word, and every other read modify write
    // of one is a loop around that.
    ("xchg_32", Amo),
    ("xchg_64", Amo),
    ("xadd_32", Amo),
    ("xadd_64", Amo),
    ("xsub_32", AmoNegated),
    ("xsub_64", AmoNegated),
    ("cmpxchg_8", CompareSwapMasked),
    ("cmpxchg_16", CompareSwapMasked),
    ("cmpxchg_32", CompareSwap),
    ("cmpxchg_64", CompareSwap),
    // An `asm` statement's text, which is spelled into the listing rather than chosen by anything.
    ("template", Template),
];

/// The opcode that is a template kept as text, named so that the lowering and the writer are
/// asking about the same opcode. See [`Form::Template`].
pub const TEMPLATE: &str = "template";

/// The form of the opcode of that name, or `None` for a name this target does not have.
///
/// The name is written the way the machine IR holds it, so `add_rr_32` rather than
/// `rv.add_rr_32`.
#[must_use]
pub fn form(name: &str) -> Option<Form> {
    static AT: LazyLock<named::Names<usize>> = LazyLock::new(|| named::index(INSTS));
    AT.get(name).map(|&at| INSTS[at].1)
}

/// Every address constructor the RISC-V rule set can write, and what its arguments are.
///
/// A load or a store takes a base register and a twelve bit constant and nothing else, so these
/// are the two of the AArch64 ones that mean that.
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
        for &(name, form) in INSTS {
            if (form.takes_imm() || form.takes_mem()) && form != Probe {
                assert!(!form.operands().is_empty(), "{name} has nothing to put its answer in");
            }
        }
        assert_eq!(form("lea_64"), Some(Lea));
        assert!(!Lea.touches_mem());
        assert!(Load.touches_mem() && Push.touches_mem());
        assert_eq!(form("a64.add_rr_64"), None);
        assert_eq!(address("amode_base_offset"), Some(Amode::BaseOffset));
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
