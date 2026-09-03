//! What each x86-64 machine instruction does with its operands.
//!
//! Design: `spec/10-backend.md` sections 10.1 and 10.2.
//!
//! The lowering rules say which machine instruction computes an IR term and `rucc-verify`
//! proves that it does. Neither says where the operands may live, and that is the other half of
//! what the backend needs: a two-address instruction destroys its first source, a shift by a
//! variable count wants the count in `cl`, and a division has its dividend and its quotient in
//! registers the program did not choose. The allocator has to be told all of it, and the rule
//! set is the wrong place to write it, because it is a fact about the instruction rather than
//! about the rewrite, and the same instruction is reached by many rules.
//!
//! So each opcode the rule set can produce has a [`Form`] here, and a form is the operand
//! vector of every instruction with it. The name is the one the rule set writes without the
//! `x64.` in front, because a machine opcode in the machine IR is a name and this is where the
//! name is given a meaning that is not the encoder's.
//!
//! # What a form is not
//!
//! It is not a promise that the opcode is one instruction. `imul_rr_8` is the form of a
//! two-address multiply and there is no two-operand `imul` on eight bit registers, so the
//! encoder writes more than one instruction for it, and the same is true of every division and
//! of the compare and set pairs. What a form promises is what the allocator has to know, which
//! is what each operand is read or written as and where it is allowed to be, and that is the
//! same whether the opcode becomes one instruction or four.
//!
//! Nothing here mentions flags. A comparison and the set that reads it are one opcode, and a
//! shift reads the flags of nothing, so no instruction in this description has a flag operand
//! and the allocator never sees one. That is a deliberate constraint on the rule set rather
//! than a simplification of the machine.

use crate::operand::{Constraint, OperandDesc};
use crate::x86_64::{GPR, RAX, RCX, RDX};

use Form::{
    AluRi, AluRr, ArgVal, BrCond, Call, CmpSet, Convert, DivQuo, DivRem, Jcc, Jmp, Lea, Load,
    LoadImm, RetVal, ShiftCl, ShiftRi, Store, Test, UnaryR,
};

