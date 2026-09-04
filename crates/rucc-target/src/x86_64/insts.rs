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
//! So each opcode has a [`Form`] here, and a form is the operand vector of every instruction with
//! it. The name is the one the rule set writes without the `x64.` in front, because a machine
//! opcode in the machine IR is a name and this is where the name is given a meaning that is not
//! the encoder's.
//!
//! Every opcode, and not only the ones a rule selects. A prologue pushes and a spill stores, and
//! neither is anything a pattern could match, so [`crate::FrameInsts`] names them and the block
//! layout's jumps are named by [`crate::BranchInsts`]. All of them end up in the same function and
//! everything downstream reads them the same way, so a second table for the ones a rule cannot
//! reach would be a second place for an opcode to be missing from.
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
use crate::x86_64::{GPR, RAX, RCX, RDX, XMM, xmm};

use Form::{
    AluRi, AluRr, AluVec, ArgVal, ArgValVec, BrCond, Call, CmpSet, CmpSetVec, CmpSetVecBoth,
    Convert, ConvertFromVec, ConvertToVec, ConvertVec, DivQuo, DivRem, Jcc, Jmp, Lea, Load,
    LoadImm, LoadVec, Move, MoveVec, Pop, Push, Ret, RetVal, RetValVec, ShiftCl, ShiftRi, Store,
    StoreVec, Test, UnaryR,
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
    ///
    /// A call through an address is the same form. The address is an operand and is a fact about
    /// the instruction rather than about the signature, so it is the one operand of a call that
    /// could have been written here, and it is not: an index into the operand vector is what a
    /// row of this table names an operand by, and how many registers a call writes before it
    /// reads anything is a different number for every call. What names it instead is
    /// [`Arg::Through`](crate::x86_64::Arg::Through), which is the first operand read rather than
    /// the operand at a place.
    Call,
    /// A copy from one general purpose register to another.
    ///
    /// The first form here no rule reaches. A copy is what the allocator writes when the two ends
    /// of a value could not be given the same register, and what a prologue writes when it puts
    /// the stack pointer in the frame pointer, and neither of those is a term a pattern could
    /// match. It is a whole register at a time whatever the value in it is worth, because a copy
    /// of half a register is a copy that has to know what the other half was for.
    Move,
    /// A register put on the stack, which is how a prologue saves one the convention preserves.
    Push,
    /// A register taken off it, which is how the epilogue gives it back.
    Pop,
    /// Leaving, which is the instruction a lowering rule cannot select for the reason
    /// [`Form::RetVal`] gives: the frame has to be given back first and the frame is worked out
    /// long after selection has finished.
    Ret,
    /// A copy from one vector register to another.
    ///
    /// The same thing as [`Form::Move`] and a separate form rather than the same one, because a
    /// form is the class each of its operands is drawn from and these two are drawn from
    /// different classes. That is also why there are three of these rather than one: a spill and
    /// a reload of a vector register are a different instruction from a spill and a reload of a
    /// general purpose one, and the allocator picks between them by asking the register file
    /// which class the value is in.
    MoveVec,
    /// A vector register read back from the stack.
    LoadVec,
    /// A vector register written to it.
    StoreVec,
    /// Two-address arithmetic on two vector registers, which is every scalar floating point
    /// operation this machine has.
    ///
    /// [`Form::AluRr`] in the other class and a separate form for the same reason the three moves
    /// above are separate: a form is which class each of its operands comes from, and an allocator
    /// handed the wrong one would put a float in a register that cannot hold one. The destination
    /// reuses the first source here too, because `addsd` writes its answer over one of the two it
    /// was given, exactly as `addq` does.
    AluVec,
    /// The value a function gives back, when it goes back in a vector register.
    ///
    /// [`Form::RetVal`] in the other class. It encodes to nothing for the same reason and exists
    /// for the same reason: a read constrained to the register the convention returns in is how
    /// the allocator is told where the value has to end up.
    RetValVec,
    /// A value the caller already passed, when it arrived in a vector register.
    ///
    /// [`Form::ArgVal`] in the other class, unconstrained here and constrained where it is built,
    /// for the reason that one gives.
    ArgValVec,
    /// A conversion from one float format to the other, which reads a vector register and writes
    /// one.
    ///
    /// [`Form::Convert`] in the other class, and the reason there are three of these is the reason
    /// there are two of that: a form is which file each of its operands is drawn from, and a
    /// conversion is the one kind of instruction here whose answer is not the same for both of
    /// them. What the destination is not is a reuse of the source, which every other vector
    /// instruction here is: `cvtss2sd` writes a register it did not read.
    ConvertVec,
    /// A conversion that reads a general purpose register and writes a vector one, which is an
    /// integer becoming a float.
    ConvertToVec,
    /// A conversion that reads a vector register and writes a general purpose one, which is a
    /// float becoming an integer.
    ConvertFromVec,
    /// A comparison of two floats and the byte it sets, which reads two vector registers and
    /// writes a general purpose one.
    ///
    /// [`Form::CmpSet`] with the two sources in the other file. The destination is in this one
    /// because a truth value is a byte and a byte is not a thing the vector registers hold: what
    /// `ucomisd` writes is the flags, and reading the flags is `setcc` and nothing else.
    CmpSetVec,
    /// The same, when the condition takes two of those bytes and a boolean operation to spell.
    ///
    /// Two of the sixteen float comparisons are not one condition on this machine. `ucomisd` says
    /// less, greater, equal or unordered in three flag bits, and every predicate but two is one of
    /// those bits: equal on its own is the flag that means equal or unordered, so an ordered
    /// equality is that flag and the one that says the operands were ordered, put together with an
    /// `and`. Its negation is the other one, with an `or`.
    ///
    /// So the instruction writes a second byte it then reads back, and that byte is written here
    /// as a second definition, the way `idiv` writes down the register it destroys on the way. It
    /// is a register the allocator picks and nothing else can be in it, because a definition that
    /// is live where the first one is live is a definition that cannot share with it.
    CmpSetVecBoth,
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
// A push reads a whole register and a pop writes one. Neither says anything about the stack
// pointer, which every one of them moves: it is not an operand because nothing may be allocated
// to it, and a frame that has one of these in it is a frame that has already accounted for the
// eight bytes it costs.
static PUSH: [OperandDesc; 1] = [OperandDesc::read(GPR)];
static POP: [OperandDesc; 1] = [OperandDesc::write(GPR)];
// Leaving reads the return address and writes the instruction pointer, and neither of those is a
// register anything here can name, so it has no operands at all. What keeps the returned value
// alive as far as this is the `ret_val` in front of it.
static LEAVE: [OperandDesc; 0] = [];
static VEC_TO_VEC: [OperandDesc; 2] = [OperandDesc::write(XMM), OperandDesc::read(XMM)];
static LOAD_VEC: [OperandDesc; 1] = [OperandDesc::write(XMM)];
static STORE_VEC: [OperandDesc; 1] = [OperandDesc::read(XMM)];
// The same shape as `TWO_ADDRESS_RR` in the other class, and separate for the same reason the
// three moves above are separate from the ones over them.
static TWO_ADDRESS_VEC: [OperandDesc; 3] = [
    OperandDesc::write(XMM).with(Constraint::Reuse(1)),
    OperandDesc::read(XMM),
    OperandDesc::read(XMM),
];
// A float comes back in `xmm0` on both of this machine's conventions, so the register is written
// here for the reason `RET_VAL` gives, and the same test holds it against both of them.
static RET_VAL_VEC: [OperandDesc; 1] = [OperandDesc::read(XMM).with(Constraint::Fixed(xmm(0)))];
static ARG_VAL_VEC: [OperandDesc; 1] = [OperandDesc::write(XMM)];
// The two shapes that cross the files, which are the first operand lists here whose two entries
// are not drawn from the same one. Nothing else about them is new: a conversion writes a register
// it did not read, the same way `movzbq` does.
static GPR_TO_VEC: [OperandDesc; 2] = [OperandDesc::write(XMM), OperandDesc::read(GPR)];
static VEC_TO_GPR: [OperandDesc; 2] = [OperandDesc::write(GPR), OperandDesc::read(XMM)];
// `TWO_TO_ONE` with the two sources in the other file, which is what comparing two floats and
// setting a byte on the answer is.
static VEC_TO_ONE: [OperandDesc; 3] =
    [OperandDesc::write(GPR), OperandDesc::read(XMM), OperandDesc::read(XMM)];
// The same with the spare byte the two conditions that take two `setcc` need. It is a definition
// rather than a fixed register so that the allocator places it, and it is a definition at all so
// that the allocator knows the instruction lands a value there: two definitions of one instruction
// are live at the same point, so the register this gets is never the register the answer gets.
static VEC_TO_ONE_BOTH: [OperandDesc; 4] = [
    OperandDesc::write(GPR),
    OperandDesc::write(GPR),
    OperandDesc::read(XMM),
    OperandDesc::read(XMM),
];

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
            Move => &ONE_TO_ONE,
            Push => &PUSH,
            Pop => &POP,
            Ret => &LEAVE,
            MoveVec => &VEC_TO_VEC,
            LoadVec => &LOAD_VEC,
            StoreVec => &STORE_VEC,
            AluVec => &TWO_ADDRESS_VEC,
            RetValVec => &RET_VAL_VEC,
            ArgValVec => &ARG_VAL_VEC,
            ConvertVec => &VEC_TO_VEC,
            ConvertToVec => &GPR_TO_VEC,
            ConvertFromVec => &VEC_TO_GPR,
            CmpSetVec => &VEC_TO_ONE,
            CmpSetVecBoth => &VEC_TO_ONE_BOTH,
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
        matches!(self, Lea | Load | Store | LoadVec | StoreVec)
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
    // Widening a truth value, which the machine does with the byte widenings above because it
    // has no narrower register than a byte. Separate names, because what these mean is what the
    // instruction does to the one bit rather than to the byte holding it.
    ("bit_to_8", Convert),
    ("bit_to_16", Convert),
    ("bit_to_32", Convert),
    ("bit_to_64", Convert),
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
    // The same job for a float, which is a separate opcode rather than a wider one because the
    // register it names is in the other file. A `float` and a `double` are both `xmm0` and are
    // still two opcodes, so that the type a function returns survives as far as the machine IR
    // and a listing says which of the two the program meant.
    ("ret_val_f32", RetValVec),
    ("ret_val_f64", RetValVec),
    // Naming the register an argument arrived in, which is the other half of the same job and is
    // the one thing here no lowering rule reaches: where an argument is depends on its position
    // and a rule pattern cannot see one.
    ("arg_val_8", ArgVal),
    ("arg_val_16", ArgVal),
    ("arg_val_32", ArgVal),
    ("arg_val_64", ArgVal),
    ("arg_val_f32", ArgValVec),
    ("arg_val_f64", ArgValVec),
    // The condition a block leaves on, which is as much of a conditional branch as a lowering
    // rule decides, since which arm falls through is the block layout's answer.
    ("br_cond_8", BrCond),
    // A call, which names nothing here because nothing about its operands is the same from one
    // call to the next. Through an address it is the same instruction to the machine and a
    // different one to the assembler, which writes the register with a star in front of it, and
    // that is the whole of why there are two names here rather than one.
    ("call", Call),
    ("call_reg", Call),
    // What a condition and the block layout come to. The test asks whether the byte a comparison
    // wrote is zero, and the jump that follows it goes to the block's first successor when the
    // answer is the one it names. `jcc_e` is the one written when the block falls through to the
    // arm the condition is true for, and `jcc_ne` the one written when it falls through to the
    // other, which is why both are here and neither is more natural than the other.
    ("test_rr_8", Test),
    ("jcc_e", Jcc),
    ("jcc_ne", Jcc),
    ("jmp", Jmp),
    // What a copy, a prologue, an epilogue, a spill and a reload are made of, which is the other
    // set of instructions no rule reaches. The arithmetic and the address computation a frame
    // needs are already above, because a prologue taking its frame is the same instruction as a
    // subtraction the program wrote and the encoder should not have two answers for it.
    ("mov_rr_64", Move),
    ("push_64", Push),
    ("pop_64", Pop),
    ("ret", Ret),
    ("movaps_rr", MoveVec),
    ("movaps_rm", LoadVec),
    ("movaps_mr", StoreVec),
    // Reading one value out of memory and writing one back, which is the same two shapes as the
    // spill and the reload above and a different instruction: those move a whole register because
    // a spill slot holds whatever was in it, and these move exactly the width of the value because
    // that is all the program asked for.
    ("movss_rm", LoadVec),
    ("movsd_rm", LoadVec),
    ("movss_mr", StoreVec),
    ("movsd_mr", StoreVec),
    ("addss_rr", AluVec),
    ("addsd_rr", AluVec),
    ("subss_rr", AluVec),
    ("subsd_rr", AluVec),
    ("mulss_rr", AluVec),
    ("mulsd_rr", AluVec),
    ("divss_rr", AluVec),
    ("divsd_rr", AluVec),
    // The conversions, which are the instructions that cross between the two register files and
    // the two float formats. Ten of them, which is one for each pair of things a C program is
    // allowed to convert between here: the two formats in both directions, and each format with a
    // thirty two and a sixty four bit integer in both directions.
    ("cvtss2sd", ConvertVec),
    ("cvtsd2ss", ConvertVec),
    ("cvttss2si_32", ConvertFromVec),
    ("cvttss2si_64", ConvertFromVec),
    ("cvttsd2si_32", ConvertFromVec),
    ("cvttsd2si_64", ConvertFromVec),
    ("cvtsi2ss_32", ConvertToVec),
    ("cvtsi2ss_64", ConvertToVec),
    ("cvtsi2sd_32", ConvertToVec),
    ("cvtsi2sd_64", ConvertToVec),
    // The same bits in the other file, which is not a conversion at all: it is where the value is
    // kept and nothing about what it is worth. That is what a `bitcast` between an integer and a
    // float of the same width is, and it is the same instruction each way with the two arguments
    // swapped.
    ("movd_to_xmm", ConvertToVec),
    ("movq_to_xmm", ConvertToVec),
    ("movd_from_xmm", ConvertFromVec),
    ("movq_from_xmm", ConvertFromVec),
    // Comparing two floats, which is one instruction that writes flags and one that reads them,
    // the same pair the integer comparisons above are. Ten per format rather than one per
    // predicate, because the machine has four answers and a C program has sixteen questions: the
    // eight here are the eight the flags answer directly, and the two after them are the two
    // that take both a flag and the bit that says whether the comparison meant anything.
    //
    // The predicates that are not here are the ones that are one of these with the operands the
    // other way round, which is a fact about the rule rather than about the instruction.
    ("ucomiss_set_a", CmpSetVec),
    ("ucomiss_set_ae", CmpSetVec),
    ("ucomiss_set_b", CmpSetVec),
    ("ucomiss_set_be", CmpSetVec),
    ("ucomiss_set_e", CmpSetVec),
    ("ucomiss_set_ne", CmpSetVec),
    ("ucomiss_set_p", CmpSetVec),
    ("ucomiss_set_np", CmpSetVec),
    ("ucomiss_set_e_and_np", CmpSetVecBoth),
    ("ucomiss_set_ne_or_p", CmpSetVecBoth),
    ("ucomisd_set_a", CmpSetVec),
    ("ucomisd_set_ae", CmpSetVec),
    ("ucomisd_set_b", CmpSetVec),
    ("ucomisd_set_be", CmpSetVec),
    ("ucomisd_set_e", CmpSetVec),
    ("ucomisd_set_ne", CmpSetVec),
    ("ucomisd_set_p", CmpSetVec),
    ("ucomisd_set_np", CmpSetVec),
    ("ucomisd_set_e_and_np", CmpSetVecBoth),
    ("ucomisd_set_ne_or_p", CmpSetVecBoth),
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
        assert_eq!(described, 240);
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
            // layout writes can read it, a test sets the flags, a jump goes somewhere, a push
            // puts a register on the stack and leaving leaves. Everything else here computes
            // something, and an opcode that computes nothing and does nothing either would be an
            // opcode nothing has any reason to select.
            assert!(
                defs > 0
                    || matches!(
                        form,
                        Store
                            | RetVal
                            | RetValVec
                            | BrCond
                            | Call
                            | Test
                            | Jcc
                            | Jmp
                            | Push
                            | Ret
                            | StoreVec
                    ),
                "{name} writes nothing and does nothing"
            );
        }
    }

    #[test]
    fn a_two_address_form_ties_its_destination_to_its_first_source() {
        for form in [AluRr, AluRi, UnaryR, ShiftRi, ShiftCl, AluVec] {
            assert_eq!(form.operands()[0].constraint, Constraint::Reuse(1));
        }
        // The float arithmetic is in the other class throughout, which is the whole reason it is a
        // separate form from the integer arithmetic it is otherwise shaped exactly like.
        assert!(AluVec.operands().iter().all(|operand| operand.class == XMM));
        assert!(AluRr.operands().iter().all(|operand| operand.class == GPR));
        // A comparison writes a byte that has nothing to do with either operand, and a
        // conversion reads one width and writes another, so neither destroys its source.
        for form in [CmpSet, CmpSetVec, CmpSetVecBoth, Convert, LoadImm, Lea] {
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

        // The same claim about a float, which comes back in the first vector register on both.
        assert_eq!(RetValVec.operands()[0].constraint, Constraint::Fixed(xmm(0)));
        assert_eq!(SYSV.sse_returns.first(), Some(&xmm(0)));
        assert_eq!(WIN64.sse_returns.first(), Some(&xmm(0)));
        assert_eq!(RetValVec.operands()[0].class, XMM);
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

        assert_eq!(ArgValVec.operands()[0].constraint, Constraint::Reg);
        assert_eq!(ArgValVec.operands()[0].role, Role::Def);
        assert_eq!(ArgValVec.operands()[0].class, XMM);
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

    /// The same claim about the other set of instructions nothing selects.
    ///
    /// `rucc_codegen::finish` reads these names out of [`crate::x86_64::FRAME`] and writes them
    /// into the machine IR, and until this table covered them there was nothing that could say
    /// what a push does with its operand. Six of the twelve names are shared with the rules, since
    /// a prologue taking its frame is a subtraction and a spill is a store, and the test says so
    /// by asking about the form rather than about which list the name came from.
    #[test]
    fn every_instruction_a_frame_is_made_of_is_described_here() {
        assert_eq!(form(FRAME.push), Some(Push));
        assert_eq!(form(FRAME.pop), Some(Pop));
        assert_eq!(form(FRAME.ret), Some(Ret));
        assert_eq!(form(FRAME.add), Some(AluRi));
        assert_eq!(form(FRAME.sub), Some(AluRi));
        assert_eq!(form(FRAME.align), Some(AluRi));
        assert_eq!(form(FRAME.lea), Some(Lea));

        // One set of moves per class the allocator may spill, and the class each of them is
        // written for is the class the form draws its operands from.
        let gpr = FRAME.classes[GPR.number() as usize];
        assert_eq!(form(gpr.mov), Some(Move));
        assert_eq!(form(gpr.load), Some(Load));
        assert_eq!(form(gpr.store), Some(Store));
        let xmm = FRAME.classes[XMM.number() as usize];
        assert_eq!(form(xmm.mov), Some(MoveVec));
        assert_eq!(form(xmm.load), Some(LoadVec));
        assert_eq!(form(xmm.store), Some(StoreVec));
        assert_eq!(MoveVec.operands()[0].class, XMM);
        assert_eq!(Move.operands()[0].class, GPR);
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
