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
use crate::x86_64::{GPR, RAX, RBX, RCX, RDX, XMM, xmm};

use Form::{
    Align, AluCarry, AluCarryI, AluMi, AluMr, AluRi, AluRm, AluRr, AluVec, ArgVal, ArgValVec,
    ArithX87, Barrier, BrCond, Call, Cmov, Cmp, CmpMi, CmpRi, CmpRm, CmpSet, CmpSetMi, CmpSetRi,
    CmpSetRm, CmpSetVec, CmpSetVecBoth, CmpSetX87, CmpSetX87Both, CmpXchg, Convert, ConvertFromVec,
    ConvertToVec, ConvertVec, CpuId, CtrlX87, DivQuo, DivRem, DivWide, Jcc, Jmp, JmpAway, JmpReg,
    Landing, Lea, Literal, Load, LoadImm, LoadVec, Move, MoveVec, MulWide, Nop, Pop, PopX87,
    Prefetch, Push, PushX87, Ret, RetVal, RetVal2, RetVal2Vec, RetValVec, Rmw, Search, Set,
    ShiftCl, ShiftRi, Spin, Store, StoreVec, Swap, Test, TestCmov, Trap, UnaryR, UnaryX87,
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
    /// Two-address arithmetic on two registers that reads the carry as well.
    ///
    /// `adc` and `sbb`, which are [`Form::AluRr`] with one more source that is not in this vector
    /// because it is not a register. A program adds two numbers wider than a register by adding the
    /// low halves and then adding the high halves and the bit that fell off the low ones, and these
    /// are the second of those two instructions.
    ///
    /// The operand vector is [`Form::AluRr`]'s exactly, and that is the point rather than an
    /// economy. The header above says nothing here mentions flags and that this is a constraint on
    /// the rule set, and it still holds: no rule reaches one of these. What a template writes is
    /// two instructions that pass a bit between them, and what has to keep them a pair is the
    /// description of the condition state next door in [`crate::FlagInsts`], which the scheduler
    /// reads and which now says these read it. A form saying so as well would be the same fact in
    /// two places, and two places is where a fact goes wrong.
    ///
    /// So why a form of its own rather than [`Form::AluRr`]. Because the list of what reads the
    /// condition state is taken off these forms rather than typed out, and an instruction that
    /// reads the state and does not carry a condition in its name is a third kind that the two
    /// tests over that list have to be able to tell from the other two. Naming it is what lets them.
    AluCarry,
    /// The same instruction as [`Form::AluCarry`] with a constant where its second source is.
    ///
    /// `adc $0, %rax`, which is what a program writes for the third word of a number three words
    /// wide: there is nothing to add there except the bit that fell off the second word, and a
    /// constant zero is how the instruction that adds only the bit is spelled. libgmp's
    /// `add_sssaaaa` in `longlong.h` is the whole reason this is here.
    ///
    /// The operand vector is [`Form::AluRi`]'s and the condition state is read the way
    /// [`Form::AluCarry`] reads it, so this is the two of them crossed and nothing more. It is a
    /// form of its own for the reason that one is: the list of what reads the state is taken off
    /// these names rather than typed out, so an instruction that reads it has to be findable by
    /// its form.
    AluCarryI,
    /// The same instruction as [`Form::AluRr`] reading its second source out of memory.
    ///
    /// One operand vector shorter than [`Form::AluRr`] and one addressing mode longer, which is
    /// the whole difference: the destination is still the first source, the machine still writes
    /// the answer into a register, and what the second source is has moved from a register number
    /// to a mode. No lowering rule produces one, because a rule matches a term and what this is
    /// is a load and an arithmetic term put together, which is [`crate::MachineInsts`]'s customer
    /// `rucc_codegen::combine`.
    AluRm,
    /// The same instruction as [`Form::AluRr`] working on memory rather than on a register.
    ///
    /// The other side of [`Form::AluRm`]. That one reads a source out of memory and writes its
    /// answer into a register, and this one reads a source out of a register and leaves its answer
    /// where it read the other one from, so it reads memory and writes it back in one instruction.
    /// One register read, no register written, and an addressing mode, which is the operand shape a
    /// store has and is why the two share a description.
    ///
    /// No lowering rule produces one, for the reason no rule produces a [`Form::AluRm`] and one
    /// more besides. A rule replaces a term with a term and this is three terms, a load and an
    /// arithmetic and a store, and the third of them is not a value at all. `rucc_codegen::combine`
    /// is what writes one, out of a run it found.
    ///
    /// The multiply has no member here. `imul` on this machine writes a register and nothing else,
    /// so there is no sixteen, thirty two or sixty four bit form of it that leaves its answer in
    /// memory, and the five operations that do are the ones with rows.
    AluMr,
    /// The same instruction as [`Form::AluRi`] working on memory rather than on a register.
    ///
    /// An addressing mode and an immediate and no register of its own, which no other form here is.
    /// The registers in the vector are the ones the addressing mode brought, and there is nothing
    /// in front of them, because the constant is on the instruction and the place the answer goes
    /// is the place the other source came from. That makes it the shortest description in the
    /// table and an empty one.
    ///
    /// It does not commute and does not need to. [`Form::AluMr`] has a left source and a right
    /// source and which of them the memory is decides whether a subtraction can use it. Here the
    /// memory is always the left source, because the immediate cannot be anywhere but on the
    /// instruction, so `subl $1, (%rax)` is the only arrangement there is and it is the one that
    /// takes the constant away from the place. The multiply is missing for the reason it is
    /// missing from [`Form::AluMr`]: `imul` writes a register whatever its sources are.
    ///
    /// Two things write one. `rucc_codegen::combine` writes one out of a load, an arithmetic
    /// against a constant and a store of the same place, which is the reason the form is here. And
    /// a prologue taking a frame under `-fstack-clash-protection` writes the eight bit inclusive or
    /// with zero, which touches the page an address is on and puts back the byte that was already
    /// there. That one is the same instruction as any other member and is described the same way,
    /// rather than having a form to itself: what a probe wants out of it is the write and not the
    /// value, and the machine does not know the difference.
    AluMi,
    /// Two-address arithmetic on one register, which is negation and complement.
    UnaryR,
    /// A two-address shift by a constant.
    ShiftRi,
    /// A two-address shift by a count, which this machine reads from `cl` and nowhere else.
    ShiftCl,
    /// A comparison and the byte it sets, which writes a destination unrelated to either
    /// source rather than destroying one of them.
    CmpSet,
    /// The same against a constant, which the machine compares against without being handed a
    /// register holding it.
    ///
    /// One register read rather than two, and the constant on the instruction. It is not
    /// two-address the way [`Form::AluRi`] is, for the reason [`Form::CmpSet`] is not either:
    /// what a comparison writes is the flags, and the byte the set behind it writes is a
    /// destination neither source has any claim on.
    CmpSetRi,
    /// The same reading its right hand side out of memory.
    ///
    /// [`Form::CmpSet`] with the second source moved from a register number to an addressing mode,
    /// which is the change [`Form::AluRm`] is to [`Form::AluRr`]. The byte stays where it is and
    /// the left hand side stays in a register, so the description is the one a comparison against
    /// a constant has, with an address on the instruction instead of an immediate.
    ///
    /// No lowering rule produces one, for the reason no rule produces a [`Form::AluRm`]: what this
    /// is is a load and a comparison put together, which is two terms. `rucc_codegen::combine`
    /// writes one, out of a comparison whose source came from a load nothing else read.
    ///
    /// Which side the memory is decides the condition rather than ruling the fold out. A
    /// comparison does not commute and does not have to: reading the two sides the other way round
    /// asks the same question backwards, so a load that fed the left hand side becomes this form
    /// with the condition turned over, and `a < b` folded on the left is `b > a`.
    CmpSetRm,
    /// The same reading its left hand side out of memory and its right off the instruction.
    ///
    /// [`Form::CmpSetRi`] with the register moved to an addressing mode, which is the change
    /// [`Form::AluMi`] is to [`Form::AluRi`]. The constant cannot be anywhere but on the
    /// instruction, so there is one arrangement here rather than two and the memory is always the
    /// left hand side, which is the side the machine subtracts from. Nothing about the condition
    /// changes, because nothing about the order of the two sides changes.
    ///
    /// Nothing but the byte is a register, so the description is the one a bare set has, and the
    /// address registers go behind it in the operand vector the way they do for every other form
    /// that names a place.
    ///
    /// No lowering rule produces one. `rucc_codegen::combine` writes one, out of a comparison
    /// against a constant whose register came from a load nothing else read.
    CmpSetMi,
    /// A comparison that keeps nothing but the flags.
    ///
    /// The same instruction as the first half of [`Form::CmpSet`] with the second half gone. It
    /// exists because a branch on the answer of a comparison does not need the answer in a
    /// register: the jump reads the flags the comparison set. Nothing selects one of these, since
    /// what it computes is not a value and a rule replaces a term with a term. The block layout
    /// writes one, in place of a comparison and a test it found next to each other, and writes the
    /// jump that reads its flags immediately after it.
    Cmp,
    /// The same against a constant, which is [`Form::CmpSetRi`] with the byte gone.
    CmpRi,
    /// The same against memory, which is [`Form::CmpSetRm`] with the byte gone.
    ///
    /// Written by the block layout for the reason [`Form::Cmp`] is, out of a folded comparison and
    /// the branch behind it. The one register it is left with moves down a place because the byte
    /// in front of it went, and the addressing mode stays as it was, so the positions the mode
    /// names have to come down with the operands. That is the layout's business and the reason it
    /// is written here is that this is the only form it has to do it for.
    CmpRm,
    /// The same against a constant, which is [`Form::CmpSetMi`] with the byte gone.
    ///
    /// Written by the block layout the way [`Form::CmpRm`] is. The byte was the only operand in
    /// front of the address, so what is left names nothing but the place, and the positions the
    /// addressing mode holds come down by one along with it.
    CmpMi,
    /// The byte a comparison sets, with the comparison gone.
    ///
    /// The other half of [`Form::CmpSet`], and it exists for the mirror of the reason [`Form::Cmp`]
    /// does. A comparison whose answer is already in the condition state still has to put that
    /// answer in a register if something wanted it there, and what is left of the pair then is the
    /// byte alone. Nothing selects one, for the reason nothing selects a [`Form::Cmp`]: a rule
    /// replaces a term with a term, and which comparison set the bits this reads is not something a
    /// pattern can see. `rucc_codegen::compare` writes one, in place of a comparison it found the
    /// machine had already made.
    ///
    /// One register written and none read, which makes it the only form here that names a
    /// destination and no source. What it really reads is the condition state, and the condition
    /// state is not an operand on this target, for the reason the module comment gives.
    Set,
    /// A move between widths, which reads one register and writes another.
    Convert,
    /// A search for a set bit, or a count of the bits above or below the lowest set one, which
    /// reads one register and writes another of the same width.
    ///
    /// The same operand vector as [`Form::Convert`] and a separate form because the two answers
    /// this description is asked for are both different. A conversion agrees with its source about
    /// every bit the narrower of the two widths has, and a search agrees with its source about
    /// nothing: what it writes is a position or a population, which is a number about the source
    /// rather than any part of it. And a conversion says nothing about the condition state, where
    /// every one of these writes it. Marking one of these a conversion would tell the bit tracking
    /// that a register holds bits it does not hold and tell the comparison pass that a comparison
    /// made before it is still standing, and either one is a wrong program rather than a slow one.
    Search,
    /// The bytes of one register turned round, which is one operand read and written in place.
    ///
    /// The same operand vector as [`Form::UnaryR`], which is what a negate and a complement are,
    /// and a separate form for the one thing that is not the same about it: `bswap` leaves the
    /// condition state exactly as it found it and every other instruction of that shape writes it.
    /// The default in [`crate::x86_64`] is that an instruction writes the flags, which is the safe
    /// way round for a description that may miss a name, so calling this a unary operation would
    /// not have been wrong. It would have been a claim this machine does not make, and the point of
    /// a form is that it is what the manual says rather than what is convenient.
    Swap,
    /// A multiply that keeps the whole of its answer, in the two registers it takes to hold it.
    ///
    /// The product of two numbers of a width is twice that width, and every other multiply on this
    /// machine throws the top half away. This one does not: it reads `rax` and one other place and
    /// writes the low half back to `rax` and the high half to `rdx`. So it is the first form here
    /// with two definitions that a program is meant to read, which is what makes it the one a
    /// bignum library asks for and cannot get any other way. A library that multiplies two limbs
    /// wants both halves, and building them out of four narrower multiplies costs what the
    /// instruction was put on the machine to save.
    ///
    /// The same shape as [`Form::DivQuo`] read the other way round, and the one thing that differs
    /// between them is worth saying. A division writes `rdx` early, because the register is filled
    /// before the divisor is read and so may not hold the divisor. Nothing fills anything here:
    /// both definitions happen after both sources have been read, which is what an ordinary
    /// definition means, and it is what lets the multiplier sit in `rdx` and still be read.
    MulWide,
    /// The quotient of a division, which comes back in `rax` and destroys `rdx` on the way.
    DivQuo,
    /// The remainder of a division, which comes back in `rdx` and destroys `rax` on the way.
    DivRem,
    /// A division of a dividend twice the width of its divisor, which keeps both of its answers.
    ///
    /// [`Form::MulWide`] undone, and the same thing the two above are with the halves the program
    /// does not care about put back. The dividend is `rdx` and `rax` read as one number, the divisor
    /// is the one place the text names, the quotient comes back in `rax` and the remainder in `rdx`.
    /// Five operands, which makes it the widest description here that is not the question put to the
    /// processor.
    ///
    /// What tells it from the two above is which way the high half of the dividend travels. Those
    /// two are each two instructions, because a division in C divides a number by a number of its
    /// own width and this machine divides a pair, so the compiler fills the high half itself and
    /// then divides, and the filling is why `rdx` is written early there. Here the program filled
    /// it. It is an operand this reads rather than a register something clobbered on the way in,
    /// and saying so is the whole difference: an early definition would tell the allocator the
    /// register is gone before the operands are read, which would be a lie about the one register
    /// that carries half the dividend.
    ///
    /// Nothing here checks that the quotient fits, on this form or on the two above, because the
    /// machine offers no way to: a quotient too wide for the register it lands in raises on the
    /// spot. A program that writes one of these has promised that the divisor is larger than the
    /// high half of the dividend, and libgmp writes that promise in the comment over `udiv_qrnnd`.
    DivWide,
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
    /// The second register a value comes back in, when it takes two of them.
    ///
    /// [`Form::RetVal`] one place further along the convention's list of return registers. A
    /// structure of at most sixteen bytes comes back in up to two registers, and which register
    /// each half goes in is the classification's answer, so a return of two values is built from
    /// the convention the way a call is rather than matched by a rule. There is no third of these
    /// because no convention this target has returns in three registers.
    RetVal2,
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
    /// that already set the flags, is the peephole `spec/10-backend.md` section 10.9 describes.
    /// It is not a rule and cannot be one, for the reason [`Form::CmpSet`] is one form rather than
    /// two: what the comparison leaves for the jump is the flags, and the flags are not a value a
    /// pattern could bind or a solver could be asked about.
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
    /// A test of a condition and the conditional move that reads its flags, as one instruction.
    ///
    /// The same argument [`Form::CmpSet`] is written under. The flags between the two halves are
    /// not a value a solver can be asked about, so the unit a rule names is the pair that produces
    /// the answer, and nothing may be put between them because there is no one instruction here to
    /// put it between.
    ///
    /// Three registers read and one written, and the written one is the value chosen when the
    /// condition is false, because that is what a conditional move is: the destination already
    /// holds one answer and the instruction overwrites it with the other. So the destination reuses
    /// the operand holding the false arm, which is the same two-address constraint the arithmetic
    /// has and is handled the same way, by a copy the allocator inserts when the false arm is still
    /// live afterwards.
    TestCmov,
    /// The move alone, reading a condition state something else left.
    ///
    /// [`Form::TestCmov`] with its front half gone, and it stands to that form the way [`Form::Cmp`]
    /// stands to [`Form::CmpSet`]. Nothing selects one, for the same reason nothing selects those:
    /// what it reads is the flags, and the flags are not a term a pattern can bind. What writes one
    /// is an `asm` template, where the program has written the comparison on one line and the move
    /// on the next and means exactly that, which is how a branchless select is written by somebody
    /// who does not trust the compiler to keep it branchless.
    ///
    /// Two registers read and one written, with the same `Reuse` the pair above has and for the
    /// same reason: the destination is the arm taken when the condition does not hold, because that
    /// is the value already sitting in the register the instruction may overwrite.
    ///
    /// What keeps it behind the comparison that set the flags, once the two are ordinary
    /// instructions in a block, is the scheduler's fourth kind of edge. It asks the target which
    /// instructions write the condition state and which read it, and this one is in the second list
    /// and not the first, so a comparison in front of it is something it waits for and a comparison
    /// behind it is something that waits for it.
    Cmov,
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
    /// A jump to the address in a register, which is what a computed `goto` comes to.
    ///
    /// The one branch here whose target is not on the block and cannot be, since which of the
    /// block's successors it arrives at is decided by the address while the program runs. They are
    /// all still successors, because everything downstream reads the block to find out where
    /// control can go and a target left off the list is a place the liveness and the layout would
    /// both believe control never reaches.
    ///
    /// One operand, which is the address, and that is the whole difference from
    /// [`Form::Call`]: a call through an address has an operand vector nothing can write down,
    /// because a call passes arguments and this passes none.
    JmpReg,
    /// A jump always taken whose target is a symbol rather than a block in this function.
    ///
    /// A tail jump out of the function. The name it goes to rides on the instruction the way a
    /// direct call's callee does, which is what tells it apart from [`Form::Jmp`]: that one asks
    /// the block where it goes, this one asks the linker.
    ///
    /// The block it ends has no successors, which is the same thing a `ret` leaves behind and is
    /// why nothing downstream needs a new answer about it. What is not the same is that no
    /// epilogue may be written into that block afterwards, and the only functions this appears in
    /// are the ones that have no epilogue anyway: it is read out of an `asm` template only in a
    /// function that is `naked`, which is micropython's `nlr_push` and nothing else so far.
    ///
    /// No operands, for [`Form::Jmp`]'s reason. A tail jump does pass the arguments of the
    /// function it goes to, and every one of those is already where the convention put it, because
    /// a naked function is the one kind whose incoming arguments nothing has moved.
    JmpAway,
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
    /// A barrier, which reads nothing, writes nothing and is only its effect on the order other
    /// instructions become visible in.
    ///
    /// The same empty operand list as [`Form::Ret`] and a separate form because a form is read as
    /// what an instruction is as well as what its operands are, and an epilogue and a fence have
    /// nothing to do with each other. Neither is reachable from a rule, and for the same shape of
    /// reason: there is nothing about either that a proof over bitvectors could discharge, since
    /// what makes them right is the frame in one case and the memory model in the other.
    Barrier,
    /// The instruction a program stops on, which reads nothing, writes nothing and never comes
    /// back.
    ///
    /// What `__builtin_trap` asks for. `ud2` is an opcode the manual promises will never be given
    /// a meaning, so the processor raises the fault for an instruction it does not know, and on
    /// Linux that arrives at the program as `SIGILL`. It needs no library, which is why it is the
    /// stop a kernel and a freestanding program can both write, and why it is two bytes rather
    /// than a call and a relocation.
    ///
    /// The same empty operand list as [`Form::Barrier`] and a form of its own for the same reason:
    /// a form is read as what an instruction is as well as what its operands are, and ordering
    /// memory and stopping the program have nothing to do with each other. No rule selects one,
    /// because there is nothing about stopping that a proof over bitvectors could discharge.
    Trap,
    /// A landing pad, which reads nothing, writes nothing and says that the address it is at is
    /// one an indirect call or jump is allowed to arrive at.
    ///
    /// What `-fcf-protection=branch` asks for. On a machine that checks, an indirect transfer to
    /// an address that is not one of these faults, so the set of addresses a corrupted function
    /// pointer can reach is the set of places somebody meant to be reachable that way rather than
    /// every byte of the program.
    ///
    /// The same empty operand list as [`Form::Barrier`] and a form of its own for the same reason:
    /// a fence and a landing pad are not the same kind of thing, and a form is read as what an
    /// instruction is as much as what its operands are. Nothing selects one. The only thing that
    /// writes one is a prologue, which is `rucc_codegen::finish`.
    Landing,
    /// A byte that does nothing, written so that something else can be written over it later.
    ///
    /// What `-fpatchable-function-entry=` asks for. The bytes are reserved rather than used: a
    /// tracer or a live patcher replaces them with a jump or a call once the program is running,
    /// and what it needs from the compiler is room at a known address and a promise that nothing
    /// jumps into the middle of it.
    ///
    /// The same empty operand list as [`Form::Landing`] and a form of its own for the same reason.
    /// A pad that means something to the hardware and a byte that means nothing to anybody are not
    /// the same kind of instruction, and nothing selects either: the only thing that writes one is
    /// a prologue, which is `rucc_codegen::finish`.
    Nop,
    /// A hint that the loop this is in is waiting for another thread, which reads nothing and
    /// writes nothing.
    ///
    /// What `pause` is, and what a spin lock writes between one read of the lock word and the
    /// next. The machine is told that the read it is about to do is the same read it just did, so
    /// it can stop guessing ahead and can let the other thread have the bus, and the loop leaves
    /// far less of a mess behind when the lock does come free. Like the hint below it, a processor
    /// that ignores the whole instruction is running the program correctly.
    ///
    /// Not a barrier, though it sits next to one in every spin loop ever written. It orders
    /// nothing, and a program that needs an ordering writes the ordering as well.
    ///
    /// The same empty operand list as [`Form::Nop`] and a form of its own for the same reason. No
    /// rule selects one: the only thing that writes one is an `asm` statement that asked for it by
    /// name.
    Spin,
    /// Where the next instruction starts, which is the one entry here that is not an instruction.
    ///
    /// Everything else in this table is a thing the processor does. This is a thing an assembler
    /// does: it says that whatever comes after it begins at an address that is a multiple of the
    /// number on it, and it is filled by whatever bytes it takes to get there. So it has no
    /// operands, reads nothing, writes nothing, and encodes to a run of bytes whose length is not
    /// known until the function has a place.
    ///
    /// It is here rather than somewhere else because the passes between selection and the writers
    /// have to see it. An alignment that the scheduler is allowed to move an instruction across is
    /// an alignment of something other than what the program pointed at, and the way a pass is told
    /// that is the same way it is told about a fence, which is a form in this table with a timing
    /// entry of [`crate::timing::Unit::Fixed`] beside it.
    ///
    /// Only an `asm` statement writes one, which is the same reason [`Form::Spin`] is here. zstd
    /// writes `__asm__(".p2align 5")` in front of the match loop of `ZSTD_compressBlock_lazy_generic`
    /// with a comment saying it measured a five per cent loss on two compression levels when the
    /// loop moved across a cache line boundary.
    Align,
    /// One byte of the instruction stream, written out as itself.
    ///
    /// The other form here that is not an instruction, and the other one only an `asm` statement
    /// writes. A template may say `.byte 0x0f, 0x01, 0xd0`, which is what a program writes when it
    /// wants an instruction its assembler was older than, and what comes back is one of these per
    /// byte with the byte on it. Both writers know it: the listing writes the directive back out
    /// and the object writer puts the byte straight in the text, and neither has anything left to
    /// work out, because a program that wrote the bytes has already answered the only question
    /// there was.
    ///
    /// One of these per directive rather than one per byte, because the bytes of one directive are
    /// one instruction of the program and everything that reaches a register through it reaches it
    /// once. They travel in the number an instruction already holds, which is what keeps this
    /// something the passes and the writers can carry without a field added for it, and [`LITERALS`]
    /// is how many fit.
    ///
    /// The registers are the other half of it and they come from the constraint letters, the way
    /// [`Form::CpuId`]'s do. There the description says which registers the instruction reaches and
    /// the letters say which operand is in each, and here the description cannot say: the bytes are
    /// a number and nothing in them is a register anybody can read. So the letters say both, and the
    /// lowering builds the operand list from them rather than from this table.
    Literal,
    /// What the processor is asked about itself, which reads two registers and writes four.
    ///
    /// The one form here whose every operand is fixed by the instruction and named by nothing
    /// that spells it. `cpuid` takes the leaf in `eax` and the subleaf in `ecx` and leaves its
    /// answer in all four of `eax`, `ebx`, `ecx` and `edx`, and the text is the mnemonic alone,
    /// so a reader of a template has to get the operands from the description rather than from
    /// what was written. That is why it is here at all: no rule selects one, nothing the front
    /// end reads asks for one, and the only thing that ever writes one is an `asm` statement,
    /// which is the same reason [`Form::Spin`] is here.
    ///
    /// Every program that asks a processor what it can do writes this, because there is no other
    /// way to ask. zstd writes four of them in `lib/common/cpu.h` to find out whether the machine
    /// it is running on has the vector instructions it would rather use.
    CpuId,
    /// A hint that an address is about to be used, which reads nothing and writes nothing.
    ///
    /// What `__builtin_prefetch` asks for. The machine is told to start bringing a line closer, and
    /// what it does about it is its own business: a processor that ignores the whole instruction is
    /// running the program correctly, since the only thing one of these can change is how long the
    /// program takes.
    ///
    /// An empty operand list and an addressing mode, which is the shape a probe has and nothing
    /// else here does. The address is the whole of what the instruction is given, and the registers
    /// it is built out of arrive through the addressing mode the way they do for a store, so there
    /// is no end in a register for an operand to point at. Unlike a store there is no value either,
    /// which is what makes the list empty rather than one long.
    Prefetch,
    /// A compare and exchange, which is the one instruction here that names four registers and
    /// only two of them by choice.
    ///
    /// What the machine does is compare what is at an address against `rax`, write the second
    /// source there when the two were equal, and leave what it found in `rax` either way. So `rax`
    /// is read and written and is not something the allocator picks, which is the same shape a
    /// division has and is written here the same way.
    ///
    /// The second definition is the byte saying whether the exchange went through, which the `setz`
    /// behind the instruction writes. It is a definition rather than a fixed register so that the
    /// allocator places it, and it is a definition at all so that the allocator knows a value lands
    /// there: two definitions of one instruction are live at the same point, so the register this
    /// gets is never `rax`, which is what keeps the `setz` from writing over the value.
    CmpXchg,
    /// A read modify write of a whole object at an address, which is one instruction on this
    /// machine for the exchange and for the add and is a loop for everything else.
    ///
    /// The same two operands as any other two-address arithmetic, and a separate form because the
    /// second place it works on is memory rather than a register: an addressing mode is on the
    /// instruction, which is the difference [`Form::takes_mem`] reads. The value it answers is the
    /// one that was there before, and it lands in the register the operand arrived in, which is what
    /// makes it two-address in the first place and is why the destination reuses the source.
    ///
    /// The `lock` in front is not part of this. An exchange with memory is indivisible on this
    /// machine whether the prefix is written or not, and an add is not, so the prefix belongs to the
    /// spelling of each instruction rather than to the shape they share.
    Rmw,
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
    /// [`Form::RetVal2`] in the other file.
    RetVal2Vec,
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
    /// Memory pushed onto the x87 stack, which is `fldt` and the four conversions that come up.
    ///
    /// The width and the format are in the opcode rather than in the form, because they are what
    /// the instruction does and not what the allocator has to arrange. `fldt` reads the format the
    /// stack already holds, `flds` and `fldl` read a narrower float and convert on the way in, and
    /// `fildl` and `fildll` read an integer. All five leave one value on the stack and none of them
    /// can be got wrong by an allocator, so all five are this.
    ///
    /// The first form here with no register operand of its own. It writes no register because the
    /// place the value lands is the top of the x87 stack, and `ClassInfo::allocatable` says why
    /// that is not a register anything may be allocated to: `st0` is wherever the top happens to
    /// be, so a name for it does not fix a register the way `rax` does. It reads no register
    /// either, for the same reason in the other direction. The registers it really touches are
    /// the ones in the addressing mode, and the builder puts those in the vector the way it does
    /// for every other instruction that carries an address.
    ///
    /// So the allocator sees an instruction that reads an address and does something, which is
    /// what a store looks like to it, and that is the whole of what it has to know.
    PushX87,
    /// The top of the x87 stack popped into memory, which is `fstpt` and the four that go down.
    ///
    /// The other half of [`Form::PushX87`] and the same operand list. Every use of the x87 stack
    /// this target makes is one of these behind one or more of those, which is the discipline
    /// `spec/10-backend.md` section 10.8 writes down: the stack is empty before the first push of
    /// a group and empty again after the last pop, so no two groups can be interleaved and nothing
    /// depends on how deep the stack was when a group started.
    ///
    /// Every one of them pops, which is why there is no form here for the ones that do not.
    /// `fst` without the `p` exists and nothing selects it, since a value that stays on the stack
    /// after it has been written out is a value the next group would have to know about.
    PopX87,
    /// The x87 control word read out of the unit or written back into it.
    ///
    /// `fnstcw` and `fldcw`, which are the only two instructions here that touch the x87 and are
    /// neither a push nor a pop. The operand list is the same as the two above and the reason for
    /// a form of their own is the same reason a barrier is not a return: a form says what an
    /// instruction is as well as what its operands are, and the depth of the stack is the thing
    /// the other two forms are read for.
    ///
    /// What they are for is the one C conversion this machine has no single instruction for. C
    /// cuts a float towards zero and the x87 rounds the way its control word says, which is to
    /// nearest, so an eighty bit float becoming an integer is the control word saved, changed,
    /// used and put back. `spec/10-backend.md` section 10.8 writes the group out and says why it
    /// is that rather than `fisttp`.
    CtrlX87,
    /// Arithmetic on the two values at the top of the x87 stack, which leaves one.
    ///
    /// The same empty operand list the three above have and the same reason for it, one step
    /// further on: both sources and the destination are depths on a stack nothing allocates from,
    /// so there is nothing here for the allocator to arrange at all. This is the first form in
    /// this table with no operands and no address either, which makes it the first instruction the
    /// allocator sees that touches nothing it knows about.
    ///
    /// Which of the two values is on top is the code generator's business and is the whole of what
    /// a subtraction and a division have two of these for. A pair of registers can be named in
    /// either order and a pair of depths cannot, so `fsubp` and `fsubrp` are two instructions
    /// rather than one instruction written twice.
    ArithX87,
    /// Arithmetic on the top of the x87 stack alone, which leaves it where it was.
    ///
    /// A sign flipped and a sign cleared, which are the two things this machine does to an eighty
    /// bit float without reading it as a number. Neither raises on anything, neither rounds, and
    /// neither can be got wrong by an allocator, so both are this.
    ///
    /// Separate from [`Form::ArithX87`] because it does not pop. The depth of the stack after one
    /// of these is the depth before it, and the depth is what these forms are read for.
    UnaryX87,
    /// A comparison of the two values at the top of the x87 stack and the byte it sets.
    ///
    /// [`Form::CmpSetVec`] on the other unit, and the same argument: what the comparison writes is
    /// the flags, reading the flags is `setcc` and nothing else, and the two are one opcode here
    /// because nothing in between them is a value a rule could name. The destination is a general
    /// purpose register because a truth value is a byte.
    ///
    /// Three instructions rather than two, and the third is the one worth writing down. The
    /// comparison takes one value off the stack and there were two on it, so a pop that throws its
    /// value away is part of this opcode. Leaving it to whatever came next would be leaving the
    /// stack deeper than the group found it, and `spec/10-backend.md` section 10.8 is a rule about
    /// a group rather than about a block.
    CmpSetX87,
    /// The same, when the condition takes two of those bytes and a boolean operation to spell.
    ///
    /// [`Form::CmpSetVecBoth`] word for word, because the flags an x87 comparison writes are the
    /// flags a vector comparison writes: less, greater, equal or unordered in three bits, with
    /// every predicate but two being one of them. The second byte is a second definition for the
    /// same reason it is there.
    CmpSetX87Both,
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
// What the processor is asked about itself. Every one of its operands is fixed by the instruction
// and none of them is written anywhere it is spelled, which is what makes it the first form here
// that an `asm` template can name and no rule can select. The leaf goes in `eax` and the subleaf
// in `ecx`, and all four registers come back written, `ebx` among them, which is why a function
// that asks gets `rbx` saved in its prologue like any other register it writes.
static CPU_ID: [OperandDesc; 6] = [
    OperandDesc::write(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::write(GPR).with(Constraint::Fixed(RBX)),
    OperandDesc::write(GPR).with(Constraint::Fixed(RCX)),
    OperandDesc::write(GPR).with(Constraint::Fixed(RDX)),
    OperandDesc::read(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::read(GPR).with(Constraint::Fixed(RCX)),
];
// One multiplicand is in `rax` and the other is anywhere else, and both halves of the product come
// back, the low one in `rax` and the high one in `rdx`. Two definitions and both of them wanted,
// which is what tells this from the two below: a division writes the register its other answer is
// in so that nothing is left there, and this writes it because that is where half the answer is.
//
// Neither definition is early, which is the other thing that is not the same as a division. Early
// says the register is gone before the operands are read, and it is true of a division because the
// sign extension that fills `rdx` runs in front of it. Nothing runs in front of this one, so `rdx`
// still holds whatever it held when the sources are read, and saying so is what lets the second
// multiplicand be allocated there.
static MUL_WIDE: [OperandDesc; 4] = [
    OperandDesc::write(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::write(GPR).with(Constraint::Fixed(RDX)),
    OperandDesc::read(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::read(GPR),
];
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
// The remainder is the answer this one keeps, and it is in `rdx`, which is the register the sign
// extension fills before the division reads its divisor. So `rdx` is written early here as well,
// even though it is what the instruction produces rather than the answer it throws away: what an
// early definition says is that the register is gone before the operands are read, and that is
// true of this one whichever of the two answers ends up in it.
static DIV_REM: [OperandDesc; 4] = [
    OperandDesc::write_early(GPR).with(Constraint::Fixed(RDX)),
    OperandDesc::write_early(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::read(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::read(GPR),
];
// The division a program writes out for itself, whose dividend is the pair of registers rather than
// one of them widened. Both answers are kept, the quotient in `rax` and the remainder in `rdx`, and
// both halves of the dividend are read out of the same two registers the answers land in.
//
// Neither definition is early, which is the one thing that is not the same as the two above. Early
// says the register is gone before the operands are read, and it is true up there because the
// instruction that fills the high half of the dividend runs first. Nothing runs in front of this
// one: the program put both halves where they are, so `rdx` holds an operand this reads rather than
// a register something has already destroyed, and calling it early would be a lie about the half of
// the dividend that lives there.
static DIV_WIDE: [OperandDesc; 5] = [
    OperandDesc::write(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::write(GPR).with(Constraint::Fixed(RDX)),
    OperandDesc::read(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::read(GPR).with(Constraint::Fixed(RDX)),
    OperandDesc::read(GPR),
];
// A compare and exchange, whose first two entries are the two values it produces and whose last
// two are the value it compares against and the value it puts there. `rax` is fixed at both ends
// because the machine reads the expected value out of it and leaves what it found in it, and the
// address is not here for the reason no address is: the builder appends the registers of the
// addressing mode behind everything written down.
static CMPXCHG: [OperandDesc; 4] = [
    OperandDesc::write(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::write(GPR),
    OperandDesc::read(GPR).with(Constraint::Fixed(RAX)),
    OperandDesc::read(GPR),
];
// A read modify write, whose two entries are the value that was there before and the value the
// operation is done with. They are one register: the instruction leaves the old value in the
// register it read the operand out of, which is the same two-address shape the arithmetic above has
// and is said the same way. The address is not here for the reason no address is.
static READ_MODIFY_WRITE: [OperandDesc; 2] =
    [OperandDesc::write(GPR).with(Constraint::Reuse(1)), OperandDesc::read(GPR)];
static ADDRESS: [OperandDesc; 1] = [OperandDesc::write(GPR)];
// A load writes one register and reads none, because the registers it reads are the ones in
// the addressing mode and the builder is what puts those in the vector.
static LOAD: [OperandDesc; 1] = [OperandDesc::write(GPR)];
// A store writes nothing. It is the first instruction here that produces no value, which is
// what having an effect means, and the allocator needs no more than that: an instruction with
// no definition keeps nothing alive past it. Arithmetic that leaves its answer in memory is the
// same shape and shares the description: one register read, an addressing mode, and nothing the
// allocator has to find a place for.
static STORE: [OperandDesc; 1] = [OperandDesc::read(GPR)];
// The same arithmetic again with the register source replaced by a constant, which leaves no
// register to describe at all. Everything the instruction reads is either in the addressing mode,
// where the builder puts it, or on the instruction as an immediate, where nothing can be allocated
// to it. An empty description is the whole truth about it and is not the same emptiness a call has:
// a call's operands are a fact about a signature this table cannot see, and these really are none.
static ALU_MI: [OperandDesc; 0] = [];
// An integer comes back in `rax` on every convention this machine has, which is why the register
// is written here rather than read out of the convention the session was given. A test checks it
// against `SYSV` and `WIN64` rather than leaving it as something a reader has to take on trust,
// and a convention that ever disagrees is one that will fail that test rather than compile.
static RET_VAL: [OperandDesc; 1] = [OperandDesc::read(GPR).with(Constraint::Fixed(RAX))];
// The second half of a structure that comes back in two registers, which is `rdx` on the one
// convention that has a second register to come back in. Written here for the reason above and
// held against the convention by the same test.
static RET_VAL_2: [OperandDesc; 1] = [OperandDesc::read(GPR).with(Constraint::Fixed(RDX))];
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
// The condition, the arm taken when it holds and the arm taken when it does not. The destination
// is the false arm, since a conditional move overwrites what is already in the register, which is
// the same shape the two-address arithmetic above has and gets the same `Reuse`.
static TEST_CMOV: [OperandDesc; 4] = [
    OperandDesc::write(GPR).with(Constraint::Reuse(1)),
    OperandDesc::read(GPR),
    OperandDesc::read(GPR),
    OperandDesc::read(GPR),
];
// The same three without the condition in front of them, which is `TEST_CMOV` with the operand the
// test read taken off. The destination is still the false arm and still reuses it.
static CMOV: [OperandDesc; 3] = [
    OperandDesc::write(GPR).with(Constraint::Reuse(1)),
    OperandDesc::read(GPR),
    OperandDesc::read(GPR),
];
static TEST: [OperandDesc; 1] = [OperandDesc::read(GPR)];
// A comparison that keeps only the flags, which is `TWO_TO_ONE` and `ONE_TO_ONE` with the byte
// they wrote gone. Both sources stay reads and neither is tied to anything, since there is no
// destination left for either of them to be destroyed by.
// `CMP_RI` is what a comparison against memory has as well as one against a constant. Both name
// one register and take their other side off the instruction, and an address is not an operand
// here any more than an immediate is.
static CMP: [OperandDesc; 2] = [OperandDesc::read(GPR), OperandDesc::read(GPR)];
static CMP_RI: [OperandDesc; 1] = [OperandDesc::read(GPR)];
// A jump reads nothing and writes nothing. Where it goes is on the block, not in an operand.
static JUMP: [OperandDesc; 0] = [];
// The address a computed goto jumps to, which is the one operand a branch here has. Where it
// goes is still the block's successors, since the register holds one of them and nothing knows
// which.
static JUMP_REG: [OperandDesc; 1] = [OperandDesc::read(GPR)];
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
// [`RET_VAL_2`] in the other file, and `xmm1` for the same reason `rdx` is.
static RET_VAL_2_VEC: [OperandDesc; 1] = [OperandDesc::read(XMM).with(Constraint::Fixed(xmm(1)))];
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

// An x87 instruction names nothing at all. Every other instruction here has at least one operand
// because it has at least one end in a register the allocator picked, and these have no end there:
// where one is an addressing mode the builder appends its registers, and everywhere else it is a
// depth on a stack nothing allocates from. So the operand vector of one of these holds exactly the
// registers of its address, and for the ones with no address it is empty.
static X87_MEM: [OperandDesc; 0] = [];
// The one exception, which is the comparison, because a truth value is a byte and a byte is not
// something the x87 holds. `VEC_TO_ONE` with the two sources gone: they are on the stack, and the
// stack is not somewhere an operand can point.
// A hint names nothing either, and for the first half of the reason above rather than the second:
// the registers its address is built out of are the address's own, and there is no second end.
static HINT: [OperandDesc; 0] = [];
// A byte written and nothing read, which is what a condition put in a register is whichever unit
// made the comparison. The x87 comparison is one of these because a truth value is a byte and a
// byte is not something the x87 holds, and `set_e` is one because the comparison it belongs to is
// somewhere further up the block.
static ONE_WRITTEN: [OperandDesc; 1] = [OperandDesc::write(GPR)];
static X87_TO_ONE_BOTH: [OperandDesc; 2] = [OperandDesc::write(GPR), OperandDesc::write(GPR)];

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
            AluRr | AluCarry => &TWO_ADDRESS_RR,
            AluRi | AluCarryI | AluRm | UnaryR | ShiftRi | Swap => &TWO_ADDRESS_RI,
            ShiftCl => &SHIFT_CL,
            CmpSet => &TWO_TO_ONE,
            CmpSetRi | CmpSetRm => &ONE_TO_ONE,
            Cmp => &CMP,
            CmpRi | CmpRm => &CMP_RI,
            CmpSetMi => &ONE_WRITTEN,
            CmpMi => &ALU_MI,
            Set => &ONE_WRITTEN,
            Convert | Search => &ONE_TO_ONE,
            CpuId => &CPU_ID,
            MulWide => &MUL_WIDE,
            DivQuo => &DIV_QUO,
            DivRem => &DIV_REM,
            DivWide => &DIV_WIDE,
            Lea => &ADDRESS,
            Load => &LOAD,
            AluMr | Store => &STORE,
            AluMi => &ALU_MI,
            RetVal => &RET_VAL,
            RetVal2 => &RET_VAL_2,
            ArgVal => &ARG_VAL,
            BrCond => &BR_COND,
            Call => &CALL,
            Test => &TEST,
            TestCmov => &TEST_CMOV,
            Cmov => &CMOV,
            Jcc | Jmp | JmpAway => &JUMP,
            JmpReg => &JUMP_REG,
            Move => &ONE_TO_ONE,
            Push => &PUSH,
            Pop => &POP,
            Ret | Barrier | Landing | Nop | Spin | Trap | Align | Literal => &LEAVE,
            Prefetch => &HINT,
            CmpXchg => &CMPXCHG,
            Rmw => &READ_MODIFY_WRITE,
            MoveVec => &VEC_TO_VEC,
            LoadVec => &LOAD_VEC,
            StoreVec => &STORE_VEC,
            AluVec => &TWO_ADDRESS_VEC,
            RetValVec => &RET_VAL_VEC,
            RetVal2Vec => &RET_VAL_2_VEC,
            ArgValVec => &ARG_VAL_VEC,
            ConvertVec => &VEC_TO_VEC,
            ConvertToVec => &GPR_TO_VEC,
            ConvertFromVec => &VEC_TO_GPR,
            CmpSetVec => &VEC_TO_ONE,
            CmpSetVecBoth => &VEC_TO_ONE_BOTH,
            PushX87 | PopX87 | CtrlX87 | ArithX87 | UnaryX87 => &X87_MEM,
            CmpSetX87 => &ONE_WRITTEN,
            CmpSetX87Both => &X87_TO_ONE_BOTH,
        }
    }

    /// Whether an instruction of this form carries an immediate.
    #[must_use]
    pub fn takes_imm(self) -> bool {
        matches!(
            self,
            LoadImm | AluRi | AluCarryI | AluMi | ShiftRi | CmpSetRi | CmpRi | CmpSetMi | CmpMi
        )
    }

    /// Whether an instruction of this form carries an addressing mode.
    #[must_use]
    pub fn takes_mem(self) -> bool {
        matches!(
            self,
            Lea | Load
                | AluRm
                | AluMr
                | AluMi
                | CmpSetRm
                | CmpRm
                | CmpSetMi
                | CmpMi
                | Store
                | LoadVec
                | StoreVec
                | PushX87
                | PopX87
                | CtrlX87
                | CmpXchg
                | Rmw
                | Prefetch
        )
    }

    /// Whether an instruction of this form reads or writes memory.
    ///
    /// Asked by a pass that wants to move a memory access from where it is to somewhere later,
    /// which is safe while nothing it passes touches memory at all. Reading and writing are one
    /// question here rather than two, because moving a read past a read is still a reordering of
    /// two accesses, and the machine IR does not say which accesses the program insisted on: a
    /// `volatile` read and an ordinary one are the same instruction with the same operands by the
    /// time anything here can see them.
    ///
    /// [`Lea`] is not on the list and is the reason the question is not simply whether the form has
    /// an addressing mode. It names an address and computes it and reads nothing there, which is
    /// the whole of what it is for.
    ///
    /// [`Push`], [`Pop`], [`Ret`] and [`Call`] are on it and have no addressing mode at all, which
    /// is the reason from the other side. What they touch is the stack and the instruction does not
    /// spell it out.
    ///
    /// A call answers `true` here and is still not enough on its own. What a call does to memory is
    /// not something the instruction says, which is why [`crate::MachineInsts::calls`] exists as a
    /// separate question, and a pass that has to know what survived a call has to ask that one too.
    #[must_use]
    pub fn touches_mem(self) -> bool {
        matches!(
            self,
            Load | AluRm
                | AluMr
                | AluMi
                | CmpSetRm
                | CmpRm
                | CmpSetMi
                | CmpMi
                | Store
                | LoadVec
                | StoreVec
                | PushX87
                | PopX87
                | CtrlX87
                | CmpXchg
                | Rmw
                | Prefetch
                | Push
                | Pop
                | Ret
                | Call
        )
    }
}

/// The opcode that says where the next instruction starts, named so that the writers can ask for it.
///
/// Every other opcode reaches a writer through the tables, which say what it is spelled as and what
/// bytes it encodes to. This one is spelled as nothing and encodes to a run of padding whose length
/// depends on where the function landed, so each writer has to recognise it and answer in its own
/// terms. Naming it here is what keeps the two of them asking about the same opcode, rather than
/// each carrying its own copy of the string. See [`Form::Align`].
pub const ALIGN: &str = "align";

/// The opcode that is one byte of the instruction stream, named the way [`ALIGN`] is.
///
/// Spelled as the directive a template wrote it as, and carrying its bytes as its immediate. See
/// [`Form::Literal`].
pub const LITERAL: &str = "byte";

/// The most bytes one `.byte` directive may carry, which is how many fit beside the opcode.
///
/// They ride in the number a machine instruction already has rather than in a field added for them,
/// and one byte of that number says how many there are, which leaves seven. That is more than any
/// instruction written this way needs: the longest instruction this machine has is fifteen bytes and
/// the ones programs still spell out are two to four, because what they are is an instruction older
/// than somebody's assembler rather than a run of data. A directive with more in it is refused
/// rather than split across two instructions, since the bytes of one directive are one instruction
/// of the program and splitting them would put a place the allocator may write in the middle of it.
pub const LITERALS: usize = 7;

/// Those bytes as the number a [`LITERAL`] instruction carries, or nothing for too many of them.
///
/// The count goes in the top byte and the bytes themselves go below it in the order they were
/// written, so the number is never negative and the two writers read it back the same way.
#[must_use]
pub fn packed(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() || bytes.len() > LITERALS {
        return None;
    }
    let mut packed = u64::try_from(bytes.len()).ok()? << 56;
    for (at, &byte) in bytes.iter().enumerate() {
        packed |= u64::from(byte) << (at * 8);
    }
    i64::try_from(packed).ok()
}

/// The bytes back out of that number, in the order the directive wrote them.
pub fn unpacked(imm: i64) -> impl Iterator<Item = u8> {
    let bits = u64::from_ne_bytes(imm.to_ne_bytes());
    let count = usize::try_from(bits >> 56).unwrap_or(0).min(LITERALS);
    (0..count).map(move |at| u8::try_from((bits >> (at * 8)) & 0xff).unwrap_or(0))
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
    // The pair of these that carries a bit from one instruction to the next. No rule selects one,
    // because a rule replaces a term with a term and the bit between these two is not a term this
    // compiler has: an addition in C is an addition of a width, and the carry out of one is a thing
    // only the program that wrote the two halves knows it wants. A template is where that program
    // says so, which is `add_ssaaaa` and `sub_ddmmss` in libgmp's `longlong.h`. Four widths and not
    // three, unlike the pair forms below, because there is nothing special about the byte one here.
    ("adc_rr_8", AluCarry),
    ("adc_rr_16", AluCarry),
    ("adc_rr_32", AluCarry),
    ("adc_rr_64", AluCarry),
    ("sbb_rr_8", AluCarry),
    ("sbb_rr_16", AluCarry),
    ("sbb_rr_32", AluCarry),
    ("sbb_rr_64", AluCarry),
    ("adc_ri_8", AluCarryI),
    ("adc_ri_16", AluCarryI),
    ("adc_ri_32", AluCarryI),
    ("adc_ri_64", AluCarryI),
    ("sbb_ri_8", AluCarryI),
    ("sbb_ri_16", AluCarryI),
    ("sbb_ri_32", AluCarryI),
    ("sbb_ri_64", AluCarryI),
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
    // The same arithmetic with the second source in memory, which is every one of the above
    // except the eight bit multiply. That one is written as a thirty two bit `imul` because the
    // machine has no narrower two-operand multiply, and reading thirty two bits out of memory
    // where the program asked for eight is a read of three bytes nobody said were there.
    ("add_rm_8", AluRm),
    ("add_rm_16", AluRm),
    ("add_rm_32", AluRm),
    ("add_rm_64", AluRm),
    ("sub_rm_8", AluRm),
    ("sub_rm_16", AluRm),
    ("sub_rm_32", AluRm),
    ("sub_rm_64", AluRm),
    ("and_rm_8", AluRm),
    ("and_rm_16", AluRm),
    ("and_rm_32", AluRm),
    ("and_rm_64", AluRm),
    ("or_rm_8", AluRm),
    ("or_rm_16", AluRm),
    ("or_rm_32", AluRm),
    ("or_rm_64", AluRm),
    ("xor_rm_8", AluRm),
    ("xor_rm_16", AluRm),
    ("xor_rm_32", AluRm),
    ("xor_rm_64", AluRm),
    ("imul_rm_16", AluRm),
    ("imul_rm_32", AluRm),
    ("imul_rm_64", AluRm),
    // The same arithmetic again with the answer left in memory, which is the five operations that
    // have a form like that. The multiply does not: `imul` on this machine writes a register and
    // reads the other side wherever it is, so there is no row for one here.
    ("add_mr_8", AluMr),
    ("add_mr_16", AluMr),
    ("add_mr_32", AluMr),
    ("add_mr_64", AluMr),
    ("sub_mr_8", AluMr),
    ("sub_mr_16", AluMr),
    ("sub_mr_32", AluMr),
    ("sub_mr_64", AluMr),
    ("and_mr_8", AluMr),
    ("and_mr_16", AluMr),
    ("and_mr_32", AluMr),
    ("and_mr_64", AluMr),
    ("or_mr_8", AluMr),
    ("or_mr_16", AluMr),
    ("or_mr_32", AluMr),
    ("or_mr_64", AluMr),
    ("xor_mr_8", AluMr),
    ("xor_mr_16", AluMr),
    ("xor_mr_32", AluMr),
    ("xor_mr_64", AluMr),
    // The same five again with the other source a constant rather than a register, which is what a
    // program that adds one to a counter in memory needs. The eight bit inclusive or is also the
    // instruction a probing prologue writes to touch a page, and it is one row here rather than two
    // because it is one instruction.
    ("add_mi_8", AluMi),
    ("add_mi_16", AluMi),
    ("add_mi_32", AluMi),
    ("add_mi_64", AluMi),
    ("sub_mi_8", AluMi),
    ("sub_mi_16", AluMi),
    ("sub_mi_32", AluMi),
    ("sub_mi_64", AluMi),
    ("and_mi_8", AluMi),
    ("and_mi_16", AluMi),
    ("and_mi_32", AluMi),
    ("and_mi_64", AluMi),
    ("or_mi_8", AluMi),
    ("or_mi_16", AluMi),
    ("or_mi_32", AluMi),
    ("or_mi_64", AluMi),
    ("xor_mi_8", AluMi),
    ("xor_mi_16", AluMi),
    ("xor_mi_32", AluMi),
    ("xor_mi_64", AluMi),
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
    // Adding one and taking one away, which no rule selects and the size directed peephole writes
    // instead. An addition of one against a register is three bytes, one for the opcode, one saying
    // which register and one for the number, and these are two, since the number is in the opcode.
    //
    // They are not the same instruction as the addition and that is why a rule cannot have them.
    // An addition writes the carry and these leave it as they found it, so whether the exchange is
    // allowed is a question about what reads the carry behind them, which is a question about the
    // instructions around one rather than about the instruction. `crate::short` is where a target
    // says which addition has one of these, and `rucc_codegen::shorten` is the walk that asks.
    ("inc_r_8", UnaryR),
    ("inc_r_16", UnaryR),
    ("inc_r_32", UnaryR),
    ("inc_r_64", UnaryR),
    ("dec_r_8", UnaryR),
    ("dec_r_16", UnaryR),
    ("dec_r_32", UnaryR),
    ("dec_r_64", UnaryR),
    // The multiply that keeps both halves of its product, signed and unsigned. No rule selects one,
    // and the reason is that nothing in the IR asks for a product wider than its operands: a C
    // multiply of two values of a type is a value of that type, and the wide product is something
    // only a library that is building arithmetic out of limbs wants. So what reaches one is a
    // program that named it, which is `umul_ppmm` in libgmp's `longlong.h`.
    //
    // Three widths and not four. The eight bit form is a different instruction wearing the same
    // name: its answer is sixteen bits and both halves of it are in `ax`, so it writes one register
    // where these write two and the description above is not true of it. It has encoder rows, the
    // way every one of these does, and it gets an opcode on the day something reaches it.
    ("mul_wide_16", MulWide),
    ("mul_wide_32", MulWide),
    ("mul_wide_64", MulWide),
    ("imul_wide_16", MulWide),
    ("imul_wide_32", MulWide),
    ("imul_wide_64", MulWide),
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
    // The division a program writes out for itself, which divides a pair of registers and keeps
    // both of its answers. No rule selects one, for the reason no rule selects the multiply above:
    // a division in C divides a number by a number of its own width, so the term a rule would match
    // is one of the two just above and the wide dividend is not a term at all. What reaches one is
    // `udiv_qrnnd` in libgmp's `longlong.h`, which is how that library does long division a limb at
    // a time.
    //
    // Three widths and not four, for the reason the multiply has three. The eight bit form divides
    // the whole of `ax` and leaves the quotient in `al` and the remainder in `ah`, so both of its
    // answers are in one register and it reads one where these read two.
    ("div_wide_16", DivWide),
    ("div_wide_32", DivWide),
    ("div_wide_64", DivWide),
    ("idiv_wide_16", DivWide),
    ("idiv_wide_32", DivWide),
    ("idiv_wide_64", DivWide),
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
    // The same ten conditions against a constant, which is four comparisons in five.
    ("cmp_set_e_ri_8", CmpSetRi),
    ("cmp_set_e_ri_16", CmpSetRi),
    ("cmp_set_e_ri_32", CmpSetRi),
    ("cmp_set_e_ri_64", CmpSetRi),
    ("cmp_set_ne_ri_8", CmpSetRi),
    ("cmp_set_ne_ri_16", CmpSetRi),
    ("cmp_set_ne_ri_32", CmpSetRi),
    ("cmp_set_ne_ri_64", CmpSetRi),
    ("cmp_set_l_ri_8", CmpSetRi),
    ("cmp_set_l_ri_16", CmpSetRi),
    ("cmp_set_l_ri_32", CmpSetRi),
    ("cmp_set_l_ri_64", CmpSetRi),
    ("cmp_set_le_ri_8", CmpSetRi),
    ("cmp_set_le_ri_16", CmpSetRi),
    ("cmp_set_le_ri_32", CmpSetRi),
    ("cmp_set_le_ri_64", CmpSetRi),
    ("cmp_set_g_ri_8", CmpSetRi),
    ("cmp_set_g_ri_16", CmpSetRi),
    ("cmp_set_g_ri_32", CmpSetRi),
    ("cmp_set_g_ri_64", CmpSetRi),
    ("cmp_set_ge_ri_8", CmpSetRi),
    ("cmp_set_ge_ri_16", CmpSetRi),
    ("cmp_set_ge_ri_32", CmpSetRi),
    ("cmp_set_ge_ri_64", CmpSetRi),
    ("cmp_set_b_ri_8", CmpSetRi),
    ("cmp_set_b_ri_16", CmpSetRi),
    ("cmp_set_b_ri_32", CmpSetRi),
    ("cmp_set_b_ri_64", CmpSetRi),
    ("cmp_set_be_ri_8", CmpSetRi),
    ("cmp_set_be_ri_16", CmpSetRi),
    ("cmp_set_be_ri_32", CmpSetRi),
    ("cmp_set_be_ri_64", CmpSetRi),
    ("cmp_set_a_ri_8", CmpSetRi),
    ("cmp_set_a_ri_16", CmpSetRi),
    ("cmp_set_a_ri_32", CmpSetRi),
    ("cmp_set_a_ri_64", CmpSetRi),
    ("cmp_set_ae_ri_8", CmpSetRi),
    ("cmp_set_ae_ri_16", CmpSetRi),
    ("cmp_set_ae_ri_32", CmpSetRi),
    ("cmp_set_ae_ri_64", CmpSetRi),
    // The same ten conditions with the right hand side read out of memory, which is the
    // shape `rucc_codegen::combine` writes where the register one of the two sides came out
    // of a load nothing else wanted.
    ("cmp_set_e_rm_8", CmpSetRm),
    ("cmp_set_e_rm_16", CmpSetRm),
    ("cmp_set_e_rm_32", CmpSetRm),
    ("cmp_set_e_rm_64", CmpSetRm),
    ("cmp_set_ne_rm_8", CmpSetRm),
    ("cmp_set_ne_rm_16", CmpSetRm),
    ("cmp_set_ne_rm_32", CmpSetRm),
    ("cmp_set_ne_rm_64", CmpSetRm),
    ("cmp_set_l_rm_8", CmpSetRm),
    ("cmp_set_l_rm_16", CmpSetRm),
    ("cmp_set_l_rm_32", CmpSetRm),
    ("cmp_set_l_rm_64", CmpSetRm),
    ("cmp_set_le_rm_8", CmpSetRm),
    ("cmp_set_le_rm_16", CmpSetRm),
    ("cmp_set_le_rm_32", CmpSetRm),
    ("cmp_set_le_rm_64", CmpSetRm),
    ("cmp_set_g_rm_8", CmpSetRm),
    ("cmp_set_g_rm_16", CmpSetRm),
    ("cmp_set_g_rm_32", CmpSetRm),
    ("cmp_set_g_rm_64", CmpSetRm),
    ("cmp_set_ge_rm_8", CmpSetRm),
    ("cmp_set_ge_rm_16", CmpSetRm),
    ("cmp_set_ge_rm_32", CmpSetRm),
    ("cmp_set_ge_rm_64", CmpSetRm),
    ("cmp_set_b_rm_8", CmpSetRm),
    ("cmp_set_b_rm_16", CmpSetRm),
    ("cmp_set_b_rm_32", CmpSetRm),
    ("cmp_set_b_rm_64", CmpSetRm),
    ("cmp_set_be_rm_8", CmpSetRm),
    ("cmp_set_be_rm_16", CmpSetRm),
    ("cmp_set_be_rm_32", CmpSetRm),
    ("cmp_set_be_rm_64", CmpSetRm),
    ("cmp_set_a_rm_8", CmpSetRm),
    ("cmp_set_a_rm_16", CmpSetRm),
    ("cmp_set_a_rm_32", CmpSetRm),
    ("cmp_set_a_rm_64", CmpSetRm),
    ("cmp_set_ae_rm_8", CmpSetRm),
    ("cmp_set_ae_rm_16", CmpSetRm),
    ("cmp_set_ae_rm_32", CmpSetRm),
    ("cmp_set_ae_rm_64", CmpSetRm),
    ("cmp_set_e_mi_8", CmpSetMi),
    ("cmp_set_e_mi_16", CmpSetMi),
    ("cmp_set_e_mi_32", CmpSetMi),
    ("cmp_set_e_mi_64", CmpSetMi),
    ("cmp_set_ne_mi_8", CmpSetMi),
    ("cmp_set_ne_mi_16", CmpSetMi),
    ("cmp_set_ne_mi_32", CmpSetMi),
    ("cmp_set_ne_mi_64", CmpSetMi),
    ("cmp_set_l_mi_8", CmpSetMi),
    ("cmp_set_l_mi_16", CmpSetMi),
    ("cmp_set_l_mi_32", CmpSetMi),
    ("cmp_set_l_mi_64", CmpSetMi),
    ("cmp_set_le_mi_8", CmpSetMi),
    ("cmp_set_le_mi_16", CmpSetMi),
    ("cmp_set_le_mi_32", CmpSetMi),
    ("cmp_set_le_mi_64", CmpSetMi),
    ("cmp_set_g_mi_8", CmpSetMi),
    ("cmp_set_g_mi_16", CmpSetMi),
    ("cmp_set_g_mi_32", CmpSetMi),
    ("cmp_set_g_mi_64", CmpSetMi),
    ("cmp_set_ge_mi_8", CmpSetMi),
    ("cmp_set_ge_mi_16", CmpSetMi),
    ("cmp_set_ge_mi_32", CmpSetMi),
    ("cmp_set_ge_mi_64", CmpSetMi),
    ("cmp_set_b_mi_8", CmpSetMi),
    ("cmp_set_b_mi_16", CmpSetMi),
    ("cmp_set_b_mi_32", CmpSetMi),
    ("cmp_set_b_mi_64", CmpSetMi),
    ("cmp_set_be_mi_8", CmpSetMi),
    ("cmp_set_be_mi_16", CmpSetMi),
    ("cmp_set_be_mi_32", CmpSetMi),
    ("cmp_set_be_mi_64", CmpSetMi),
    ("cmp_set_a_mi_8", CmpSetMi),
    ("cmp_set_a_mi_16", CmpSetMi),
    ("cmp_set_a_mi_32", CmpSetMi),
    ("cmp_set_a_mi_64", CmpSetMi),
    ("cmp_set_ae_mi_8", CmpSetMi),
    ("cmp_set_ae_mi_16", CmpSetMi),
    ("cmp_set_ae_mi_32", CmpSetMi),
    ("cmp_set_ae_mi_64", CmpSetMi),
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
    // And a bit out of something wider, which is the `and` against an immediate spelled again
    // under a name that says the bit rather than the register.
    ("bit_of_8", AluRi),
    ("bit_of_16", AluRi),
    ("bit_of_32", AluRi),
    ("bit_of_64", AluRi),
    ("low_8", Convert),
    ("low_16", Convert),
    ("low_32", Convert),
    // Finding a bit and counting the zeroes in front of it, which read one register and write
    // another of the same width. A form of their own rather than the conversions above, for the
    // reason `Form::Search` gives. No rule selects any of them and the reason is
    // `rucc_codegen::expand`, which builds every bit count out of arithmetic rather than out of
    // these, so what reaches one today is a program that wrote it in an `asm` template. See
    // `crate::x86_64::encode` for why the two families are four opcodes and not two.
    ("bsf_16", Search),
    ("bsf_32", Search),
    ("bsf_64", Search),
    ("bsr_16", Search),
    ("bsr_32", Search),
    ("bsr_64", Search),
    ("lzcnt_32", Search),
    ("lzcnt_64", Search),
    ("tzcnt_32", Search),
    ("tzcnt_64", Search),
    // The bytes of a register turned round, at the two widths the machine has a defined answer
    // for. No rule selects either, and the reason is the one the searches have: `rucc_codegen`
    // builds a byte reversal out of shifts and masks so that every target gets the same answer,
    // which tamnd/rucc#310 is about. Nothing here reaches one but a template that names it.
    ("bswap_32", Swap),
    ("bswap_64", Swap),
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
    // Reading and writing a truth value, which the machine does with the byte forms above for the
    // reason it widens one with the byte widenings. Separate names for the same reason as well.
    ("mov_rm_bit", Load),
    ("mov_mr_bit", Store),
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
    // And the whole of `xmm0` for the format that fills it, which is a third opcode for the same
    // reason the first two are two: the register is the same one and how much of it the value is
    // is not.
    ("ret_val_f128", RetValVec),
    // The second half of a structure that comes back in two registers, at every width a half can
    // be. The narrow ones are not a rounding of the wide one: the second eightbyte of a nine byte
    // structure is one byte, and saying so is what keeps a listing honest about how much of the
    // register the program meant.
    ("ret_val2_8", RetVal2),
    ("ret_val2_16", RetVal2),
    ("ret_val2_32", RetVal2),
    ("ret_val2_64", RetVal2),
    ("ret_val2_f32", RetVal2Vec),
    ("ret_val2_f64", RetVal2Vec),
    ("ret_val2_f128", RetVal2Vec),
    // Naming the register an argument arrived in, which is the other half of the same job and is
    // the one thing here no lowering rule reaches: where an argument is depends on its position
    // and a rule pattern cannot see one.
    ("arg_val_8", ArgVal),
    ("arg_val_16", ArgVal),
    ("arg_val_32", ArgVal),
    ("arg_val_64", ArgVal),
    ("arg_val_f32", ArgValVec),
    ("arg_val_f64", ArgValVec),
    ("arg_val_f128", ArgValVec),
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
    // answer is the one it names. Every condition is here twice over, once as itself and once as
    // its opposite, because which of the two a block gets is which of its arms is laid out next
    // and neither of them is more natural than the other.
    // The conditional move, and the test in front of it that turns the condition byte into flags.
    // One entry rather than two for the reason the comparisons above are one: what passes between
    // the halves is the flags, and the flags are not something a rule can name. The eight bit form
    // moves thirty two bits, because the machine has no conditional move narrower than sixteen and
    // the low eight bits of the answer are decided by the low eight bits of the two arms, which is
    // the same trade `imul_rr_8` makes one screen up.
    ("test_cmov_ne_8", TestCmov),
    ("test_cmov_ne_16", TestCmov),
    ("test_cmov_ne_32", TestCmov),
    ("test_cmov_ne_64", TestCmov),
    ("test_rr_8", Test),
    // The same test at the other three widths, which the layout never writes and the peephole does.
    // A comparison of a register against zero and a test of that register against itself ask the
    // machine the same question: both leave the sign, the zero and the parity of what is in the
    // register and both clear the carry and the overflow, since nothing is below zero unsigned and
    // a subtraction of zero cannot overflow. The test is the shorter of the two because it carries
    // no constant.
    ("test_rr_16", Test),
    ("test_rr_32", Test),
    ("test_rr_64", Test),
    // The comparison the test is taken back out in favour of, where the byte being tested came
    // from a comparison and nothing else wanted it. It is the comparison the byte came from with
    // the byte gone, so the flags it sets are the flags the pair already set, and the jump behind
    // it names the condition the byte was standing in for.
    ("cmp_rr_8", Cmp),
    ("cmp_rr_16", Cmp),
    ("cmp_rr_32", Cmp),
    ("cmp_rr_64", Cmp),
    ("cmp_ri_8", CmpRi),
    ("cmp_ri_16", CmpRi),
    ("cmp_ri_32", CmpRi),
    ("cmp_ri_64", CmpRi),
    // And the same against memory, which is what the layout leaves of a folded comparison
    // it took the byte off.
    ("cmp_rm_8", CmpRm),
    ("cmp_rm_16", CmpRm),
    ("cmp_rm_32", CmpRm),
    ("cmp_rm_64", CmpRm),
    ("cmp_mi_8", CmpMi),
    ("cmp_mi_16", CmpMi),
    ("cmp_mi_32", CmpMi),
    ("cmp_mi_64", CmpMi),
    // And the other half of the same pair, which is the byte with the comparison gone. There is
    // one per condition and not one per width, because what a `setcc` writes is a byte whatever
    // the comparison in front of it was comparing.
    ("set_e", Set),
    ("set_ne", Set),
    ("set_l", Set),
    ("set_le", Set),
    ("set_g", Set),
    ("set_ge", Set),
    ("set_b", Set),
    ("set_be", Set),
    ("set_a", Set),
    ("set_ae", Set),
    // The move the flags choose, with the comparison in front of it gone. Ten conditions at three
    // widths, and three rather than four because this machine has no conditional move of a byte:
    // the instruction is sixteen bits and wider and always has been. What writes one is an `asm`
    // template, and the ten are all here rather than the one zstd writes because the machine has
    // ten and a table that answered for one of them would be a description of a program.
    ("cmov_e_16", Cmov),
    ("cmov_e_32", Cmov),
    ("cmov_e_64", Cmov),
    ("cmov_ne_16", Cmov),
    ("cmov_ne_32", Cmov),
    ("cmov_ne_64", Cmov),
    ("cmov_l_16", Cmov),
    ("cmov_l_32", Cmov),
    ("cmov_l_64", Cmov),
    ("cmov_le_16", Cmov),
    ("cmov_le_32", Cmov),
    ("cmov_le_64", Cmov),
    ("cmov_g_16", Cmov),
    ("cmov_g_32", Cmov),
    ("cmov_g_64", Cmov),
    ("cmov_ge_16", Cmov),
    ("cmov_ge_32", Cmov),
    ("cmov_ge_64", Cmov),
    ("cmov_b_16", Cmov),
    ("cmov_b_32", Cmov),
    ("cmov_b_64", Cmov),
    ("cmov_be_16", Cmov),
    ("cmov_be_32", Cmov),
    ("cmov_be_64", Cmov),
    ("cmov_a_16", Cmov),
    ("cmov_a_32", Cmov),
    ("cmov_a_64", Cmov),
    ("cmov_ae_16", Cmov),
    ("cmov_ae_32", Cmov),
    ("cmov_ae_64", Cmov),
    // The ten conditions a jump can name, which are the ten a comparison can write a byte for.
    // Two of them are what a test of a byte against itself comes to, and the eight below are
    // only ever reached from a comparison the layout put the jump behind.
    ("jcc_e", Jcc),
    ("jcc_ne", Jcc),
    ("jcc_l", Jcc),
    ("jcc_le", Jcc),
    ("jcc_g", Jcc),
    ("jcc_ge", Jcc),
    ("jcc_b", Jcc),
    ("jcc_be", Jcc),
    ("jcc_a", Jcc),
    ("jcc_ae", Jcc),
    ("jmp", Jmp),
    ("jmp_away", JmpAway),
    // The same jump through a register, which is where a computed goto ends up. It is a separate
    // name for the reason `call_reg` is one: the assembler writes the register with a star in
    // front of it and the machine reads a different opcode byte, and both come from the target
    // being a register rather than a place in the program.
    ("jmp_reg", JmpReg),
    // What a copy, a prologue, an epilogue, a spill and a reload are made of, which is the other
    // set of instructions no rule reaches. The arithmetic and the address computation a frame
    // needs are already above, because a prologue taking its frame is the same instruction as a
    // subtraction the program wrote and the encoder should not have two answers for it.
    ("mov_rr_64", Move),
    ("push_64", Push),
    ("pop_64", Pop),
    ("ret", Ret),
    // The barrier, which is the whole of what an ordering costs on this machine. `crate::expand`
    // in the code generator says why one instruction covers every ordering there is.
    ("mfence", Barrier),
    // The four hints, which are one instruction with four spare bits filled in four ways. Which of
    // them a program gets is the locality it wrote: none of the data wanted afterwards is the one
    // that does not keep the line at all, and all of it wanted is the one that brings it closest.
    ("prefetch_nta", Prefetch),
    ("prefetch_t2", Prefetch),
    ("prefetch_t1", Prefetch),
    ("prefetch_t0", Prefetch),
    // The instruction a program stops on, which `__builtin_trap` asks for. Nothing selects one:
    // `rucc_codegen::lower` writes it by name where the builtin stood.
    ("ud2", Trap),
    // The landing pad, which says an indirect branch may arrive here. A prologue writes one under
    // `-fcf-protection=branch` and nothing else produces one.
    ("endbr64", Landing),
    // A byte that does nothing, which `-fpatchable-function-entry=` reserves room with so that
    // something else can be written over it while the program runs. A prologue writes them and
    // nothing else produces one.
    ("nop", Nop),
    // The hint a spin loop writes, which is the first instruction here that only an `asm`
    // statement can reach. Nothing the front end reads asks for one and no rule selects one.
    ("pause", Spin),
    // What the processor is asked about itself, which is the second instruction here that only an
    // `asm` statement can reach and the first whose operands are all implicit.
    ("cpuid", CpuId),
    // Where the next instruction starts, which is the one entry here that is not an instruction at
    // all. Only an `asm` statement writes one, and what it is written as is a directive in the
    // listing and a run of padding in the bytes rather than anything the processor does.
    ("align", Align),
    ("byte", Literal),
    // Compare and exchange, at each width the machine has one for. It is the instruction the
    // whole atomic family is built on: everything the machine has no single instruction for is a
    // loop around one of these, and `spec/10-backend.md` section 10.2 is where that is written
    // down. The `lock` in front of it is a prefix rather than part of the name, which is why the
    // name here has none.
    ("cmpxchg_8", CmpXchg),
    ("cmpxchg_16", CmpXchg),
    ("cmpxchg_32", CmpXchg),
    ("cmpxchg_64", CmpXchg),
    // The read modify writes the machine has a single instruction for. An exchange with memory is
    // indivisible without being asked, and an add has to be asked, which is why one of the two
    // carries the prefix in `crate::x86_64::text` and the other does not.
    ("xchg_8", Rmw),
    ("xchg_16", Rmw),
    ("xchg_32", Rmw),
    ("xchg_64", Rmw),
    ("xadd_8", Rmw),
    ("xadd_16", Rmw),
    ("xadd_32", Rmw),
    ("xadd_64", Rmw),
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
    // Reading and writing an eighty bit float, which is the whole of how one gets to the only unit
    // on this machine that can do arithmetic on it and back again. There is no register to register
    // form and there is nothing to add here later: the x87 has no instruction that names two of its
    // registers by number, because it names them by depth. So a `long double` is in memory whenever
    // it is not being operated on, and these two are the pair of instructions that move it.
    //
    // Neither of them looks at the value it moves. `fld` of a single or a double converts, and
    // converting is where a signalling NaN is quieted and where a format that is not a number
    // raises, but at eighty bits there is nothing to convert: the machine holds the value in the
    // format it is already in, so the load is the bits and the store is the bits back. That is why
    // a copy of a `long double` is one of each of these rather than something that has to know
    // what was in it.
    ("fld_t", PushX87),
    ("fstp_t", PopX87),
    // The conversions, which on this machine are the same two instructions reading and writing a
    // different format rather than instructions of their own. A `float`, a `double` and an integer
    // become an eighty bit float by being loaded, and an eighty bit float becomes one of them by
    // being stored, so there is nothing here that converts between two things on the stack and
    // nothing that could: the stack holds one format and only one.
    //
    // Every widening is exact, which is worth saying because it is why none of these four can
    // round. An eighty bit float has sixty four bits of significand and fifteen of exponent, so
    // every `float`, every `double` and every sixty four bit integer is a value it holds outright.
    ("fld_s", PushX87),
    ("fld_l", PushX87),
    ("fild_l", PushX87),
    ("fild_ll", PushX87),
    // The narrowings, which round, and round the way C wants: to nearest, which is what the
    // control word says unless something has changed it.
    ("fstp_s", PopX87),
    ("fstp_l", PopX87),
    // Going to an integer is the one that does not, because C cuts towards zero and this rounds
    // to nearest like everything else the unit does. So these two are written inside the group
    // `spec/10-backend.md` section 10.8 gives, with the control word changed around them.
    ("fistp_l", PopX87),
    ("fistp_ll", PopX87),
    // The control word itself, saved and put back. `fnstcw` is the one instruction here that
    // writes memory without taking anything off the stack, and `fldcw` the one that reads memory
    // without putting anything on it.
    ("fnstcw", CtrlX87),
    ("fldcw", CtrlX87),
    // The arithmetic, which is what the unit is for and is the first thing here that does anything
    // to an eighty bit value rather than moving it. Two values at the top of the stack, one answer
    // left where they were, and nothing named: both sources and the destination are depths, so
    // these are the first instructions in this table that touch nothing the allocator knows about
    // at all, not even an address.
    //
    // Two subtractions and two divisions, because a depth cannot be swapped. Which of the two
    // operands is on top is decided when the code generator pushes them, and a rule that wanted
    // the other order has nowhere to put it, so the machine gives the other order as another
    // instruction. An addition and a multiplication need one each, being what they are.
    ("fadd_p", ArithX87),
    ("fsub_p", ArithX87),
    ("fsubr_p", ArithX87),
    ("fmul_p", ArithX87),
    ("fdiv_p", ArithX87),
    ("fdivr_p", ArithX87),
    // The sign, flipped and cleared, which are the two things this machine does to one of these
    // without reading it as a number. Neither rounds and neither raises, since neither looks at
    // what it has: a negation is the top bit inverted and an absolute value is the top bit off,
    // and that is true of a number, of an infinity and of a NaN alike.
    ("fchs", UnaryX87),
    ("fabs", UnaryX87),
    // Comparing two of them, which is ten opcodes for the reason the vector comparisons are ten:
    // the machine gives four answers and a C program asks sixteen questions, eight of which are
    // one flag and two of which are a flag and the bit that says the comparison meant anything.
    // The six that are not here are these with the operands the other way round, which for the
    // x87 is a fact about which one the code generator pushed first.
    ("fucomip_set_a", CmpSetX87),
    ("fucomip_set_ae", CmpSetX87),
    ("fucomip_set_b", CmpSetX87),
    ("fucomip_set_be", CmpSetX87),
    ("fucomip_set_e", CmpSetX87),
    ("fucomip_set_ne", CmpSetX87),
    ("fucomip_set_p", CmpSetX87),
    ("fucomip_set_np", CmpSetX87),
    ("fucomip_set_e_and_np", CmpSetX87Both),
    ("fucomip_set_ne_or_p", CmpSetX87Both),
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
        assert_eq!(described, 613);
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
            // layout writes can read it, a test and a comparison set the flags, a jump goes
            // somewhere, a push
            // puts a register on the stack and leaving leaves, and a barrier is nothing but the
            // order it puts the accesses around it in. Arithmetic that leaves its answer in
            // memory is a store as far as this is concerned: what it produces is out there rather
            // than in a register. Everything else here computes something,
            // and an opcode that computes nothing and does nothing either would be an opcode
            // nothing has any reason to select.
            //
            // The x87 instructions are the only ones here that write no register and read no
            // register either. What each of them does is to the x87 stack, and the stack is not
            // somewhere a value may be told to live, so there is no operand to write down for the
            // end of a move that is not the address, and none at all for the arithmetic: both of
            // its sources and its answer are depths.
            //
            // The alignment is the one entry that computes nothing and does nothing and is still
            // worth having, because it is not an instruction. What it says is where the next one
            // starts, and something has to carry that from the template it was written in to the
            // writer that turns it into bytes.
            assert!(
                defs > 0
                    || matches!(
                        form,
                        Store
                            | AluMr
                            | RetVal
                            | RetVal2
                            | RetValVec
                            | RetVal2Vec
                            | BrCond
                            | Call
                            | Test
                            | Cmp
                            | CmpRi
                            | CmpRm
                            | CmpMi
                            | Jcc
                            | Jmp
                            | JmpAway
                            | JmpReg
                            | Push
                            | Ret
                            | StoreVec
                            | Barrier
                            | PushX87
                            | PopX87
                            | CtrlX87
                            | ArithX87
                            | UnaryX87
                            | AluMi
                            | Landing
                            | Nop
                            | Spin
                            | Prefetch
                            | Trap
                            | Align
                            | Literal
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
        assert_eq!(rem[0].role, Role::EarlyDef, "the divisor may not be where the remainder goes");
        assert_eq!(rem[1].constraint, Constraint::Fixed(RAX));
        // Both answers are written early here and one of them is not, and the difference is which
        // register the division also reads. `rdx` is filled by the sign extension before the
        // divisor is read whichever answer is being kept, so both forms have to say so about it.
        // `rax` holds the dividend, and an operand that reads a register claims it where the
        // operands are read, which is the same point an early definition would claim it at, so the
        // quotient has nothing left to say.
        assert_eq!(rem[1].role, Role::EarlyDef);
        assert_eq!(quo[0].role, Role::Def);
        assert_eq!(quo[2].role, Role::Use);
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

    /// The second register, which only one of the two conventions has. Written down here the way
    /// the first one is, and held against the convention the same way, so that a convention which
    /// grew a different second register would fail here rather than compile a function whose
    /// caller reads the wrong half of a structure.
    #[test]
    fn the_second_half_of_a_structure_comes_back_where_sysv_says_it_does() {
        assert_eq!(RetVal2.operands()[0].constraint, Constraint::Fixed(RDX));
        assert_eq!(SYSV.int_returns.get(1), Some(&RDX));
        assert_eq!(RetVal2Vec.operands()[0].constraint, Constraint::Fixed(xmm(1)));
        assert_eq!(SYSV.sse_returns.get(1), Some(&xmm(1)));
        assert_eq!(RetVal2Vec.operands()[0].class, XMM);

        // Windows returns a structure of more than eight bytes through a hidden pointer instead,
        // so it has no second register and nothing here should ever select one of these for it.
        assert_eq!(WIN64.int_returns.get(1), None);
        assert_eq!(WIN64.sse_returns.get(1), None);
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

    /// The two instructions that reach the x87 stack, and the two things about them that are not
    /// true of anything else here.
    ///
    /// They carry an address and no operand of their own, which is what says the end of the move
    /// that is not memory is not a register the allocator picked. And they are the only pair here
    /// where one is the only way into a place and the other is the only way out of it, which is
    /// what the discipline in `spec/10-backend.md` section 10.8 is written against.
    #[test]
    fn every_instruction_that_reaches_the_x87_stack_names_only_an_address() {
        assert_eq!(form("fld_t"), Some(PushX87));
        assert_eq!(form("fstp_t"), Some(PopX87));
        assert_eq!(form("fnstcw"), Some(CtrlX87));
        for shape in [PushX87, PopX87, CtrlX87] {
            assert!(shape.operands().is_empty(), "an x87 move names a register it did not pick");
            assert!(shape.takes_mem(), "an x87 move goes to or comes from memory");
            assert!(!shape.takes_imm());
        }
        // The arithmetic goes one further and names nothing at all, not even an address. Both of
        // its sources and its answer are depths on the stack, so an instruction of one of these
        // shapes touches nothing the allocator has any say over.
        assert_eq!(form("fadd_p"), Some(ArithX87));
        assert_eq!(form("fchs"), Some(UnaryX87));
        for shape in [ArithX87, UnaryX87] {
            assert!(shape.operands().is_empty(), "x87 arithmetic names a register it did not pick");
            assert!(!shape.takes_mem(), "x87 arithmetic works on what is already on the stack");
            assert!(!shape.takes_imm());
        }
        // The comparison is the exception, and the one operand it has is the byte it sets, which
        // is in the other file because a truth value is a byte and the x87 holds no bytes.
        assert_eq!(form("fucomip_set_e"), Some(CmpSetX87));
        assert_eq!(form("fucomip_set_e_and_np"), Some(CmpSetX87Both));
        for shape in [CmpSetX87, CmpSetX87Both] {
            assert!(shape.operands().iter().all(|operand| operand.role.is_def()));
            assert!(shape.operands().iter().all(|operand| operand.class == GPR));
            assert!(!shape.takes_mem());
        }
        // Nothing else here has an empty operand list and an address, and the two halves of that
        // are worth saying separately. A call has an empty list and no address, and almost every
        // instruction that carries an address has an operand for the end of it that is a register.
        // Arithmetic against a constant in memory is the exception the other way round, and so is
        // a comparison of memory against a constant once the byte it set has gone: each has an
        // address and an empty list, because everything it reads is either a register the
        // addressing mode brought or the constant on the instruction.
        for &(name, shape) in INSTS {
            assert!(
                shape.operands().is_empty()
                    == matches!(
                        shape,
                        Call | Jcc
                            | Jmp
                            | JmpAway
                            | Ret
                            | Barrier
                            | AluMi
                            | CmpMi
                            | Landing
                            | Nop
                            | Spin
                            | Prefetch
                            | Trap
                            | Align
                            | Literal
                    )
                    || matches!(shape, PushX87 | PopX87 | CtrlX87 | ArithX87 | UnaryX87),
                "{name} has an empty operand list and is not one of the ones that should"
            );
        }
    }

    /// The x87 instructions come in a shape that has to stay balanced, so this counts them.
    ///
    /// One way onto the stack per format a value can be read from, one way off it per format a
    /// value can be written to, and the control word pair that is neither. A push with no matching
    /// pop, or the other way round, would be a format this target can convert in one direction and
    /// not the other, which is the mistake that reaches a program as a `long double` that cannot be
    /// got back out again.
    #[test]
    fn the_ways_onto_the_x87_stack_and_off_it_are_the_same_in_number() {
        let count = |wanted| INSTS.iter().filter(|&&(_, shape)| shape == wanted).count();
        assert_eq!(count(PushX87), 5, "the extended format, two floats and two integers");
        assert_eq!(count(PopX87), 5, "the same five the other way");
        assert_eq!(count(CtrlX87), 2, "the control word saved and put back");
        // The arithmetic is not balanced the same way, because what it is counted against is C
        // rather than the stack. Four operations, two of which have an order that cannot be
        // swapped and so come in two.
        assert_eq!(count(ArithX87), 6, "an add, a multiply and a subtract and a divide each way");
        assert_eq!(count(UnaryX87), 2, "the sign flipped and the sign cleared");
        assert_eq!(count(CmpSetX87) + count(CmpSetX87Both), 10, "the ten a float comparison has");
    }

    #[test]
    fn an_address_constructor_is_not_an_instruction() {
        assert_eq!(address("amode_base_index_scale"), Some(Address::BaseIndexScale));
        assert_eq!(address("amode_base_offset"), Some(Address::BaseOffset));
        assert_eq!(address("amode_base"), Some(Address::Base));
        assert_eq!(address("lea_64"), None);
        assert_eq!(form("amode_index_scale"), None);
    }

    /// The block layout reads the names out of [`crate::x86_64::BRANCH`] and writes them
    /// into the machine IR without ever asking what any of them is, so a name there that is not
    /// an opcode here would come out as an instruction nothing further along could describe. The
    /// forms are pinned too, because the layout writes one shape each and a name that turned out
    /// to be an ordinary two-address instruction would be written with no operands at all. The
    /// indirect jump is the one it does not write and only reads, and it is pinned here anyway,
    /// since a name that was not this form would be a block the layout thought had a branch in it
    /// and did not.
    #[test]
    fn every_instruction_the_block_layout_writes_is_described_here() {
        use crate::x86_64::BRANCH;

        assert_eq!(BRANCH.prefix, FRAME.prefix, "one target, one prefix");
        assert_eq!(form(BRANCH.cond), Some(BrCond));
        assert_eq!(form(BRANCH.test), Some(Test));
        assert_eq!(form(BRANCH.if_true), Some(Jcc));
        assert_eq!(form(BRANCH.if_false), Some(Jcc));
        assert_eq!(form(BRANCH.jump), Some(Jmp));
        assert_eq!(form(BRANCH.indirect), Some(JmpReg));
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