/// The operand vector one machine instruction has.
///
/// A form rather than a list per opcode, because a hundred and fifty six opcodes have eleven
/// answers between them and writing the eleven once is what makes a mistake in one of them a
/// mistake a test can find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// A destination and an immediate, which is `mov r, imm`.
    LoadImm,
    /// Two-address arithmetic on two registers: the destination is the first source, which the
    /// allocator is the one that has to arrange.
    AluRr,
    /// Two-address arithmetic on a register and an immediate.
    AluRi,
    /// Two-address arithmetic on one register, which is negation and complement.
    UnaryR,
    /// A two-address shift by a constant.
    ShiftRi,
    /// A two-address shift by a count, which this machine reads from `cl` and nowhere else.
    ShiftCl,
    /// A comparison and the byte it sets, which writes a destination unrelated to either
    /// source rather than destroying one of them.
    CmpSet,
    /// A move between widths, which reads one register and writes another.
    Convert,
    /// The quotient of a division, which comes back in `rax` and destroys `rdx` on the way.
    DivQuo,
    /// The remainder of a division, which comes back in `rdx` and destroys `rax` on the way.
    DivRem,
    /// An address computation, whose registers are in an addressing mode rather than in the
    /// operand vector, and which the builder puts there.
    Lea,
    /// A load: a destination register, and an addressing mode the value comes from.
    Load,
    /// A store: an addressing mode the value goes to, and the register it comes out of. It
    /// writes no register at all, which makes it the first form here with no definition in it.
    Store,
    /// The value a function gives back, in the register it is given back in.
    ///
    /// It is not the `ret` instruction and it encodes to nothing. What the selector can do about
    /// a return is put the value where the caller will look for it, and what it cannot do is
    /// leave, because the epilogue has to give the frame back first and the epilogue is written
    /// long after selection has finished. So this is the whole of the return that a lowering rule
    /// gets to decide, and `rucc_codegen::finish` appends the rest to the same block.
    ///
    /// The point of it surviving as an instruction rather than being nothing at all is the
    /// operand: a read constrained to the return register is how the allocator is told to get
    /// the value there, and it is what keeps the value alive that far.
    RetVal,
    /// A value the caller already passed, in the register it arrived in.
    ///
    /// The mirror of [`Form::RetVal`] and the same kind of thing: it encodes to nothing, and what
    /// it is for is telling the allocator where a value already is. A function's arguments are
    /// there before its first instruction runs, so something has to define them, and a block
    /// parameter cannot, because there is no edge into the entry block for a move to go on.
    ///
    /// Which register is not written here, unlike the return, because the answer depends on the
    /// argument's position and on every argument before it. `rucc_codegen::abi` works that out
    /// from the convention and puts it on the operand.
    ArgVal,
    /// The condition a block leaves on, in a register.
    ///
    /// The third form here that encodes to nothing, and the smallest. Where the two arms go is on
    /// the block rather than on the instruction, so this says nothing about either of them: it
    /// reads the condition, which keeps the value alive to the end of the block and gets it into
    /// a register. What turns it into a test and a jump is the block layout, which is the only
    /// thing that knows which of the two arms falls through and therefore which way round the
    /// jump goes. What takes the test back out again, where the condition came from a comparison
    /// that already set the flags, is the peephole pass `spec/10-backend.md` section 10.9
    /// describes, and it is a rule like any other rule.
    ///
    /// An unconditional jump is not a form at all, because there is nothing left of one once the
    /// edge is on the block.
    BrCond,
    /// A comparison of a register against itself, which is what asks whether it is zero.
    ///
    /// The first instruction here that sets the flags and says nothing about them, which is the
    /// same arrangement every instruction here has: the flags are not an operand and the
    /// allocator never sees one. What makes that sound is that this and the jump that reads it
    /// are put in by the block layout, next to each other, after allocation has finished, so
    /// there is nothing left that could put an instruction between them.
    Test,
    /// A jump taken when the flags say so, whose target is on the block.
    ///
    /// Where it goes is the block's first successor, for the reason every other arm is on the
    /// block: an instruction is twenty four bytes and a block reference would not fit in one, and
    /// the successors of a block are the thing every pass over the CFG already reads. The second
    /// successor is where the block goes when the jump is not taken, and after the layout has run
    /// that is always the block laid out next, which is why nothing is written for it.
    Jcc,
    /// A jump always taken, whose target is on the block.
    ///
    /// The one this becomes when the block it goes to is not the next block in the layout. A
    /// block that falls into the next one has no jump at all, which is what laying blocks out in
    /// a good order is worth.
    Jmp,
    /// A call, whose operand vector is not a fact about the instruction.
    ///
    /// Empty for a different reason than the jumps are. A jump has no operands because there is
    /// nothing for it to read, and this has none because there is nothing true of
    /// every call: how many values it passes, which registers they are in, whether anything comes
    /// back and where, are all facts about the signature and the convention. So the operands of a
    /// call are built where it is built, by `rucc_codegen::abi`, the same way an argument's
    /// register is.
    ///
    /// What is the same about every call is the rest of it, and none of that is an operand
    /// either. The registers the convention does not preserve are gone across it, which is said
    /// with a definition per register that nothing reads, and that is what stops the allocator
    /// from leaving a value in one. The bytes below the stack pointer the arguments that did not
    /// fit in registers occupy are the frame's, which is why the selector reports how many a
    /// function's widest call needs rather than writing anything about them here.
    Call,
}

// The destination of a two-address instruction is the operand after it, which is the first
// source. Writing it as a reuse rather than as a copy is what lets the allocator put the two in
// one register when the source dies here and insert the copy when it does not.
static TWO_ADDRESS_RR: [OperandDesc; 3] = [
    OperandDesc::write(GPR).with(Constraint::Reuse(1)),
    OperandDesc::read(GPR),
    OperandDesc::read(GPR),
];
static TWO_ADDRESS_RI: [OperandDesc; 2] =
    [OperandDesc::write(GPR).with(Constraint::Reuse(1)), OperandDesc::read(GPR)];
// The count is in `cl` because that is the only register this machine shifts by. It is the
// whole of `rcx` as far as the allocator is concerned, since `cl` is part of `rcx` and nothing
// else may be using the rest of it.
static SHIFT_CL: [OperandDesc; 3] = [
    OperandDesc::write(GPR).with(Constraint::Reuse(1)),
    OperandDesc::read(GPR),
    OperandDesc::read(GPR).with(Constraint::Fixed(RCX)),
];
static LOAD_IMM: [OperandDesc; 1] = [OperandDesc::write(GPR)];
static ONE_TO_ONE: [OperandDesc; 2] = [OperandDesc::write(GPR), OperandDesc::read(GPR)];
static TWO_TO_ONE: [OperandDesc; 3] =
    [OperandDesc::write(GPR), OperandDesc::read(GPR), OperandDesc::read(GPR)];
// The dividend is in `rax` and the divisor is anywhere else. A division produces both answers
// and this opcode is one of them, so the register the other one lands in is written here as
// well, and it is written early: the sign extension that fills it runs before the division
// reads its divisor, so the divisor may not be sitting in it, and an early definition is how a
// target says exactly that.
static DIV_QUO: [OperandDesc; 4] = [
    OperandDesc::write(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::write_early(GPR).with(Constraint::Fixed(RDX)),
    OperandDesc::read(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::read(GPR),
];
static DIV_REM: [OperandDesc; 4] = [
    OperandDesc::write(GPR).with(Constraint::Fixed(RDX)),
    OperandDesc::write_early(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::read(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::read(GPR),
];
static ADDRESS: [OperandDesc; 1] = [OperandDesc::write(GPR)];
// A load writes one register and reads none, because the registers it reads are the ones in
// the addressing mode and the builder is what puts those in the vector.
static LOAD: [OperandDesc; 1] = [OperandDesc::write(GPR)];
// A store writes nothing. It is the first instruction here that produces no value, which is
// what having an effect means, and the allocator needs no more than that: an instruction with
// no definition keeps nothing alive past it.
static STORE: [OperandDesc; 1] = [OperandDesc::read(GPR)];
// An integer comes back in `rax` on every convention this machine has, which is why the register
// is written here rather than read out of the convention the session was given. A test checks it
// against `SYSV` and `WIN64` rather than leaving it as something a reader has to take on trust,
// and a convention that ever disagrees is one that will fail that test rather than compile.
static RET_VAL: [OperandDesc; 1] = [OperandDesc::read(GPR).with(Constraint::Fixed(RAX))];
// An argument is unconstrained here and constrained where it is built, because which register the
// third argument is in is a fact about the convention and about the two arguments before it, and
// none of that is available to a table of shapes. The class is the same reason: an argument in a
// vector register is one of these too, with the class the convention names for it.
static ARG_VAL: [OperandDesc; 1] = [OperandDesc::write(GPR)];
// A condition is in any register at all, since the instruction this becomes is a `test` of a
// register against itself and every general purpose register can be tested.
static BR_COND: [OperandDesc; 1] = [OperandDesc::read(GPR)];
// A call names no operand here at all, because none of them is a fact about the instruction. What
// it passes and what comes back are facts about the signature it is made against.
static CALL: [OperandDesc; 0] = [];
// A test of a register against itself reads the same register twice. It is written once here,
// because the two operands of the instruction are the same register and the allocator would
// otherwise be free to put two different ones there.
static TEST: [OperandDesc; 1] = [OperandDesc::read(GPR)];
// A jump reads nothing and writes nothing. Where it goes is on the block, not in an operand.
static JUMP: [OperandDesc; 0] = [];

impl Form {
    /// The operands of an instruction of this form, the ones it writes before the ones it
    /// reads.
    ///
    /// The registers an addressing mode names are not here. They are operands and the allocator
    /// rewrites them like any other, and `rucc_mir::InstBuilder::mem` is what puts them in the
    /// vector, because the addressing mode holds their positions and a caller that had to keep
    /// those positions right by hand would eventually not.
    #[must_use]
    pub fn operands(self) -> &'static [OperandDesc] {
        match self {
            LoadImm => &LOAD_IMM,
            AluRr => &TWO_ADDRESS_RR,
            AluRi | UnaryR | ShiftRi => &TWO_ADDRESS_RI,
            ShiftCl => &SHIFT_CL,
            CmpSet => &TWO_TO_ONE,
            Convert => &ONE_TO_ONE,
            DivQuo => &DIV_QUO,
            DivRem => &DIV_REM,
            Lea => &ADDRESS,
            Load => &LOAD,
            Store => &STORE,
            RetVal => &RET_VAL,
            ArgVal => &ARG_VAL,
            BrCond => &BR_COND,
            Call => &CALL,
            Test => &TEST,
            Jcc | Jmp => &JUMP,
        }
    }

    /// Whether an instruction of this form carries an immediate.
    #[must_use]
    pub fn takes_imm(self) -> bool {
        matches!(self, LoadImm | AluRi | ShiftRi)
    }

    /// Whether an instruction of this form carries an addressing mode.
    #[must_use]
    pub fn takes_mem(self) -> bool {
        matches!(self, Lea | Load | Store)
    }
}

/// Every opcode the x86-64 rule set can produce, and the form of each.
///
/// Grouped by family and by width rather than sorted, because this is a list a person checks
/// against a manual and the manual is organized the same way. A lookup is a scan, which is what
/// a selector does once per instruction it emits.
pub static INSTS: &[(&str, Form)] = &[
    // Constants.
    ("mov_ri_8", LoadImm),
    ("mov_ri_16", LoadImm),
    ("mov_ri_32", LoadImm),
    ("mov_ri_64", LoadImm),
    // Arithmetic, register with register.
    ("add_rr_8", AluRr),
    ("add_rr_16", AluRr),
    ("add_rr_32", AluRr),
    ("add_rr_64", AluRr),
    ("sub_rr_8", AluRr),
    ("sub_rr_16", AluRr),
    ("sub_rr_32", AluRr),
    ("sub_rr_64", AluRr),
    ("and_rr_8", AluRr),
    ("and_rr_16", AluRr),
    ("and_rr_32", AluRr),
    ("and_rr_64", AluRr),
    ("or_rr_8", AluRr),
    ("or_rr_16", AluRr),
    ("or_rr_32", AluRr),
    ("or_rr_64", AluRr),
    ("xor_rr_8", AluRr),
    ("xor_rr_16", AluRr),
    ("xor_rr_32", AluRr),
    ("xor_rr_64", AluRr),
    ("imul_rr_8", AluRr),
    ("imul_rr_16", AluRr),
    ("imul_rr_32", AluRr),
    ("imul_rr_64", AluRr),
    // Arithmetic, register with immediate.
    ("add_ri_8", AluRi),
    ("add_ri_16", AluRi),
    ("add_ri_32", AluRi),
    ("add_ri_64", AluRi),
    ("sub_ri_8", AluRi),
    ("sub_ri_16", AluRi),
    ("sub_ri_32", AluRi),
    ("sub_ri_64", AluRi),
    ("and_ri_8", AluRi),
    ("and_ri_16", AluRi),
    ("and_ri_32", AluRi),
    ("and_ri_64", AluRi),
    ("or_ri_8", AluRi),
    ("or_ri_16", AluRi),
    ("or_ri_32", AluRi),
    ("or_ri_64", AluRi),
    ("xor_ri_8", AluRi),
    ("xor_ri_16", AluRi),
    ("xor_ri_32", AluRi),
    ("xor_ri_64", AluRi),
    ("imul_ri_8", AluRi),
    ("imul_ri_16", AluRi),
    ("imul_ri_32", AluRi),
    ("imul_ri_64", AluRi),
    // Negation and complement.
    ("neg_r_8", UnaryR),
    ("neg_r_16", UnaryR),
    ("neg_r_32", UnaryR),
    ("neg_r_64", UnaryR),
    ("not_r_8", UnaryR),
    ("not_r_16", UnaryR),
    ("not_r_32", UnaryR),
    ("not_r_64", UnaryR),
    // Division and remainder, signed and unsigned.
    ("idiv_quo_8", DivQuo),
    ("idiv_quo_16", DivQuo),
    ("idiv_quo_32", DivQuo),
    ("idiv_quo_64", DivQuo),
    ("idiv_rem_8", DivRem),
    ("idiv_rem_16", DivRem),
    ("idiv_rem_32", DivRem),
    ("idiv_rem_64", DivRem),
    ("div_quo_8", DivQuo),
    ("div_quo_16", DivQuo),
    ("div_quo_32", DivQuo),
    ("div_quo_64", DivQuo),
    ("div_rem_8", DivRem),
    ("div_rem_16", DivRem),
    ("div_rem_32", DivRem),
    ("div_rem_64", DivRem),
    // Shifts by a constant.
    ("shl_ri_8", ShiftRi),
    ("shl_ri_16", ShiftRi),
    ("shl_ri_32", ShiftRi),
    ("shl_ri_64", ShiftRi),
    ("shr_ri_8", ShiftRi),
    ("shr_ri_16", ShiftRi),
    ("shr_ri_32", ShiftRi),
    ("shr_ri_64", ShiftRi),
    ("sar_ri_8", ShiftRi),
    ("sar_ri_16", ShiftRi),
    ("sar_ri_32", ShiftRi),
    ("sar_ri_64", ShiftRi),
    // Shifts by a register, which is `cl` and nothing else.
    ("shl_rcl_8", ShiftCl),
    ("shl_rcl_16", ShiftCl),
    ("shl_rcl_32", ShiftCl),
    ("shl_rcl_64", ShiftCl),
    ("shr_rcl_8", ShiftCl),
    ("shr_rcl_16", ShiftCl),
    ("shr_rcl_32", ShiftCl),
    ("shr_rcl_64", ShiftCl),
    ("sar_rcl_8", ShiftCl),
    ("sar_rcl_16", ShiftCl),
    ("sar_rcl_32", ShiftCl),
    ("sar_rcl_64", ShiftCl),
    // The comparisons, ten conditions at four widths.
    ("cmp_set_e_8", CmpSet),
    ("cmp_set_e_16", CmpSet),
    ("cmp_set_e_32", CmpSet),
    ("cmp_set_e_64", CmpSet),
    ("cmp_set_ne_8", CmpSet),
    ("cmp_set_ne_16", CmpSet),
    ("cmp_set_ne_32", CmpSet),
    ("cmp_set_ne_64", CmpSet),
    ("cmp_set_l_8", CmpSet),
    ("cmp_set_l_16", CmpSet),
    ("cmp_set_l_32", CmpSet),
    ("cmp_set_l_64", CmpSet),
    ("cmp_set_le_8", CmpSet),
    ("cmp_set_le_16", CmpSet),
    ("cmp_set_le_32", CmpSet),
    ("cmp_set_le_64", CmpSet),
    ("cmp_set_g_8", CmpSet),
    ("cmp_set_g_16", CmpSet),
    ("cmp_set_g_32", CmpSet),
    ("cmp_set_g_64", CmpSet),
    ("cmp_set_ge_8", CmpSet),
    ("cmp_set_ge_16", CmpSet),
    ("cmp_set_ge_32", CmpSet),
    ("cmp_set_ge_64", CmpSet),
    ("cmp_set_b_8", CmpSet),
    ("cmp_set_b_16", CmpSet),
    ("cmp_set_b_32", CmpSet),
    ("cmp_set_b_64", CmpSet),
    ("cmp_set_be_8", CmpSet),
    ("cmp_set_be_16", CmpSet),
    ("cmp_set_be_32", CmpSet),
    ("cmp_set_be_64", CmpSet),
    ("cmp_set_a_8", CmpSet),
    ("cmp_set_a_16", CmpSet),
    ("cmp_set_a_32", CmpSet),
    ("cmp_set_a_64", CmpSet),
    ("cmp_set_ae_8", CmpSet),
    ("cmp_set_ae_16", CmpSet),
    ("cmp_set_ae_32", CmpSet),
    ("cmp_set_ae_64", CmpSet),
    // The conversions between widths.
    ("movzx_8_16", Convert),
    ("movzx_8_32", Convert),
    ("movzx_8_64", Convert),
    ("movzx_16_32", Convert),
    ("movzx_16_64", Convert),
    ("mov_32_to_64", Convert),
    ("movsx_8_16", Convert),
    ("movsx_8_32", Convert),
    ("movsx_8_64", Convert),
    ("movsx_16_32", Convert),
    ("movsx_16_64", Convert),
    ("movsxd_32_64", Convert),
    ("low_8", Convert),
    ("low_16", Convert),
    ("low_32", Convert),
    // The address computation the addressing modes are reached through.
    ("lea_64", Lea),
    // Reading and writing memory, at each width the machine has a `mov` for.
    ("mov_rm_8", Load),
    ("mov_rm_16", Load),
    ("mov_rm_32", Load),
    ("mov_rm_64", Load),
    ("mov_mr_8", Store),
    ("mov_mr_16", Store),
    ("mov_mr_32", Store),
    ("mov_mr_64", Store),
    // Putting the value a function gives back where the caller looks for it, which is as much of
    // a return as a lowering rule decides.
    ("ret_val_8", RetVal),
    ("ret_val_16", RetVal),
    ("ret_val_32", RetVal),
    ("ret_val_64", RetVal),
    // Naming the register an argument arrived in, which is the other half of the same job and is
    // the one thing here no lowering rule reaches: where an argument is depends on its position
    // and a rule pattern cannot see one.
    ("arg_val_8", ArgVal),
    ("arg_val_16", ArgVal),
    ("arg_val_32", ArgVal),
    ("arg_val_64", ArgVal),
    // The condition a block leaves on, which is as much of a conditional branch as a lowering
    // rule decides, since which arm falls through is the block layout's answer.
    ("br_cond_8", BrCond),
    // A call, which names nothing here because nothing about its operands is the same from one
    // call to the next.
    ("call", Call),
    // What a condition and the block layout come to. The test asks whether the byte a comparison
    // wrote is zero, and the jump that follows it goes to the block's first successor when the
    // answer is the one it names. `jcc_e` is the one written when the block falls through to the
    // arm the condition is true for, and `jcc_ne` the one written when it falls through to the
    // other, which is why both are here and neither is more natural than the other.
    ("test_rr_8", Test),
    ("jcc_e", Jcc),
    ("jcc_ne", Jcc),
    ("jmp", Jmp),
];

/// The form of the opcode of that name, or `None` for a name this target does not have.
///
/// The name is written the way the machine IR holds it, so `add_rr_32` rather than
/// `x64.add_rr_32`. The prefix is how a rule file says which target a term belongs to and it is
/// not part of the opcode.
#[must_use]
pub fn form(name: &str) -> Option<Form> {
    INSTS.iter().find(|(known, _)| *known == name).map(|&(_, form)| form)
}

/// What an address constructor's arguments are.
///
/// An addressing mode is an argument to an instruction rather than an instruction, and a rule
/// file writes one as a term so that a rule can say which registers go where. The selector has
/// to turn that term into a machine IR memory operand, and what each constructor's arguments
/// mean is the same kind of target fact as [`Form`], so it is written here rather than in the
/// selector.
///
/// The scale and the displacement are arguments rather than part of the name because each is a
/// number the rule matched and the machine encodes it as a number. There is none with a symbol
/// yet, because the rules that would need one are the ones about a global and those are not
/// written.
///
/// What the arguments mean is the whole of what tells these apart, and there is deliberately no
/// predicate here that answers half the question: the same register is a base in one of these
/// and an index in another, and the same constant is a scale in one and a displacement in
/// another, so anything building an address out of one has to look at which it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    /// A base register, an index register and a scale, in that order.
    BaseIndexScale,
    /// An index register and a scale, which is an address with nothing to add it to.
    IndexScale,
    /// A base register on its own, which is what a pointer already in a register is.
    Base,
    /// A base register and a constant added to it, which is every field of a structure and
    /// every local reached through a frame pointer.
    BaseOffset,
}

/// Every address constructor the x86-64 rule set can write, and what its arguments are.
pub static ADDRESSES: &[(&str, Address)] = &[
    ("amode_base_index_scale", Address::BaseIndexScale),
    ("amode_index_scale", Address::IndexScale),
    ("amode_base", Address::Base),
    ("amode_base_offset", Address::BaseOffset),
];

/// The address constructor of that name, or `None` for a name that is not one.
///
/// This is what tells an instruction head from an address head, so a selector asks it before it
/// decides that a term it does not recognize is an error.
#[must_use]
pub fn address(name: &str) -> Option<Address> {
    ADDRESSES.iter().find(|(known, _)| *known == name).map(|&(_, kind)| kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operand::Role;
    use crate::x86_64::{FRAME, SYSV, WIN64};

    #[test]
    fn every_opcode_is_described_once() {
        let mut names: Vec<&str> = INSTS.iter().map(|&(name, _)| name).collect();
        let described = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), described, "an opcode is described twice");
        // Every head in the model file, which is what the rule set may write and what
        // `rucc-verify` has an answer for. The two lists are checked against each other by
        // `rucc-codegen`, which is the crate that can read the rule set.
        assert_eq!(described, 178);
    }

    #[test]
    fn a_shape_writes_before_it_reads() {
        for &(name, form) in INSTS {
            let operands = form.operands();
            let defs = operands.iter().filter(|operand| operand.role.is_def()).count();
            assert!(
                operands[..defs].iter().all(|operand| operand.role.is_def()),
                "{name} writes an operand after one it reads"
            );
            // An instruction that writes no register at all is one whose whole purpose is what it
            // does rather than what it computes. A store writes memory, a return puts a value
            // where the caller will look, a branch puts a condition where the jump that the
            // layout writes can read it, a test sets the flags and a jump goes somewhere.
            // Everything else here computes something, and an opcode that computes nothing and
            // does nothing either would be an opcode no rule has any reason to select.
            assert!(
                defs > 0 || matches!(form, Store | RetVal | BrCond | Call | Test | Jcc | Jmp),
                "{name} writes nothing and does nothing"
            );
        }
    }

    #[test]
    fn a_two_address_form_ties_its_destination_to_its_first_source() {
        for form in [AluRr, AluRi, UnaryR, ShiftRi, ShiftCl] {
            assert_eq!(form.operands()[0].constraint, Constraint::Reuse(1));
        }
        // A comparison writes a byte that has nothing to do with either operand, and a
        // conversion reads one width and writes another, so neither destroys its source.
        for form in [CmpSet, Convert, LoadImm, Lea] {
            assert_eq!(form.operands()[0].constraint, Constraint::Reg);
        }
    }

    #[test]
    fn a_division_names_the_registers_the_machine_insists_on() {
        let quo = DivQuo.operands();
        assert_eq!(quo[0].constraint, Constraint::Fixed(RAX));
        assert_eq!(quo[1].constraint, Constraint::Fixed(RDX));
        assert_eq!(quo[1].role, Role::EarlyDef, "the divisor may not be where the rest goes");
        assert_eq!(quo[2].constraint, Constraint::Fixed(RAX));
        assert_eq!(quo[3].constraint, Constraint::Reg);
        let rem = DivRem.operands();
        assert_eq!(rem[0].constraint, Constraint::Fixed(RDX));
        assert_eq!(rem[1].constraint, Constraint::Fixed(RAX));
    }

    #[test]
    fn a_return_leaves_the_value_where_both_conventions_look_for_it() {
        // The register in the form is written down rather than read out of a convention, so this
        // is where the two are checked against each other. Both conventions this target has agree
        // about it, and one that did not would fail here rather than compile a function whose
        // caller reads a register nothing was put in.
        assert_eq!(RetVal.operands()[0].constraint, Constraint::Fixed(RAX));
        assert_eq!(SYSV.int_returns.first(), Some(&RAX));
        assert_eq!(WIN64.int_returns.first(), Some(&RAX));
        // It writes nothing, because the value is the caller's and this function has finished
        // with it.
        assert_eq!(RetVal.operands().len(), 1);
        assert!(!RetVal.takes_imm() && !RetVal.takes_mem());
    }

    #[test]
    fn an_argument_names_no_register_because_its_position_is_what_says_which_one() {
        // The opposite of the return above, and deliberately so. Writing `rdi` here would be
        // writing down where the first SysV integer argument is and then being wrong about every
        // other argument and about Windows, so the register is put on the operand by the code
        // that knows the position.
        assert_eq!(ArgVal.operands()[0].constraint, Constraint::Reg);
        assert_eq!(ArgVal.operands()[0].role, Role::Def);
        assert_eq!(ArgVal.operands().len(), 1);
        assert!(!ArgVal.takes_imm() && !ArgVal.takes_mem());
    }

    #[test]
    fn a_shift_by_a_register_wants_it_in_cl() {
        assert_eq!(ShiftCl.operands()[2].constraint, Constraint::Fixed(RCX));
        assert!(!ShiftCl.takes_imm());
        assert!(ShiftRi.takes_imm());
    }

    #[test]
    fn only_the_shapes_that_carry_one_carry_an_immediate_or_an_address() {
        assert!(LoadImm.takes_imm() && AluRi.takes_imm() && ShiftRi.takes_imm());
        assert!(!AluRr.takes_imm() && !CmpSet.takes_imm() && !DivQuo.takes_imm());
        assert!(Lea.takes_mem());
        assert!(!AluRr.takes_mem() && !LoadImm.takes_mem());
    }

    #[test]
    fn an_address_constructor_is_not_an_instruction() {
        assert_eq!(address("amode_base_index_scale"), Some(Address::BaseIndexScale));
        assert_eq!(address("amode_base_offset"), Some(Address::BaseOffset));
        assert_eq!(address("amode_base"), Some(Address::Base));
        assert_eq!(address("lea_64"), None);
        assert_eq!(form("amode_index_scale"), None);
    }

    /// The block layout reads the four names out of [`crate::x86_64::BRANCH`] and writes them
    /// into the machine IR without ever asking what any of them is, so a name there that is not
    /// an opcode here would come out as an instruction nothing further along could describe. The
    /// forms are pinned too, because the layout writes one shape each and a name that turned out
    /// to be an ordinary two-address instruction would be written with no operands at all.
    #[test]
    fn every_instruction_the_block_layout_writes_is_described_here() {
        use crate::x86_64::BRANCH;

        assert_eq!(BRANCH.prefix, FRAME.prefix, "one target, one prefix");
        assert_eq!(form(BRANCH.cond), Some(BrCond));
        assert_eq!(form(BRANCH.test), Some(Test));
        assert_eq!(form(BRANCH.if_true), Some(Jcc));
        assert_eq!(form(BRANCH.if_false), Some(Jcc));
        assert_eq!(form(BRANCH.jump), Some(Jmp));
        assert_ne!(BRANCH.if_true, BRANCH.if_false, "the two arms are not the same jump");
    }

    #[test]
    fn an_opcode_is_found_by_the_name_the_machine_ir_holds() {
        assert_eq!(form("add_rr_32"), Some(AluRr));
        assert_eq!(form("shl_rcl_64"), Some(ShiftCl));
        assert_eq!(form("lea_64"), Some(Lea));
        assert_eq!(form("x64.add_rr_32"), None, "the prefix is not part of the opcode");
        assert_eq!(form("add_rr_128"), None);
    }
}
