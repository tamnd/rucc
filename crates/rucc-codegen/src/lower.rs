//! The selector: an IR function becomes a machine IR function.
//!
//! Design: `spec/10-backend.md` sections 10.2 and 10.3.
//!
//! What the matcher in [`crate::select`] does is answer one question about one term. What this
//! does is ask it: walk a function, decide which terms are worth asking about, and build machine
//! instructions out of what comes back. Nothing here decides what an IR term lowers to. That is
//! in `rules/x86-64.rules` and it is proved before it is used, which is the whole point of the
//! arrangement and the reason this file is short.
//!
//! # What it does with an instruction
//!
//! It tries the ways the instruction can be shown to the matcher, in order, and takes the first
//! that a rule fires on. [`crate::term`] is what a way of showing one is, and the order is the
//! most specific first: an operand that is a constant is offered as a constant before it is
//! offered as a register, and an operand computed by an instruction of its own is offered as
//! that instruction before it is offered as a register. A rule that wants an immediate too wide
//! for the machine has a guard that turns it down, and the search carries on to the way of
//! showing it that puts the constant in a register, which is the right answer and is one nobody
//! had to write down.
//!
//! A constant is not lowered where it is written. It is materialized where a register for it is
//! first wanted, which is what keeps a constant that every use folded into an immediate from
//! leaving a dead instruction behind, and it also gives the value the shortest live range it
//! could have. The instruction that materializes it comes from the rule set like everything else.
//!
//! # What it does not do yet
//!
//! Everything is in the general purpose registers, because every rule in the set is about an
//! integer, so a call that passes a `double` and a function that returns one are both reported
//! rather than lowered. So is an argument that travels on the stack, on either side of a call,
//! and so is a call through an address rather than to a name.
//!
//! # A call
//!
//! Not a rule, because a rule pattern sees one term and what a call's operands are is whatever
//! the signature made them. [`crate::abi`] builds one instead, out of the same description of the
//! convention the arguments come from: the values it passes are reads constrained to the
//! registers the convention places them in, what comes back is a write constrained to the
//! register it comes back in, and every other register the callee is free to destroy is a write
//! of that register and nothing else, which is all the allocator needs to keep a value out of it.
//!
//! What that costs the frame is an argument area, and nothing after selection could work out how
//! big, so the size of the widest call is given back with the function. A function that makes no
//! call at all is a leaf, and a leaf is the function that may use the red zone.
//!
//! # Where a block goes
//!
//! On the block, which is what machine IR does with an edge and is why the branches need no more
//! rule language than the arithmetic did. A rule never names a block, so an unconditional jump
//! has no rule at all and a conditional branch has one that is about its condition and nothing
//! else. The arms are copied across after the block is filled, arguments and all, because an
//! argument that is a constant is materialized where a register for it is first wanted and the
//! end of the block is where an edge wants it.
//!
//! What this leaves behind is a function whose blocks are in the order the IR held them and whose
//! branches are still branches on a register. Turning one into a `test` and a `jcc` is the block
//! layout's, since which of the two arms falls through is the layout's answer, and [`crate::split`]
//! has to run before allocation so that every edge carrying a value has somewhere to put it.
//!
//! A store and a return are the two things here that write no register. A store is emitted like
//! everything else and the only difference is that there is no result to put anywhere, so the
//! operands the target describes are all reads. A return is the same, and what it is for is its
//! one operand: the target constrains it to the register the caller reads the value out of, and
//! the allocator is what gets it there. The instruction that leaves is not chosen here at all,
//! because the epilogue has to give the frame back first and [`crate::finish`] writes that after
//! allocation, so a return of nothing is lowered to nothing.
//!
//! The entry block is the one block whose parameters are not block parameters here. They are the
//! function's arguments, they are already somewhere when it starts, and [`crate::abi`] is what
//! says where. An argument that arrives on the stack is reported rather than read, because where
//! the stack put it is a distance into a frame and no frame exists until after allocation.
//!
//! Blocks are walked in the order the function holds them and a value is expected to be defined
//! before it is used, which is true of the IR this is given because every pass before it keeps
//! definitions ahead of uses.

use std::fmt;

use rucc_base::Interner;
use rucc_diag::Span;
use rucc_ir::{
    Abi, Block, Def, Extra, FloatPred, Func, Inst, Linkage, MemOrder, Opcode, Param, Type, Value,
};
use rucc_mir as mir;
use rucc_target::x86_64;
use rucc_target::{CallRegs, Constraint, RegClass};

use crate::abi::{self, Missing, Refused};
use crate::coverage::Fired;
use crate::frame::{Layout, Local};
use crate::select::{Match, Piece, Rule, Table};
use crate::term::{MAX_ARGS, PLAIN, Plan, Shown, Term, Terms};
use crate::varargs;

/// The prefix a rule file puts in front of a machine term, which says which target it belongs
/// to and is not part of the opcode.
pub(crate) const PREFIX: &str = "x64.";

/// How wide an address is on this target, which is the width a cast between a pointer and an
/// integer has to be at for the cast to be nothing.
const ADDRESS_BITS: u32 = 64;

/// How many bytes a `long double` takes in memory, and what it is aligned to, which are the same
/// number and are both more than the ten bytes that mean anything.
///
/// The psABI's answer rather than a choice here. `sizeof (long double)` is sixteen on this
/// machine, so an array of them is laid out this way whatever a slot holding one does, and a slot
/// that agreed with the array is one fewer thing to get wrong.
const X87_BYTES: u32 = 16;

/// How many values the x87 stack holds at once.
///
/// Eight, which is the machine's number rather than a choice here, and it matters in one place:
/// the parameters of a block are copied through the stack so that they all move at once, and a
/// block with more of them than this has nowhere to put the ninth.
const X87_DEPTH: usize = 8;

/// How many bytes a value passes through on its way between a register and the x87 stack.
///
/// Eight, because the widest thing that crosses is a `double` or a sixty four bit integer, and
/// nothing crosses at eighty bits: a value that wide is already in the frame and the stack reaches
/// it where it is.
const X87_CROSSING: u32 = 8;

/// Where the rounding field of the x87 control word is and what it has to be set to for the unit
/// to cut towards zero, which is the one rounding C asks for that the unit does not do by default.
///
/// Both bits on is truncate. The field is ORed into the word that was already there rather than
/// written over it, so the precision control and the exception masks somebody else set stay set.
const X87_TRUNCATE: i64 = 0x0c00;

/// Whether a type is the one this machine has no register for.
///
/// Only the eighty bit float is, and that is a fact about x86-64 rather than about floats: every
/// other scalar the front end produces is in a general purpose register or a vector one, and this
/// one is on the x87 stack while it is being worked on and in memory the rest of the time. So it
/// has no place in [`Lowering::class_of`] and no name in [`crate::term`], and every instruction
/// that touches one is written out by hand in this file.
fn on_x87(ty: Type) -> bool {
    ty.is_scalar() && ty.is_float() && ty.bits() == 80
}

/// Why a function could not be lowered.
///
/// One reason and then nothing. A function with no rule for something in it is a function this
/// cannot finish, and the second thing it could not lower is not news.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// An instruction no rule fires on.
    Inst {
        /// The instruction that stopped it.
        inst: Inst,
        /// What the rule file would call it, or nothing if the rule language has no name for it
        /// at all, which is what an instruction at a width nothing is written about looks like.
        term: Option<&'static str>,
        /// The opcode, which is what gets named when the rule language has no word for it.
        ///
        /// An opcode the rule language has no word for is exactly the opcode no rule lowers, so
        /// without this the message would be empty in every case where somebody needs it.
        opcode: Opcode,
        /// What it produces, or nothing for an instruction that is only an effect.
        ty: Option<Type>,
    },
    /// A parameter that does not arrive somewhere this can bring it in from.
    ///
    /// Not an instruction, which is why it is a separate arm: it is a fact about the signature
    /// and there is nothing in the body of the function to point at.
    Argument {
        /// Its position in the signature.
        index: usize,
        /// What is wrong with where it arrives.
        missing: Missing,
    },
    /// A call that passes or gives back a value this cannot put where the convention wants it.
    Call {
        /// The call.
        inst: Inst,
        /// Which value, and what is wrong with where it travels.
        refused: Refused,
    },
    /// A `return` this cannot put where the convention wants it.
    ///
    /// A separate arm from [`Unsupported::Inst`] because it is not an instruction no rule fires
    /// on. A return of more than one value is built from the convention rather than matched, the
    /// same way a call is, so what goes wrong with one is what goes wrong with a call and not the
    /// absence of a rule.
    Returned {
        /// The `return`.
        inst: Inst,
        /// What is wrong with where one of the values travels.
        missing: Missing,
    },
    /// A stack slot whose size is not known until the function runs, which is what a variable
    /// length array is.
    ///
    /// Not an instruction no rule covers. Growing the stack where the declaration stands is
    /// arithmetic on the stack pointer, and everything else in the frame then has to be reached
    /// through a frame pointer instead, and neither of those is a term a rule could be written
    /// about or a thing the frame here knows how to lay out.
    Dynamic {
        /// The `alloca`.
        inst: Inst,
    },
    /// More parameters of a type that travels on the x87 stack than the stack is deep.
    ///
    /// Not an instruction either, for the reason a function's parameter is not one: it is a fact
    /// about the block and there is nothing in the block to point at. What crosses an edge for one
    /// of these is the address of where the value is, and the block copies the bytes into a slot
    /// of its own, all of them through the stack at once so that a block carrying two of them
    /// swapped is copied in an order that is right. Eight is as many as the stack holds, and a
    /// ninth would have to be copied before or after the rest, which is the order that could be
    /// wrong.
    Phi {
        /// Which block it arrives at.
        block: Block,
        /// How many of them arrive there, which is the whole of what is wrong.
        count: usize,
        /// What they are.
        ty: Type,
    },
}

impl Unsupported {
    /// The instruction it is about, or nothing for the one arm that is about a signature.
    ///
    /// What a caller wants this for is the span. The function knows where every instruction in
    /// it came from, so a caller holding both can point a message at the line somebody wrote
    /// rather than at the file as a whole, and nothing here has to carry a span of its own.
    pub fn inst(&self) -> Option<Inst> {
        match *self {
            Unsupported::Inst { inst, .. }
            | Unsupported::Call { inst, .. }
            | Unsupported::Returned { inst, .. }
            | Unsupported::Dynamic { inst, .. } => Some(inst),
            Unsupported::Argument { .. } | Unsupported::Phi { .. } => None,
        }
    }
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Unsupported::Inst { term: Some(term), .. } => write!(f, "no rule lowers `{term}`"),
            Unsupported::Inst { term: None, opcode, ty: Some(ty), .. } => {
                write!(f, "no rule lowers a `{opcode}` producing a `{ty}`")
            }
            Unsupported::Inst { term: None, opcode, ty: None, .. } => {
                write!(f, "no rule lowers a `{opcode}`")
            }
            Unsupported::Argument { index, missing } => {
                write!(f, "parameter {index} {}", missing.why())
            }
            Unsupported::Call { refused: Refused { argument: Some(index), missing }, .. } => {
                write!(f, "argument {index} of this call {}", missing.why())
            }
            Unsupported::Call { refused: Refused { argument: None, missing }, .. } => {
                write!(f, "what this call gives back {}", missing.why())
            }
            Unsupported::Returned { missing, .. } => {
                write!(f, "what this function gives back {}", missing.why())
            }
            Unsupported::Dynamic { .. } => {
                f.write_str("nothing here grows the stack for a variable length array")
            }
            Unsupported::Phi { block, count, ty } => {
                let block = block.index();
                write!(
                    f,
                    "block{block} takes {count} parameters of type `{ty}` and only {X87_DEPTH} can cross an edge at once"
                )
            }
        }
    }
}

impl std::error::Error for Unsupported {}

/// A lowered function, and what the frame needs that the machine IR does not hold.
#[derive(Debug)]
pub struct Lowered {
    /// The function, in machine instructions.
    pub func: mir::Func,
    /// What it wants its stack to look like, which is separate from the function so that the two
    /// can be read and written at the same time.
    pub stack: Stack,
    /// Which rules of the table lowered it, which is what `-Zrule-coverage` asks for and what
    /// `crate::coverage` writes down.
    pub fired: Fired,
}

/// What a function's stack has to hold, as far as selection is able to say.
///
/// All of it is answered here because selection is where a call is built and where an `alloca`
/// is read, and nothing after it could tell what either of them needed.
#[derive(Debug, Default)]
pub struct Stack {
    /// How many bytes the widest call in the function needs below the stack pointer for the
    /// arguments it passes there, or `None` for a function that makes no call at all.
    ///
    /// `None` is a leaf, which is the function that may use the red zone and the one whose stack
    /// pointer does not have to be left aligned for anybody.
    pub calls: Option<u32>,
    /// The memory the function asked for itself, one entry for every `alloca` in it, in the order
    /// the walk reached them.
    pub locals: Vec<Local>,
    /// Which instruction computes the address of which of those locals.
    ///
    /// An address in the frame is a distance from the stack pointer, and there is no frame until
    /// after allocation, so the instruction is written here with nothing in its displacement and
    /// [`crate::finish`] writes the number in once [`crate::frame::Frame`] knows it.
    pub addresses: Vec<(mir::Inst, usize)>,
    /// Which instruction reads which of the arguments the caller passed on the stack, as how far up
    /// the caller's argument area it reads.
    ///
    /// Waiting on [`crate::finish`] for the same reason the addresses above are, and on one thing
    /// more: where the caller's argument area is from inside this function depends on whether the
    /// prologue had to force the stack pointer's alignment, so which register the load reads
    /// through is not settled here either.
    pub arguments: Vec<(mir::Inst, u32)>,
}

impl Stack {
    /// The layout given, with the three fields only the lowering knows the answer to filled in.
    ///
    /// Everything else in a layout comes from the flags the function is compiled under or from the
    /// allocation, so this takes one and returns it rather than building one.
    #[must_use]
    pub fn layout<'a>(&'a self, base: Layout<'a>) -> Layout<'a> {
        Layout {
            leaf: self.calls.is_none(),
            outgoing: self.calls.unwrap_or(0),
            locals: &self.locals,
            ..base
        }
    }
}

/// The x86-64 machine IR for that function.
///
/// # Errors
///
/// The first instruction no rule fires on, which today is anything at a width the rule set is not
/// written at, a parameter that does not arrive in a register this can read, or a call that
/// passes something this cannot put where the convention wants it.
pub fn func(
    source: &Func,
    names: &mut Interner,
    conv: &'static CallRegs,
) -> Result<Lowered, Unsupported> {
    Lowering::new(source, names, conv).run()
}

/// One function being lowered.
struct Lowering<'a> {
    source: &'a Func,
    names: &'a mut Interner,
    out: mir::Func,
    /// The machine register each IR value is in, once it has one.
    regs: Vec<Option<mir::Reg>>,
    /// For a constant that has been written into a register, the block it was written into,
    /// which is the only block that register is any good in.
    written: Vec<Option<mir::Block>>,
    /// How many times each IR value is read, which is what says whether an instruction may be
    /// folded into the one that reads it.
    uses: Vec<u32>,
    /// The block being filled.
    at: Option<mir::Block>,
    /// The machine IR block each IR block became.
    blocks: Vec<Option<mir::Block>>,
    /// The class an address is in, which is the general purpose one and is not a question: every
    /// register an addressing mode names holds part of an address, and there is no machine here
    /// that computes an address anywhere but in this file. Which class a *value* is in is
    /// [`Lowering::class_of`], and it is a question, because a float is in the other one.
    gpr: RegClass,
    /// Where the convention this function is compiled for puts things, which is read for the
    /// arguments and for the calls.
    conv: &'static CallRegs,
    /// What the function wants its stack to look like, filled in as the walk finds out.
    stack: Stack,
    /// What a `va_start` in this function has to write, or nothing for a function that takes no
    /// arguments its signature does not name.
    ///
    /// Worked out once, when the entry block binds the parameters, because every number in it is
    /// about where those parameters left the walk over the argument registers and there is nowhere
    /// else that knows.
    varargs: Option<Varargs>,
    /// Which of the function's stack objects each eighty bit value lives in, once it has asked
    /// for one.
    ///
    /// One slot per value and it is never given back, which is what makes an eighty bit value
    /// behave like every other one: it is written once and read wherever it is read, and no two
    /// of them share a slot the way two of them would share a register. What is in a register is
    /// the address, and that is worked out again at every use rather than kept, so nothing here
    /// holds a general purpose register open across a whole function.
    slots: Vec<Option<usize>>,
    /// The eight bytes a value passes through between a register and the x87 stack, once
    /// something has wanted them.
    ///
    /// One for the whole function, because every group that uses it is a handful of instructions
    /// with nothing in between: the bytes are written, read straight back and never looked at
    /// again, so a second slot would be a second slot holding the same nothing.
    crossing: Option<usize>,
    /// The four bytes the control word is saved in and the changed copy written to, once
    /// something has wanted them.
    ///
    /// One for the whole function for the reason above, and four rather than two because it is
    /// two words: the one the unit had and the one with the rounding field turned to truncate.
    control: Option<usize>,
    /// Which rules have fired so far.
    fired: Fired,
}

/// What a `va_start` in a variadic function writes into the list it is given.
///
/// Three of the four are settled here and the fourth is not a number at all yet: where the save
/// area is and where the caller's argument area is are both distances into a frame that does not
/// exist until after allocation, so both are `lea` instructions [`crate::finish`] fills in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Varargs {
    /// Which of the function's stack objects is the register save area.
    save: usize,
    /// How far up the caller's argument area the first argument the signature does not name is,
    /// which is the whole of that area the named ones did not take.
    incoming: u32,
    /// What `gp_offset` starts at, which is past the general purpose registers the named arguments
    /// took.
    integers: u32,
    /// What `fp_offset` starts at, which is past the vector ones.
    floats: u32,
}

/// How far a function's name reaches, narrowed from the linkage the IR gave it.
///
/// The IR has five and an object file says three, and the two the linker cannot tell apart are
/// the two weak ones: which of them a symbol had is a fact the optimizer reads and the linker has
/// no way to record. A function is never `Common`, since that is what a tentative definition of an
/// object is and there is no tentative definition of a function, and it is written here rather
/// than left out so that a linkage added later has to come past this.
const fn binding(linkage: Linkage) -> mir::Binding {
    match linkage {
        Linkage::Internal => mir::Binding::Local,
        Linkage::Weak | Linkage::LinkOnce => mir::Binding::Weak,
        Linkage::External | Linkage::Common => mir::Binding::Global,
    }
}

impl<'a> Lowering<'a> {
    fn new(source: &'a Func, names: &'a mut Interner, conv: &'static CallRegs) -> Self {
        let counts = source.counts();
        let name = source.name;
        let mut uses = vec![0; counts.values];
        for block in source.blocks() {
            for inst in source.insts(block) {
                for &arg in &source[source[inst].args] {
                    uses[arg.index()] += 1;
                }
                for call in source.successors(inst) {
                    for &arg in &source[call.args] {
                        uses[arg.index()] += 1;
                    }
                }
            }
        }
        let mut out = mir::Func::new(name);
        out.align = source.align;
        out.binding = binding(source.linkage);
        Self {
            source,
            names,
            out,
            regs: vec![None; counts.values],
            written: vec![None; counts.values],
            blocks: vec![None; counts.blocks],
            uses,
            at: None,
            gpr: x86_64::GPR,
            conv,
            stack: Stack::default(),
            varargs: None,
            slots: vec![None; counts.values],
            crossing: None,
            control: None,
            fired: Fired::new(),
        }
    }

    fn run(mut self) -> Result<Lowered, Unsupported> {
        // Every block before any of them is filled, because a block that jumps forward has to
        // name the block it jumps to and a machine IR block is named by a handle rather than by
        // the IR block it came from.
        for block in self.source.blocks() {
            let out = self.out.create_block();
            self.blocks[block.index()] = Some(out);
        }
        for block in self.source.blocks() {
            self.block(block)?;
        }
        Ok(Lowered { func: self.out, stack: self.stack, fired: self.fired })
    }

    /// One block: its parameters, then every instruction in it that is not folded into another.
    fn block(&mut self, block: Block) -> Result<(), Unsupported> {
        let out = self.out_block(block);
        self.at = Some(out);
        if self.source.entry() == Some(block) {
            self.arrive(block, out)?;
        } else {
            let mut arriving = Vec::new();
            for &param in &self.source[block].params {
                // A value with no register to arrive in, which the class would not say, since
                // `class_of` puts one of these in the general purpose file on purpose and what it
                // means by that is that nothing there can hold it. What crosses the edge for one
                // of those is the address of where the value already is, so the parameter is a
                // pointer here and the bytes it points at are copied below.
                let ty = self.source[param].ty;
                let reg = self.out.append_param(out, self.class_of(ty));
                self.regs[param.index()] = Some(reg);
                if on_x87(ty) {
                    arriving.push((param, reg));
                }
            }
            self.settle(block, &arriving)?;
        }

        // What each instruction matched, and which instructions were folded into another. The
        // instruction that is folded comes before the one that folds it, so the decision has to
        // be made for the whole block before any of it is written, and it is made backwards: an
        // instruction that has been folded into a later one does not get to fold anything into
        // itself, because the rule that took it only reached one level down.
        let insts: Vec<Inst> = self.source.insts(block).collect();
        let mut found: Vec<Option<Match<Term>>> = (0..insts.len()).map(|_| None).collect();
        let mut folded: Vec<Inst> = Vec::new();
        for (index, &inst) in insts.iter().enumerate().rev() {
            if folded.contains(&inst) {
                continue;
            }
            if let Some((plan, matched)) = self.select(inst) {
                folded.extend(self.folds(inst, plan));
                found[index] = Some(matched);
            }
        }

        for (&inst, matched) in insts.iter().zip(found) {
            if folded.contains(&inst) || self.writes_nothing(inst) {
                continue;
            }
            // A call is built from the convention rather than matched, which is why it is the one
            // opcode looked at by name here. Through an address it is a different instruction and
            // the same convention, so the two arrive at the same place and differ in one line of
            // it.
            match self.source[inst].opcode {
                Opcode::Call | Opcode::CallIndirect => {
                    self.called(inst)?;
                    continue;
                }
                // Built from the frame rather than matched, for the same shape of reason a call
                // is built from the convention: what a rule replaces a term with is instructions,
                // and what an `alloca` needs first is bytes, which the rule language has no way
                // to ask for.
                Opcode::Alloca => {
                    self.reserve(inst)?;
                    continue;
                }
                // The address of a name, built here for the same reason an `alloca` is: what a
                // rule replaces a term with is instructions over values, and the operand of this
                // one is a symbol, which is a thing the rule language has no way to bind and the
                // solver has no way to say anything about. There is nothing in `lea sym(%rip)` a
                // proof over bitvectors could discharge, because what makes it the right answer
                // is the relocation and what the linker does with it.
                Opcode::GlobalAddr => {
                    self.address_of(inst)?;
                    continue;
                }
                // Built from the frame for the reason an `alloca` is, and from the convention for
                // the reason a call is: three of the four fields it writes are distances that do
                // not exist until the frame does, and the fourth is where the walk over the
                // argument registers stopped. A function that is not variadic has no such walk to
                // report, so it has nothing here and is refused below, which is the right answer
                // for a `va_start` in one.
                Opcode::VaStart if self.varargs.is_some() => {
                    self.va_start(inst)?;
                    continue;
                }
                // A return of more than one value, which is a structure small enough to come
                // back in a pair of registers. Built from the convention for the reason a call
                // is: which register each half goes in depends on the halves in front of it,
                // because the two register files are walked separately, and a pattern over a term
                // cannot see them. A return of one value is a term with a name and a rule, and it
                // stays one.
                //
                // A return of none in a function whose answer went through memory is here too,
                // and for a different reason: what it gives back is not written in the IR at all.
                // The convention says the address the caller handed over comes back, and only the
                // signature says this function was handed one.
                //
                // And a return of one eighty bit value, for a third reason: what a rule would
                // write is an instruction leaving the value in a register, and this one is left on
                // the x87 stack instead. A rule could not name that stack any more than any other
                // rule about this type could.
                Opcode::Return
                    if self.source[self.source[inst].args].len() > 1
                        || self.sret().is_some()
                        || self.gives_back_x87(inst) =>
                {
                    self.returned(inst)?;
                    continue;
                }
                // A cast between a pointer and an integer of the same width, which on this
                // machine is every one the front end writes. No instruction at all, so no rule
                // could name one.
                Opcode::PtrToInt | Opcode::IntToPtr => {
                    self.rename(inst)?;
                    continue;
                }
                // A barrier, which is one instruction or none depending on the ordering. Written
                // by name because there is nothing about it a rule could be proved against, the
                // way there is nothing to prove about the address of a symbol.
                Opcode::Fence => {
                    self.barrier(inst)?;
                    continue;
                }
                // Anything at all with an eighty bit float in it, which is the one arm here
                // chosen by a type rather than by an opcode, because what makes these different
                // is not what they do but where the value is. A `long double` has no register,
                // so it has no name in `crate::term` and no rule could bind one: every one of
                // these is a group of instructions over a frame slot, written out below.
                //
                // Last of the arms, so that a call and a return with one of these in them reach
                // the convention first and are refused by it, which is the truer answer: what is
                // wrong there is where the value has to travel and not that nothing can compute
                // it.
                _ if self.touches_x87(inst) => {
                    self.x87(inst)?;
                    continue;
                }
                _ => {}
            }
            let matched = matched.ok_or_else(|| self.unsupported(inst))?;
            self.emit(inst, &matched)?;
            // After it is built rather than when it matched, so that what is recorded is the rules
            // this function was lowered by and not the rules something was tried with.
            self.fired.mark(matched.rule);
        }
        self.edges(block, out)
    }

    /// One call, which is built from the convention rather than matched against the table for the
    /// same reason the arguments of the function itself are.
    ///
    /// The arguments are read before the call is built, which is what materializes a constant
    /// argument into a register, since no call passes an immediate.
    ///
    /// A call to a name and a call through an address are both here, and what tells them apart is
    /// the opcode rather than whether a callee was recorded, which is the same thing the verifier
    /// reads. Through an address the first operand is the address and the arguments are the ones
    /// behind it, and everything after that is the same: where each argument goes, where the value
    /// comes back and which registers are gone across it are the convention's answers and the
    /// convention does not ask what is being called.
    fn called(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let data = &self.source[inst];
        let Extra::Call(info) = data.extra else { return Err(self.unsupported(inst)) };
        let info = self.source[info];
        let indirect = data.opcode == Opcode::CallIndirect;

        let values: Vec<Value> = self.source[data.args].to_vec();
        let callee = if indirect {
            let &address = values.first().ok_or_else(|| self.unsupported(inst))?;
            abi::Callee::Through(self.reg_of(address)?)
        } else {
            abi::Callee::Named(info.callee.ok_or_else(|| self.unsupported(inst))?)
        };

        // What the ABI asks of each argument, read out before any of them is, because reading one
        // borrows the function this is a table in. The ones the signature names are the signature's
        // answer and the ones behind them are the call's, which is where a structure passed to a
        // variadic callee by value says that its bytes travel: there is no parameter to say it on.
        let signature = &self.source[info.signature];
        let variadic = signature.variadic;
        let named: Vec<Abi> = signature.params.iter().map(|param| param.abi).collect();
        let beyond: Vec<Abi> = self.source[info.varargs].to_vec();
        // Every value that comes back and not only the first. A structure small enough to travel
        // in registers comes back in up to two of them, and which register each half is in is the
        // convention's answer, which is why the whole list goes to the same place the arguments do
        // rather than to a rule.
        let returns: Vec<Type> = signature.return_types().collect();

        let mut args = Vec::with_capacity(values.len());
        for (index, value) in values.into_iter().skip(usize::from(indirect)).enumerate() {
            let abi = named.get(index).or_else(|| beyond.get(index - named.len()));
            let abi = abi.copied().unwrap_or_default();
            let ty = self.source[value].ty;
            // What travels for an eighty bit value is its bytes, so what the call is handed is
            // where they are rather than a register they are in, and there is no register they
            // could be in. Everything else about it is a sixteen byte object passed by value and
            // is built by the same code.
            let reg =
                if abi::on_the_stack(ty) { self.x87_slot(value) } else { self.reg_of(value)? };
            args.push(abi::Passing { ty, reg, abi });
        }
        let block = self.at.expect("a block is being filled");
        let what = abi::Calling { callee, args: &args, returns: &returns, variadic };
        let made = abi::call(&mut self.out, block, &what, self.conv, self.names)
            .map_err(|refused| Unsupported::Call { inst, refused })?;
        let calls = &mut self.stack.calls;
        *calls = Some(calls.unwrap_or(0).max(made.outgoing));
        // An eighty bit value came back on the x87 stack, and the one thing that has to happen
        // before anything else touches that stack is taking it off. So the `fstp` goes here, in
        // front of everything the block does next, and after it the value is in its slot and is
        // read the way every other one is.
        let results: Vec<Value> = self.source[inst].results().collect();
        if let [result] = results[..] {
            if abi::on_the_stack(self.source[result].ty) {
                let span = self.source.span(inst);
                let into = self.x87_slot(result);
                let into = self.through(into);
                self.x87_at("fstp_t", span, into);
                return Ok(());
            }
        }
        for (result, &reg) in results.into_iter().zip(&made.results) {
            self.regs[result.index()] = Some(reg);
        }
        Ok(())
    }

    /// The pointer a function returning through memory was handed, or nothing in a function that
    /// was not.
    ///
    /// It is the first parameter and the signature is what says so, since in the IR it is an
    /// ordinary pointer and reads like one everywhere in the body. A function with a signature
    /// like that and no entry block has nothing to give back and no body to give it back from.
    fn sret(&self) -> Option<Value> {
        let first = self.source.signature().params.first()?;
        if !matches!(first.abi, Abi::Sret { .. }) {
            return None;
        }
        self.source[self.source.entry()?].params.first().copied()
    }

    /// One `return` the convention has to write, as the place each value has to be in by the end.
    ///
    /// One pseudo per value, each a read constrained to a return register, which is what a return
    /// of one value already is and is the whole of what either does. The `ret` itself comes from
    /// the epilogue for both, long after this, because the frame has to be given back first.
    ///
    /// The two register files are counted separately, so a structure of a `double` and a `long`
    /// leaves the `double` in the first vector register and the `long` in the first integer one
    /// rather than in the second of either. That is the same walk `rucc_codegen::abi` makes on
    /// the other side of the call, which is what makes the two ends agree.
    ///
    /// A function whose answer went through memory gives back the address it was handed, in front
    /// of nothing else, because a signature that returns that way returns nothing else. That the
    /// caller already knows the address is not enough: it is allowed to read the register instead,
    /// and a caller that does gets whatever the allocator last left there. In a leaf function that
    /// is usually the right answer by accident, and one call in the body is enough to make it a
    /// wild pointer, which is why this is written rather than left to luck.
    ///
    /// Where everything goes is worked out before anything is written, so a return this cannot
    /// make leaves no half of one behind.
    /// Whether what a `return` gives back is the one value that goes back on the x87 stack.
    fn gives_back_x87(&self, inst: Inst) -> bool {
        let [value] = self.source[self.source[inst].args] else { return false };
        abi::on_the_stack(self.source[value].ty)
    }

    fn returned(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let values: Vec<Value> = self.source[self.source[inst].args].to_vec();
        let (mut ints, mut floats) = (0usize, 0usize);
        let mut parts = Vec::with_capacity(values.len() + 1);
        // An eighty bit value goes back on the x87 stack, which is where the convention says it is
        // and is the one place a value is left rather than put in a register. So the whole of the
        // return is an `fld` of its slot, and the stack it leaves the value on is not empty at the
        // `ret`, which is the one time in this file that is true and is what the convention asks
        // for. What comes after is the epilogue, which gives the frame back and touches nothing in
        // the unit.
        if let [value] = values[..] {
            let ty = self.source[value].ty;
            if abi::on_the_stack(ty) && self.sret().is_none() {
                let span = self.source.span(inst);
                let from = self.x87_slot(value);
                let from = self.through(from);
                self.x87_at("fld_t", span, from);
                return Ok(());
            }
        }
        for value in self.sret().into_iter().chain(values) {
            let ty = self.source[value].ty;
            let at = if crate::term::float_slot(ty).is_some() { &mut floats } else { &mut ints };
            // Why it cannot come back, and not only that it cannot. A type that travels nowhere
            // says so itself, and a type that travels perfectly well ran out of registers.
            let missing = abi::refuses(ty).unwrap_or(Missing::NoRoom);
            let name = abi::ret_of(ty, *at).ok_or(Unsupported::Returned { inst, missing })?;
            *at += 1;
            // The register is the target's answer and not one worked out here, the same as it is
            // for a return of one value, so that both halves of a pair and every rule that writes
            // half of one are reading the same table.
            let opcode = name.strip_prefix(PREFIX).expect("a machine instruction of this target");
            let form = x86_64::form(opcode).ok_or_else(|| self.unsupported(inst))?;
            let [desc] = form.operands() else { return Err(self.unsupported(inst)) };
            parts.push((self.names.intern(name), self.reg_of(value)?, *desc));
        }

        let block = self.at.expect("a block is being filled");
        let span = self.source.span(inst);
        for (opcode, reg, desc) in parts {
            let operand = mir::Operand {
                reg,
                class: desc.class,
                role: desc.role,
                constraint: desc.constraint,
            };
            self.out.build(block, mir::Opcode::new(opcode)).at(span).operand(operand).finish();
        }
        Ok(())
    }

    /// One `alloca`: the bytes it asks for go on the list the frame is laid out from, and the
    /// address of them is one instruction.
    ///
    /// The instruction is a `lea` off the stack pointer, which is the one register that reaches
    /// the frame in every function, and its displacement is left at nothing because there is no
    /// frame yet. Which instruction is waiting for which local is remembered, and
    /// [`crate::finish`] fills the numbers in after [`crate::frame::Frame`] has placed them.
    ///
    /// There is deliberately no rule for `alloca` and no name for one in [`crate::term`], and
    /// that is what stops it being folded into something else. An operand shown as the
    /// instruction that computed it is offered to the matcher by its name, so an `alloca` with no
    /// name is one no pattern can reach past, and the address it computes is always in a register
    /// by the time anything reads it.
    fn reserve(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let data = &self.source[inst];
        // A variable length array carries the size it wants as an operand rather than in the
        // instruction, which is the whole of what tells the two apart here.
        if !self.source[data.args].is_empty() {
            return Err(Unsupported::Dynamic { inst });
        }
        let Extra::Mem(mem) = data.extra else { return Err(self.unsupported(inst)) };
        let info = self.source[mem];
        let size = u32::try_from(info.size).map_err(|_| Unsupported::Dynamic { inst })?;
        let result = data.first_result.ok_or_else(|| self.unsupported(inst))?;

        // At least one, because the frame divides by the alignment and an object with no
        // alignment at all is one the front end had nothing to say about rather than one that may
        // go anywhere.
        let index = self.stack.locals.len();
        self.stack.locals.push(Local { size, align: info.align.max(1) });

        let block = self.at.expect("a block is being filled");
        let reg = self.new_reg(result);
        let span = self.source.span(inst);
        let lea = mir::Opcode::new(self.names.intern(&format!("{PREFIX}{}", x86_64::FRAME.lea)));
        let sp = mir::Operand::read(mir::Reg::physical(self.conv.stack_pointer), self.gpr);
        let made =
            self.out.build(block, lea).at(span).def(reg, self.gpr).mem(mir::Mem::at(sp)).finish();
        self.stack.addresses.push((made, index));
        Ok(())
    }

    /// Whether an instruction has an eighty bit float anywhere in it.
    ///
    /// Producing one and reading one are the same question here, because what makes one of these
    /// different from every other instruction is not the operation but where the value is. A
    /// `long double` is on the x87 stack while it is being worked on and in a frame slot the rest
    /// of the time, and neither of those is somewhere the operand of a rule could point.
    fn touches_x87(&self, inst: Inst) -> bool {
        let data = &self.source[inst];
        data.results().any(|value| on_x87(self.source[value].ty))
            || self.source[data.args].iter().any(|&arg| on_x87(self.source[arg].ty))
    }

    /// Everything that happens to an eighty bit float, as the group of instructions it is.
    ///
    /// The first six move one, and every one of those is a load, a store, or a load and a store at
    /// two different formats, because that is the whole of what this machine converts with: the
    /// x87 has no instruction that turns one thing on its stack into another, so a widening is
    /// `fld` of the narrow format and a narrowing is `fstp` of it.
    ///
    /// The rest work on one, and they are here rather than in a rule for the same reason the six
    /// are. An add is a push, a push, the add and a pop, and what passes between those four is the
    /// top of a stack nothing allocates from, so there is no value in the middle of the group for
    /// a pattern to bind or a replacement to name. The comparison is the same shape with its last
    /// two instructions folded into one opcode, which is where the byte it produces comes from.
    ///
    /// Every group leaves the stack as empty as it found it, which is what `spec/10-backend.md`
    /// section 10.8 asks of one and is why nothing in this file has to track a depth: each push
    /// below is answered by a pop a line or two later, so no two groups can ever be looking at
    /// the same eight registers.
    fn x87(&mut self, inst: Inst) -> Result<(), Unsupported> {
        match self.source[inst].opcode {
            Opcode::Load => self.x87_load(inst),
            Opcode::Store => self.x87_store(inst),
            Opcode::FPExt => self.x87_widen(inst),
            Opcode::FPTrunc => self.x87_narrow(inst),
            Opcode::SIToFP => self.x87_from_signed(inst),
            Opcode::FPToSI => self.x87_to_signed(inst),
            Opcode::FAdd => self.x87_arith(inst, "fadd_p"),
            Opcode::FSub => self.x87_arith(inst, "fsub_p"),
            Opcode::FMul => self.x87_arith(inst, "fmul_p"),
            Opcode::FDiv => self.x87_arith(inst, "fdiv_p"),
            Opcode::FNeg => self.x87_flip(inst),
            Opcode::FCmp => self.x87_compare(inst),
            Opcode::FConst => self.x87_const(inst),
            _ => Err(self.unsupported(inst)),
        }
    }

    /// The eighty bit parameters of a block, copied out of the addresses an edge handed over and
    /// into slots of the block's own.
    ///
    /// What crosses an edge for a value of this type is an address, because the value is sixteen
    /// bytes of the frame and no register holds any of it. The block cannot keep that address: a
    /// second edge into the same block hands over a second one, and a read after the block would
    /// then be a read of whichever edge was taken rather than of one place. So the block has a
    /// slot per parameter and the bytes are copied into it here, which is the move on an edge that
    /// every other type gets from the allocator.
    ///
    /// Every load runs before every store and the stores run backwards, so all of the values are
    /// on the x87 stack at once and nothing reads a slot another one has already written. That
    /// costs nothing in the ordinary case of one parameter and is what makes the back edge of a
    /// loop that swaps two of these work. It is also the reason for the limit: the stack is eight
    /// deep, and a block with more of these than that is refused rather than copied in an order
    /// that could be wrong.
    fn settle(&mut self, block: Block, arriving: &[(Value, mir::Reg)]) -> Result<(), Unsupported> {
        let Some(&(first, _)) = arriving.first() else { return Ok(()) };
        if arriving.len() > X87_DEPTH {
            let ty = self.source[first].ty;
            return Err(Unsupported::Phi { block, count: arriving.len(), ty });
        }
        // A block parameter comes from no instruction, so what this points at is the first thing
        // in the block, which is where a reader looking for the copy would look.
        let first_inst = self.source.insts(block).next();
        let span = first_inst.map_or(Span::DUMMY, |it| self.source.span(it));
        for &(_, reg) in arriving {
            let from = self.through(reg);
            self.x87_at("fld_t", span, from);
        }
        for &(param, _) in arriving.iter().rev() {
            let into = self.x87_slot(param);
            let into = self.through(into);
            self.x87_at("fstp_t", span, into);
        }
        Ok(())
    }

    /// The frame slot an eighty bit value lives in, as its address in a fresh register.
    ///
    /// The slot is the value's for the whole function and is taken the first time somebody asks.
    /// The address is worked out again every time, which is a `lea` per use and is deliberate: one
    /// address kept in a register from the definition to the last use would hold a general purpose
    /// register open across everything in between, and a function with a handful of these in it
    /// would spend its registers on addresses of things rather than on things.
    fn x87_slot(&mut self, value: Value) -> mir::Reg {
        // An argument of the function has a slot already and it is the caller's. The convention
        // puts the bytes in the argument area and hands over where they are, so the address that
        // arrived is the answer and no second copy of the value is made. Nothing ever writes to a
        // value of this type once it exists, so nothing writes to the caller's copy either. A
        // parameter of any other block is not this: what arrived there is an address a predecessor
        // chose, [`Lowering::settle`] has already copied the bytes out of it, and the slot those
        // bytes landed in is the one below.
        let entry = self.source.entry();
        if let (Def::Param { block, .. }, Some(reg)) =
            (self.source[value].def, self.regs[value.index()])
        {
            if entry == Some(block) {
                return reg;
            }
        }
        let index = match self.slots[value.index()] {
            Some(index) => index,
            None => {
                let index = self.stack.locals.len();
                self.stack.locals.push(Local { size: X87_BYTES, align: X87_BYTES });
                self.slots[value.index()] = Some(index);
                index
            }
        };
        let block = self.at.expect("a block is being filled");
        self.frame_address(block, index)
    }

    /// The bytes a value crosses between a register and the x87 stack through, as their address
    /// in a fresh register.
    fn x87_crossing(&mut self) -> mir::Reg {
        let index = match self.crossing {
            Some(index) => index,
            None => {
                let index = self.stack.locals.len();
                self.stack.locals.push(Local { size: X87_CROSSING, align: X87_CROSSING });
                self.crossing = Some(index);
                index
            }
        };
        let block = self.at.expect("a block is being filled");
        self.frame_address(block, index)
    }

    /// The two control words, as the address of the first of them in a fresh register.
    fn x87_control(&mut self) -> mir::Reg {
        let index = match self.control {
            Some(index) => index,
            None => {
                let index = self.stack.locals.len();
                self.stack.locals.push(Local { size: 4, align: 4 });
                self.control = Some(index);
                index
            }
        };
        let block = self.at.expect("a block is being filled");
        self.frame_address(block, index)
    }

    /// An address held in a register, as the addressing mode that reaches it.
    fn through(&self, reg: mir::Reg) -> mir::Mem {
        mir::Mem::at(mir::Operand::read(reg, self.gpr))
    }

    /// One instruction of a group, which names an address and nothing else.
    ///
    /// Every x87 instruction that moves a value is one of these. What it does to the stack is in
    /// the mnemonic rather than in an operand, so there is no register to write down and no
    /// register the allocator gets a say in.
    fn x87_at(&mut self, name: &str, span: Span, at: mir::Mem) {
        let block = self.at.expect("a block is being filled");
        let opcode = mir::Opcode::new(self.names.intern(&format!("{PREFIX}{name}")));
        self.out.build(block, opcode).at(span).mem(at).finish();
    }

    /// One instruction of a group that names nothing at all.
    ///
    /// The arithmetic is these. Both of an add's operands are already on the stack when it runs
    /// and so is where the answer goes, and the stack is not somewhere an instruction says, so
    /// `faddp` has an argument in the assembler's syntax and nothing here for the argument to come
    /// from. What it works on is which two pushes came before it, which is a fact about the order
    /// of the group and is why the group is written in one place.
    fn x87_only(&mut self, name: &str, span: Span) {
        let block = self.at.expect("a block is being filled");
        let opcode = mir::Opcode::new(self.names.intern(&format!("{PREFIX}{name}")));
        self.out.build(block, opcode).at(span).finish();
    }

    /// A `load` of a `long double`: onto the stack from where it was, and off it into the slot.
    ///
    /// Two instructions rather than the two general purpose moves the same sixteen bytes would
    /// take, because `fld` and `fstp` at this format neither convert nor look: the value goes on
    /// in the format it was already in and comes back off in it, so a signalling NaN stays one
    /// and nothing is raised. Which is what makes this a copy at all.
    fn x87_load(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let (args, result) = self.ends(inst)?;
        let &address = args.first().ok_or_else(|| self.unsupported(inst))?;
        let span = self.source.span(inst);
        let from = self.reg_of(address)?;
        let from = self.through(from);
        let into = self.x87_slot(result);
        let into = self.through(into);
        self.x87_at("fld_t", span, from);
        self.x87_at("fstp_t", span, into);
        Ok(())
    }

    /// A `store` of a `long double`: the same pair the other way round.
    fn x87_store(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let args = self.source[self.source[inst].args].to_vec();
        let [value, address] = args[..] else { return Err(self.unsupported(inst)) };
        let span = self.source.span(inst);
        let from = self.x87_slot(value);
        let from = self.through(from);
        let into = self.reg_of(address)?;
        let into = self.through(into);
        self.x87_at("fld_t", span, from);
        self.x87_at("fstp_t", span, into);
        Ok(())
    }

    /// A `float`, a `double` or an integer becoming a `long double`.
    ///
    /// Through memory, because the x87 reads memory and nothing else: the value is in a register
    /// the machine has and the unit has no way to be handed one, so it is written to the crossing
    /// bytes and loaded back at the format that widens it. Every one of these is exact. Sixty four
    /// bits of significand and fifteen of exponent hold every `float`, every `double` and every
    /// sixty four bit integer outright, so none of the four can round and none can raise.
    fn x87_across(
        &mut self,
        inst: Inst,
        put: &'static str,
        class: RegClass,
        get: &'static str,
    ) -> Result<(), Unsupported> {
        let (args, result) = self.ends(inst)?;
        let &source = args.first().ok_or_else(|| self.unsupported(inst))?;
        let span = self.source.span(inst);
        let value = self.reg_of(source)?;
        let across = self.x87_crossing();
        let across = self.through(across);
        let into = self.x87_slot(result);
        let into = self.through(into);

        let block = self.at.expect("a block is being filled");
        let store = mir::Opcode::new(self.names.intern(&format!("{PREFIX}{put}")));
        self.out.build(block, store).at(span).uses(value, class).mem(across).finish();
        self.x87_at(get, span, across);
        self.x87_at("fstp_t", span, into);
        Ok(())
    }

    /// A `long double` becoming a `float`, a `double` or an integer.
    ///
    /// Through memory for the reason above and in the same three instructions backwards. The two
    /// that go to a float round to nearest, which is what the control word says unless somebody
    /// has changed it and is what C wants. The two that go to an integer do not, which is why they
    /// do not come here.
    fn x87_back(
        &mut self,
        inst: Inst,
        put: &'static str,
        get: &'static str,
        class: RegClass,
    ) -> Result<(), Unsupported> {
        let (args, result) = self.ends(inst)?;
        let &source = args.first().ok_or_else(|| self.unsupported(inst))?;
        let span = self.source.span(inst);
        let from = self.x87_slot(source);
        let from = self.through(from);
        let across = self.x87_crossing();
        let across = self.through(across);

        self.x87_at("fld_t", span, from);
        self.x87_at(put, span, across);
        let block = self.at.expect("a block is being filled");
        let reg = self.new_reg(result);
        let load = mir::Opcode::new(self.names.intern(&format!("{PREFIX}{get}")));
        self.out.build(block, load).at(span).def(reg, class).mem(across).finish();
        Ok(())
    }

    /// An `fpext` up to a `long double`, which is the only direction this machine has one in.
    fn x87_widen(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let sse = self.conv.sse_class;
        match self.source[self.narrow(inst)?].ty.bits() {
            32 => self.x87_across(inst, "movss_mr", sse, "fld_s"),
            64 => self.x87_across(inst, "movsd_mr", sse, "fld_l"),
            _ => Err(self.unsupported(inst)),
        }
    }

    /// An `fptrunc` down from a `long double`, which is the other direction of the same.
    fn x87_narrow(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let sse = self.conv.sse_class;
        let result = self.source[inst].first_result.ok_or_else(|| self.unsupported(inst))?;
        match self.source[result].ty.bits() {
            32 => self.x87_back(inst, "fstp_s", "movss_rm", sse),
            64 => self.x87_back(inst, "fstp_l", "movsd_rm", sse),
            _ => Err(self.unsupported(inst)),
        }
    }

    /// A `sitofp` up to a `long double`.
    ///
    /// Thirty two bits and sixty four, and nothing narrower, because C widens an integer to `int`
    /// before it converts one and the front end writes that widening down. An unsigned integer is
    /// not here at all: `fild` reads its operand as signed, so a value above the signed range
    /// comes back short by two to the sixty fourth and has to be added back, which is arithmetic
    /// rather than a move and waits with the rest of it.
    fn x87_from_signed(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let gpr = self.gpr;
        match self.source[self.narrow(inst)?].ty.bits() {
            32 => self.x87_across(inst, "mov_mr_32", gpr, "fild_l"),
            64 => self.x87_across(inst, "mov_mr_64", gpr, "fild_ll"),
            _ => Err(self.unsupported(inst)),
        }
    }

    /// An `fptosi` down from a `long double`, which is the one conversion here with no single
    /// instruction behind it.
    ///
    /// C cuts towards zero and the unit rounds the way its control word says, so the store that
    /// takes the value off the stack is wrapped in the control word being saved, changed and put
    /// back. Five instructions around the one that does the work, and three more moving the word
    /// through a register, because this machine has no way to OR a constant into memory at this
    /// width. The unit has a shorter answer in `fisttp`, and `spec/10-backend.md` section 10.8
    /// says why it is not used: it is SSE3, the x86-64 baseline is not, and there is nothing here
    /// that can gate an instruction on a feature yet.
    fn x87_to_signed(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let (args, result) = self.ends(inst)?;
        let &source = args.first().ok_or_else(|| self.unsupported(inst))?;
        let (put, get) = match self.source[result].ty.bits() {
            32 => ("fistp_l", "mov_rm_32"),
            64 => ("fistp_ll", "mov_rm_64"),
            _ => return Err(self.unsupported(inst)),
        };
        let span = self.source.span(inst);
        let gpr = self.gpr;
        let from = self.x87_slot(source);
        let from = self.through(from);
        let across = self.x87_crossing();
        let across = self.through(across);
        let control = self.x87_control();
        let saved = self.through(control).plus(0);
        let cut = self.through(control).plus(2);

        // The word the unit has now, into the first of the two slots and into a register, with the
        // rounding field turned to truncate on the way to the second.
        self.x87_at("fnstcw", span, saved);
        let block = self.at.expect("a block is being filled");
        let was = self.out.new_vreg(gpr);
        let read = mir::Opcode::new(self.names.intern("x64.mov_rm_16"));
        self.out.build(block, read).at(span).def(was, gpr).mem(saved).finish();
        let now = self.out.new_vreg(gpr);
        let set = mir::Opcode::new(self.names.intern("x64.or_ri_16"));
        // Two address, which is written out here rather than taken from the two shorthands
        // because the shorthands leave an operand unconstrained: this machine ORs into the
        // register it read, so the two have to be the same one and only the constraint says so.
        self.out
            .build(block, set)
            .at(span)
            .operand(mir::Operand::write(now, gpr).with(Constraint::Reuse(1)))
            .operand(mir::Operand::read(was, gpr))
            .imm(X87_TRUNCATE)
            .finish();
        let write = mir::Opcode::new(self.names.intern("x64.mov_mr_16"));
        self.out.build(block, write).at(span).uses(now, gpr).mem(cut).finish();

        // The conversion itself, under the changed word, and then the word the unit had put back
        // before anything else runs.
        self.x87_at("fldcw", span, cut);
        self.x87_at("fld_t", span, from);
        self.x87_at(put, span, across);
        self.x87_at("fldcw", span, saved);

        let block = self.at.expect("a block is being filled");
        let reg = self.new_reg(result);
        let load = mir::Opcode::new(self.names.intern(&format!("{PREFIX}{get}")));
        self.out.build(block, load).at(span).def(reg, gpr).mem(across).finish();
        Ok(())
    }

    /// A constant of this type, as the bits of it written into its slot.
    ///
    /// No x87 instruction at all, which is the surprise here. A slot holding an eighty bit value is
    /// the value, so a constant is ten bytes put where the value lives, and the unit never has to
    /// see it: whatever reads it will `fld` it out of the slot the way it reads any other one.
    ///
    /// Ten bytes in two goes, because the machine stores eight at a time and there is no store of
    /// an immediate to memory, so each half is put in a register first. The six bytes above the ten
    /// are left alone, since nothing reads them: they are the padding that makes the type sixteen
    /// wide and they are unspecified in the psABI rather than zero.
    ///
    /// The other way is a constant pool, an `fldt` of a symbol, and a relocation, which is what a
    /// compiler with somewhere to put a literal does. This back end has nowhere to put one yet, and
    /// four instructions in the frame is what that costs until it does.
    fn x87_const(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let Extra::Imm(imm) = self.source[inst].extra else { return Err(self.unsupported(inst)) };
        let result = self.source[inst].first_result.ok_or_else(|| self.unsupported(inst))?;
        let bits = self.source[imm].bits();
        let span = self.source.span(inst);
        let gpr = self.gpr;
        let slot = self.x87_slot(result);
        let low = self.through(slot).plus(0);
        let high = self.through(slot).plus(8);

        let block = self.at.expect("a block is being filled");
        for (bytes, at, into) in
            [(bits as u64 as i64, low, "64"), (((bits >> 64) & 0xffff) as i64, high, "16")]
        {
            let held = self.out.new_vreg(gpr);
            let put = mir::Opcode::new(self.names.intern(&format!("{PREFIX}mov_ri_{into}")));
            self.out.build(block, put).at(span).def(held, gpr).imm(bytes).finish();
            let store = mir::Opcode::new(self.names.intern(&format!("{PREFIX}mov_mr_{into}")));
            self.out.build(block, store).at(span).uses(held, gpr).mem(at).finish();
        }
        Ok(())
    }

    /// One arithmetic instruction on two eighty bit values, as the four it takes.
    ///
    /// The left operand is pushed first and the right one on top of it, so the left ends up
    /// underneath and the instruction computes the top against the one below in that order, which
    /// is what a subtraction and a division need and is why neither `fsubrp` nor `fdivrp` appears
    /// anywhere in this file. The reversed forms exist for a code generator that decided its push
    /// order the other way round, and this one does not.
    ///
    /// The answer is left where the deeper of the two was and the shallower is gone, which is what
    /// the `p` on the mnemonic means, so one push has already been paid back by the time the
    /// `fstp` runs and the stack is level again after it.
    ///
    /// Nothing here is folded and nothing is reused. Two values that are the same value get two
    /// pushes of the same slot, and an operand that was just computed is read back out of the slot
    /// it was written to rather than left on the stack, which costs a store and a load per
    /// instruction in an expression. Keeping a partial result on the stack across the next
    /// instruction's operands means knowing how deep the stack is at every point in the block, and
    /// that is a different thing from writing a group.
    fn x87_arith(&mut self, inst: Inst, with: &'static str) -> Result<(), Unsupported> {
        let (args, result) = self.ends(inst)?;
        let [left, right] = args[..] else { return Err(self.unsupported(inst)) };
        let span = self.source.span(inst);
        let left = self.x87_slot(left);
        let left = self.through(left);
        let right = self.x87_slot(right);
        let right = self.through(right);
        let into = self.x87_slot(result);
        let into = self.through(into);
        self.x87_at("fld_t", span, left);
        self.x87_at("fld_t", span, right);
        self.x87_only(with, span);
        self.x87_at("fstp_t", span, into);
        Ok(())
    }

    /// A negation, which is a push, the sign bit turned over and a pop.
    ///
    /// `fchs` does not read the value as a number, so this is right for a zero, for an infinity
    /// and for a NaN, and it raises nothing on any of them. Which is what C asks of a negation and
    /// is not what subtracting from zero would give: `0.0L - x` is a different answer at a
    /// negative zero and a signalling one at a NaN.
    fn x87_flip(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let (args, result) = self.ends(inst)?;
        let &source = args.first().ok_or_else(|| self.unsupported(inst))?;
        let span = self.source.span(inst);
        let from = self.x87_slot(source);
        let from = self.through(from);
        let into = self.x87_slot(result);
        let into = self.through(into);
        self.x87_at("fld_t", span, from);
        self.x87_only("fchs", span);
        self.x87_at("fstp_t", span, into);
        Ok(())
    }

    /// A comparison of two eighty bit values, as the two pushes and the one opcode that reads them.
    ///
    /// The right operand is pushed first and the left one on top of it, which is the other way
    /// round from the arithmetic and is because `fucomip` asks about the top against what is under
    /// it: the comparison this machine can do is the top's, so the value the predicate is about
    /// has to be the top. The pop that gets the loser off the stack and the byte that reads the
    /// flags are both inside the opcode, since what passes between those and the comparison is the
    /// flags and the flags are not something anything here can name.
    ///
    /// Which of the ten opcodes, and which way round, is the same table the vector comparisons
    /// match against in `rules/x86-64.rules`, and it has to stay the same table: a predicate that
    /// picked a different condition here than there would be a `long double` comparison that
    /// disagreed with the `double` comparison of the same two numbers, which is the one thing a
    /// wider format is not allowed to do.
    ///
    /// The always false and the always true are refused rather than folded into a constant,
    /// because a comparison this machine never has to do is one the optimizer should have removed
    /// and an instruction here that quietly agreed with it would hide that it did not.
    fn x87_compare(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let Extra::FloatPred(pred) = self.source[inst].extra else {
            return Err(self.unsupported(inst));
        };
        let (args, result) = self.ends(inst)?;
        let [left, right] = args[..] else { return Err(self.unsupported(inst)) };
        // Two of the fourteen need a second byte and an instruction to put the two together,
        // because they are two conditions at once: an ordered equal is equal and not unordered,
        // and an unordered not equal is either. The opcode carries all of that and says here only
        // that it writes somewhere else as well.
        let (name, reversed, both) = match pred {
            FloatPred::Ogt => ("fucomip_set_a", false, false),
            FloatPred::Oge => ("fucomip_set_ae", false, false),
            FloatPred::Olt => ("fucomip_set_a", true, false),
            FloatPred::Ole => ("fucomip_set_ae", true, false),
            FloatPred::One => ("fucomip_set_ne", false, false),
            FloatPred::Ord => ("fucomip_set_np", false, false),
            FloatPred::Uno => ("fucomip_set_p", false, false),
            FloatPred::Ueq => ("fucomip_set_e", false, false),
            FloatPred::Ult => ("fucomip_set_b", false, false),
            FloatPred::Ule => ("fucomip_set_be", false, false),
            FloatPred::Ugt => ("fucomip_set_b", true, false),
            FloatPred::Uge => ("fucomip_set_be", true, false),
            FloatPred::Oeq => ("fucomip_set_e_and_np", false, true),
            FloatPred::Une => ("fucomip_set_ne_or_p", false, true),
            FloatPred::False | FloatPred::True => return Err(self.unsupported(inst)),
        };
        let (top, under) = if reversed { (right, left) } else { (left, right) };

        let span = self.source.span(inst);
        let gpr = self.gpr;
        let under = self.x87_slot(under);
        let under = self.through(under);
        let top = self.x87_slot(top);
        let top = self.through(top);
        self.x87_at("fld_t", span, under);
        self.x87_at("fld_t", span, top);

        let block = self.at.expect("a block is being filled");
        let reg = self.new_reg(result);
        // Taken before the instruction is started rather than inside it, since both come from the
        // same function being built and only one thing at a time may be adding to it.
        let spare = both.then(|| self.out.new_vreg(gpr));
        let opcode = mir::Opcode::new(self.names.intern(&format!("{PREFIX}{name}")));
        let mut build = self.out.build(block, opcode).at(span).def(reg, gpr);
        if let Some(spare) = spare {
            build = build.def(spare, gpr);
        }
        build.finish();
        Ok(())
    }

    /// The operands and the one result of an instruction that has exactly one.
    fn ends(&self, inst: Inst) -> Result<(&'a [Value], Value), Unsupported> {
        let data = &self.source[inst];
        let result = data.first_result.ok_or_else(|| self.unsupported(inst))?;
        Ok((&self.source[data.args], result))
    }

    /// The operand of a conversion, which is the end of it that is not the `long double`.
    fn narrow(&self, inst: Inst) -> Result<Value, Unsupported> {
        let args = &self.source[self.source[inst].args];
        args.first().copied().ok_or_else(|| self.unsupported(inst))
    }

    /// One `va_start`, as the four fields of the list it was handed.
    ///
    /// Two of them are numbers this already knows, and each costs an instruction to put in a
    /// register before it can be stored, because the machine here has no store of an immediate to
    /// memory. The other two are addresses in the frame, and each is a `lea` [`crate::finish`]
    /// finishes: the save area is one of the function's own stack objects, and the caller's
    /// argument area is where the parameters that had no register came from, which is the same
    /// place and the same fixup a parameter past the sixth already uses.
    ///
    /// What is written is exactly the four fields [`crate::varargs`] describes, in the order they
    /// are laid out, so that reading this beside that table is the whole of the check.
    fn va_start(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let Some(&list) = self.source[self.source[inst].args].first() else {
            return Err(self.unsupported(inst));
        };
        let started = self.varargs.ok_or_else(|| self.unsupported(inst))?;
        let list = self.reg_of(list)?;
        let block = self.at.expect("a block is being filled");
        let span = self.source.span(inst);

        for (at, count) in
            [(varargs::GP_OFFSET, started.integers), (varargs::FP_OFFSET, started.floats)]
        {
            let held = self.out.new_vreg(self.gpr);
            let load = mir::Opcode::new(self.names.intern("x64.mov_ri_32"));
            self.out.build(block, load).at(span).def(held, self.gpr).imm(i64::from(count)).finish();

            let store = mir::Opcode::new(self.names.intern("x64.mov_mr_32"));
            let mem = self.field(list, at);
            self.out.build(block, store).at(span).uses(held, self.gpr).mem(mem).finish();
        }

        // The first argument the signature did not name, which is as far up the caller's argument
        // area as the ones it did name reached. Nothing here knows where that area is, so the
        // distance is recorded the way a parameter read out of it is and finished with it.
        let overflow = self.out.new_vreg(self.gpr);
        let lea = mir::Opcode::new(self.names.intern(&format!("{PREFIX}{}", x86_64::FRAME.lea)));
        let sp = mir::Operand::read(mir::Reg::physical(self.conv.stack_pointer), self.gpr);
        let made = self
            .out
            .build(block, lea)
            .at(span)
            .def(overflow, self.gpr)
            .mem(mir::Mem::at(sp))
            .finish();
        self.stack.arguments.push((made, started.incoming));

        let save = self.frame_address(block, started.save);
        for (at, held) in [(varargs::OVERFLOW, overflow), (varargs::SAVE_AREA, save)] {
            let store = mir::Opcode::new(self.names.intern("x64.mov_mr_64"));
            let mem = self.field(list, at);
            self.out.build(block, store).at(span).uses(held, self.gpr).mem(mem).finish();
        }
        Ok(())
    }

    /// One field of a list, as the addressing mode that reaches it.
    fn field(&self, list: mir::Reg, at: i64) -> mir::Mem {
        let base = mir::Operand::read(list, self.gpr);
        mir::Mem::at(base).plus(i32::try_from(at).expect("a field of a list is a small offset"))
    }

    /// The address of a name: one `lea` off the instruction pointer, with the name on it.
    ///
    /// The same instruction an `alloca` gets and for a related reason. An address that is not in
    /// the program is a `lea` of an addressing mode that names no register, and the mode carries
    /// the symbol so that [`rucc_asm`] can write it relative to `%rip` and leave the relocation
    /// for the assembler. Both halves of that already existed: the printer writes `sym(%rip)` and
    /// the encoder emits the relocation, because a call to a name the file does not define needed
    /// them first.
    ///
    /// There is deliberately no name for this in [`crate::term`], which is what stops the address
    /// being folded into the instruction that reads it. Folding it is the right thing to do and
    /// is what turns a load of a global from two instructions into one, but it is a separate
    /// question about addressing modes and issue #282 is it. Until then the address is in a
    /// register before anything uses it, which is correct and one instruction longer.
    ///
    /// What this does not do is give the name anything to refer to. A module carries its globals
    /// and nothing writes them out, so a file that defines the variable it reads compiles to a
    /// reference the linker cannot resolve. Issue #293 is the other half.
    fn address_of(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let data = &self.source[inst];
        let Extra::Symbol(symbol) = data.extra else { return Err(self.unsupported(inst)) };
        let result = data.first_result.ok_or_else(|| self.unsupported(inst))?;

        let block = self.at.expect("a block is being filled");
        let reg = self.new_reg(result);
        let span = self.source.span(inst);
        let lea = mir::Opcode::new(self.names.intern(&format!("{PREFIX}{}", x86_64::FRAME.lea)));
        self.out.build(block, lea).at(span).def(reg, self.gpr).mem(mir::Mem::of(symbol)).finish();
        Ok(())
    }

    /// A conversion that converts nothing: the result is the operand under another type.
    ///
    /// `ptrtoint` and `inttoptr` at one width are the whole of this. An address on this machine is
    /// an integer as wide as the machine addresses, so a cast between the two changes what the
    /// type system calls the value and changes nothing about the value, and the register holding
    /// it is the register that already held it. The front end never writes either of them at any
    /// other width, because it widens or narrows around the cast rather than through it, so the
    /// two widths disagreeing here means the IR came from somewhere else and is refused rather
    /// than guessed at.
    ///
    /// Reading the operand first is what materializes it when it is a constant, which is the case
    /// that matters: a null pointer is an `inttoptr` of zero, and that zero has to reach a
    /// register before anything can call it an address.
    fn rename(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let data = &self.source[inst];
        let [arg] = self.source[data.args] else { return Err(self.unsupported(inst)) };
        let result = data.first_result.ok_or_else(|| self.unsupported(inst))?;
        if !self.is_address_width(self.source[arg].ty)
            || !self.is_address_width(self.source[result].ty)
        {
            return Err(self.unsupported(inst));
        }
        let reg = self.reg_of(arg)?;
        self.regs[result.index()] = Some(reg);
        Ok(())
    }

    /// One barrier, which on this machine is one instruction at the strongest ordering and no
    /// instruction at all at every other one.
    ///
    /// x86-64 is total store order, so the only reordering the machine does is a store followed by
    /// a load of a different address, and the only ordering that forbids that is sequential
    /// consistency. An acquire, a release and an acquire release fence are therefore already true
    /// of every program running here, and what a program wanted from writing one is that the
    /// compiler not move memory accesses across it. The optimizer has finished by the time this
    /// runs and nothing below reorders one access past another, so the constraint is already
    /// discharged and there is nothing to write.
    ///
    /// The strongest one is `mfence`, which is what gcc 16.2.0 writes for
    /// `__atomic_thread_fence(__ATOMIC_SEQ_CST)` and for `__sync_synchronize`. A locked instruction
    /// on the stack is faster on most parts and is what some compilers write instead; it is also a
    /// write to memory the program did not ask for, and the plain barrier is the one that says what
    /// it means.
    ///
    /// Written here by name rather than by a rule, for the same reason a `lea` of a symbol is:
    /// there is nothing in a barrier that a proof over bitvectors could discharge. It computes
    /// nothing, so there is no equality to state, and what makes it the right answer is the memory
    /// model, which the rule language cannot talk about.
    fn barrier(&mut self, inst: Inst) -> Result<(), Unsupported> {
        let Extra::Order(order) = self.source[inst].extra else {
            return Err(self.unsupported(inst));
        };
        if order != MemOrder::SeqCst {
            return Ok(());
        }
        let block = self.at.expect("a block is being filled");
        let span = self.source.span(inst);
        let fence = mir::Opcode::new(self.names.intern("x64.mfence"));
        self.out.build(block, fence).at(span).finish();
        Ok(())
    }

    /// Whether a type is the width an address is, which is what makes a cast to or from one free.
    fn is_address_width(&self, ty: Type) -> bool {
        ty.is_ptr() || (ty.is_int() && ty.bits() == ADDRESS_BITS)
    }

    /// Where a block goes, which in machine IR is on the block rather than on its terminator.
    ///
    /// That is why no rule ever names a block: a branch is selected for what it reads and the
    /// edges are copied across here, arguments and all. The arguments are read last, after every
    /// instruction of the block is written, because an argument that is a constant is
    /// materialized where it is first wanted and the end of the block is where an edge wants it.
    ///
    /// Which is not quite the end. A block that leaves two ways has the branch as its last
    /// instruction, and anything appended after a branch is something the branch has already
    /// jumped past, so a constant materialized here would be a register the block below reads and
    /// nothing ever writes. The branch is put back on the end when that happened, which is the
    /// only reordering anything in this crate does and is why the branch is remembered before a
    /// single argument is read.
    fn edges(&mut self, block: Block, out: mir::Block) -> Result<(), Unsupported> {
        let Some(term) = self.source.terminator(block) else { return Ok(()) };
        let branch =
            if self.source[term].opcode == Opcode::BrIf { self.out.terminator(out) } else { None };

        let calls: Vec<rucc_ir::BlockCall> = self.source.successors(term).collect();
        let mut succs = Vec::with_capacity(calls.len());
        for call in calls {
            let args: Vec<Value> = self.source[call.args].to_vec();
            let mut regs = Vec::with_capacity(args.len());
            for value in args {
                // The address of where the value is rather than the value, for the one type a
                // register holds none of. The block on the other side copies the bytes out of it
                // into a slot of its own, which is what makes a second edge into the same block
                // safe.
                let reg = if on_x87(self.source[value].ty) {
                    self.x87_slot(value)
                } else {
                    self.reg_of(value)?
                };
                regs.push(reg);
            }
            succs.push(mir::BlockCall { block: self.out_block(call.block), args: regs });
        }
        if let Some(branch) = branch {
            if self.out.terminator(out) != Some(branch) {
                self.out.remove_inst(branch);
                self.out.append_inst(out, branch);
            }
        }
        *self.out.succs_mut(out) = succs;
        Ok(())
    }

    /// The machine IR block an IR block became.
    fn out_block(&self, block: Block) -> mir::Block {
        self.blocks[block.index()].expect("every block was created before any was filled")
    }

    /// The parameters of the entry block, which are the function's arguments.
    ///
    /// They are not block parameters in the machine IR and they cannot be. A block parameter is
    /// given its value by a move on the edge into the block, and there is no edge into an entry
    /// block, so what arrives in a function is the convention's to say. [`crate::abi`] is what
    /// says it.
    ///
    /// The ones past the last register arrived in the caller's memory and are read out of it, and
    /// the loads that read them come back here so that the frame can finish them the way it
    /// finishes an `alloca`.
    fn arrive(&mut self, block: Block, out: mir::Block) -> Result<(), Unsupported> {
        let params = self.source[block].params.clone();
        // The type of each is the block's answer and what the ABI asks of it is the signature's,
        // and the two lists are the same list: a parameter the classification turned into a
        // pointer is a pointer in the block too. A block with more parameters than the signature
        // names is not one the front end writes, and each of those is taken as a plain value.
        let asked: Vec<Abi> = self.source.signature().params.iter().map(|it| it.abi).collect();
        let types: Vec<Param> = params
            .iter()
            .enumerate()
            .map(|(index, &value)| {
                let abi = asked.get(index).copied().unwrap_or_default();
                Param { ty: self.source[value].ty, abi }
            })
            .collect();
        // A save area for a function that takes arguments its signature does not name, on a
        // convention whose list is the four field one. Windows is the other kind and has no area at
        // all, so a `va_start` in one is refused rather than built wrong.
        let variadic = self.source.signature().variadic && !self.conv.shared_positions;
        let area = variadic.then(|| varargs::Area::of(self.conv));
        let arrived = abi::entry(&mut self.out, out, &types, self.conv, self.names, area)
            .map_err(|(index, missing)| Unsupported::Argument { index, missing })?;
        for (&param, reg) in params.iter().zip(&arrived.regs) {
            self.regs[param.index()] = Some(*reg);
        }
        if let Some(area) = area {
            self.save_area(out, &arrived, area);
        }
        self.stack.arguments.extend(arrived.stack);
        Ok(())
    }

    /// The prologue of a variadic function, which is every argument register it was handed written
    /// into the frame.
    ///
    /// Every one the signature did not name, that is. Which of those hold anything is a thing only
    /// the caller knew and there is nothing here to ask, so all of them are written, and the ones a
    /// named parameter took are not, because `va_start` sets the two offsets past them and nothing
    /// ever reads their slots.
    ///
    /// What that costs is up to fourteen stores in the prologue of a function that may read none of
    /// them, and the convention's answer to that is the count of vector registers in `%al`, which
    /// lets a callee skip the eight vector stores when the call passed no floats. Skipping them is a
    /// branch in a prologue, and a prologue is written long after this by [`crate::finish`], which
    /// has no blocks to branch between. So they are all written every time, which is correct and is
    /// what `-O0` costs. Issue #323 is the branch.
    ///
    /// A vector register is written eight bytes at a time and not sixteen, for the reason
    /// [`crate::varargs`] gives: the upper half of a slot is not something any reader of a list
    /// looks at.
    ///
    /// The address is computed once into a register rather than written as a displacement off the
    /// stack pointer, because a displacement into a frame is not known until after allocation and
    /// one `lea` costs less than a fixup list for a dozen stores. It is the same `lea` an `alloca`
    /// gets and [`crate::finish`] fills it in the same way.
    fn save_area(&mut self, out: mir::Block, arrived: &abi::Arrived, area: varargs::Area) {
        let save = self.stack.locals.len();
        self.stack.locals.push(Local { size: area.size, align: varargs::VECTOR_SLOT });
        self.varargs = Some(Varargs {
            save,
            incoming: arrived.used,
            integers: u32::try_from(arrived.took.0).unwrap_or(0) * area.stride(false),
            floats: area.starts_at(true)
                + u32::try_from(arrived.took.1).unwrap_or(0) * area.stride(true),
        });

        let base = self.frame_address(out, save);
        for &(reg, class, at) in &arrived.spare {
            let name = if class == self.gpr { "x64.mov_mr_64" } else { "x64.movsd_mr" };
            let store = mir::Opcode::new(self.names.intern(name));
            let up = i32::try_from(at).expect("a register save area under two gigabytes");
            let mem = mir::Mem::at(mir::Operand::read(base, self.gpr)).plus(up);
            self.out.build(out, store).uses(reg, class).mem(mem).finish();
        }
    }

    /// The address of one of the function's stack objects, in a fresh register.
    ///
    /// Written with nothing in its displacement, because where an object is in a frame is not known
    /// until after allocation, and given to [`crate::finish`] to fill in the way an `alloca` is.
    fn frame_address(&mut self, out: mir::Block, local: usize) -> mir::Reg {
        let reg = self.out.new_vreg(self.gpr);
        let lea = mir::Opcode::new(self.names.intern(&format!("{PREFIX}{}", x86_64::FRAME.lea)));
        let sp = mir::Operand::read(mir::Reg::physical(self.conv.stack_pointer), self.gpr);
        let made = self.out.build(out, lea).def(reg, self.gpr).mem(mir::Mem::at(sp)).finish();
        self.stack.addresses.push((made, local));
        reg
    }

    /// Whether an instruction is one no machine instruction is written for where it stands.
    ///
    /// Four of them, and none is a lowering decision, which is why none is a rule. A constant is
    /// written where a register for it is first wanted rather than where the IR put it, and every
    /// reader of one may have folded it into an immediate, in which case nowhere is the right
    /// place. A return of nothing has nothing to put anywhere: the epilogue gives the frame back
    /// and leaves, and it is appended to every block with no successors long after this has
    /// finished, so a return with a value is one instruction here and a return without one is
    /// none. Unless the value went back through memory, in which case there is something to put
    /// somewhere after all and the IR does not carry it: the address the caller handed over has
    /// to be in `rax` on the way out, and [`Lowering::returned`] is what writes that.
    ///
    /// An unconditional jump is the third, and there is even less of it: the edge is on the
    /// block, and whether the block it goes to is the next one and needs no jump at all is the
    /// block layout's answer rather than this one's.
    ///
    /// The fourth is a point control does not arrive at, in both of the forms the IR has for it:
    /// the `unreachable` terminator the front end puts at the end of a function whose body can run
    /// off the bottom, and the `unreachable_hint` a call to `__builtin_unreachable` becomes. What
    /// to write for a place nothing reaches is a question with no wrong answer, and nothing is the
    /// smallest one and the one gcc 16.2.0 gives at `-O0`. The terminator leaves the block with no
    /// successors, so the epilogue lands at the end of it the way it does on any other block that
    /// goes nowhere, and the function cannot fall out of its own last instruction into whatever
    /// the assembler puts next.
    fn writes_nothing(&self, inst: Inst) -> bool {
        let data = &self.source[inst];
        match data.opcode {
            Opcode::IConst | Opcode::Jump | Opcode::Unreachable | Opcode::UnreachableHint => true,
            Opcode::Return => self.source[data.args].is_empty() && self.sret().is_none(),
            _ => false,
        }
    }

    /// The rule that fires on an instruction, and what it bound.
    ///
    /// The plans are tried in order and the first that matches wins, which is the maximal munch
    /// `spec/10-backend.md` asks for: a plan that offers more to the matcher is tried before one
    /// that offers less.
    fn select(&self, inst: Inst) -> Option<(Plan, Match<Term>)> {
        for plan in self.plans(inst) {
            let terms = Terms::new(self.source, inst, plan);
            if let Some(matched) = TABLE.find(&terms, Term::Root) {
                return Some((plan, matched));
            }
        }
        None
    }

    /// Every way this instruction can be shown to the matcher, most offered first.
    fn plans(&self, inst: Inst) -> Vec<Plan> {
        let args = &self.source[self.source[inst].args];
        let mut plans = vec![PLAIN];
        for (index, &arg) in args.iter().enumerate().take(MAX_ARGS) {
            let mut ways = Vec::new();
            if self.foldable(inst, arg) {
                ways.push(Shown::Expand);
            }
            if Terms::new(self.source, inst, PLAIN).constant(arg).is_some() {
                ways.push(Shown::Const);
            }
            ways.push(Shown::Reg);
            plans = plans
                .into_iter()
                .flat_map(|plan| {
                    ways.iter().map(move |&way| {
                        let mut next = plan;
                        next[index] = way;
                        next
                    })
                })
                .collect();
        }
        plans
    }

    /// Whether an operand may be shown as the instruction that computed it.
    ///
    /// It has to be in the same block, because a rule that folds one instruction into another
    /// moves the work to where the second one is. It has to be read only by this instruction,
    /// because folding it does not delete it for anybody else and doing the work twice is not a
    /// saving. And it has to be something rather than a block parameter, and not a constant,
    /// which is shown as a constant instead.
    fn foldable(&self, into: Inst, value: Value) -> bool {
        let Def::Result { inst, .. } = self.source[value].def else { return false };
        if self.source[inst].opcode == Opcode::IConst || self.uses[value.index()] != 1 {
            return false;
        }
        self.source.block_of(inst).is_some()
            && self.source.block_of(inst) == self.source.block_of(into)
    }

    /// The instructions a match folded into the one it matched.
    ///
    /// The plan is what says this, not the bindings: a binding is a register or a number either
    /// way, and an operand shown as the instruction that computed it is one no rule could have
    /// matched without taking that instruction, because the plan offered the matcher nothing
    /// else to call it.
    fn folds(&self, inst: Inst, plan: Plan) -> Vec<Inst> {
        let args = &self.source[self.source[inst].args];
        args.iter()
            .take(MAX_ARGS)
            .enumerate()
            .filter(|&(index, _)| plan[index] == Shown::Expand)
            .filter_map(|(_, &arg)| match self.source[arg].def {
                Def::Result { inst, .. } => Some(inst),
                Def::Param { .. } => None,
            })
            .collect()
    }

    /// Build the machine instruction a match calls for.
    fn emit(&mut self, inst: Inst, matched: &Match<Term>) -> Result<(), Unsupported> {
        let rule: &Rule = TABLE.rule(matched);
        let pieces = rule.replacement;
        let Some(Piece::App { head, arity }) = pieces.first() else {
            return Err(self.unsupported(inst));
        };
        let opcode = head.strip_prefix(PREFIX).ok_or_else(|| self.unsupported(inst))?;
        let form = x86_64::form(opcode).ok_or_else(|| self.unsupported(inst))?;

        let mut read = Read::default();
        let mut at = 1;
        for _ in 0..*arity {
            at = self.read(inst, pieces, at, &matched.bindings, &mut read)?;
        }

        let descs = form.operands();
        let writes = descs.iter().take_while(|desc| desc.role.is_def()).count();
        if descs.len() - writes != read.regs.len() {
            return Err(self.unsupported(inst));
        }

        // The first thing the instruction writes is what it computes, and any others are
        // registers the machine destroys on the way, which are fresh because nothing else is in
        // them and nothing reads them. An instruction that writes nothing at all is one whose
        // whole purpose is its effect, which is what a store is, and there is no result to put
        // anywhere.
        let mut regs = Vec::new();
        if writes > 0 {
            let result = self.source[inst].first_result.ok_or_else(|| self.unsupported(inst))?;
            regs.push(self.new_reg(result));
            // The rest are the registers the machine destroys on the way, and the class each is in
            // is the one the instruction's description gives it rather than a guess, so that an
            // instruction that wrecks a register in the other file says so.
            regs.extend(descs[1..writes].iter().map(|desc| self.out.new_vreg(desc.class)));
        } else if self.source[inst].first_result.is_some() {
            // A rule that throws away a value the IR gave a name to would leave every reader of
            // that name with nothing to read, so it is a rule this and the target disagree about.
            return Err(self.unsupported(inst));
        }
        regs.extend(read.regs.iter().copied());

        let block = self.at.expect("a block is being filled");
        let opcode = mir::Opcode::new(self.names.intern(head));
        let mut build = self.out.build(block, opcode).at(self.source.span(inst));
        for (desc, reg) in descs.iter().zip(regs) {
            let operand = mir::Operand {
                reg,
                class: desc.class,
                role: desc.role,
                constraint: desc.constraint,
            };
            build = build.operand(operand);
        }
        if let Some(mem) = read.mem {
            build = build.mem(mem);
        }
        if let Some(imm) = read.imm {
            build = build.imm(imm);
        }
        build.finish();
        Ok(())
    }

    /// Read one argument of a replacement, which is a register, a number or an address.
    ///
    /// Gives back the position after it, because a replacement is flat and an address takes
    /// arguments of its own.
    fn read(
        &mut self,
        inst: Inst,
        pieces: &'static [Piece],
        at: usize,
        bindings: &[Term],
        out: &mut Read,
    ) -> Result<usize, Unsupported> {
        match pieces.get(at) {
            Some(Piece::Int(value)) => {
                out.imm = i64::try_from(*value).ok();
                Ok(at + 1)
            }
            Some(Piece::Var { index, .. }) => {
                match bindings.get(*index) {
                    Some(&Term::Reg(value)) => {
                        let reg = self.reg_of(value)?;
                        out.regs.push(reg);
                    }
                    Some(&Term::Num(value)) => out.imm = i64::try_from(value).ok(),
                    // A pattern binds a register or a number and nothing else, so this is a
                    // rule the matcher and this file disagree about.
                    _ => return Err(self.unsupported(inst)),
                }
                Ok(at + 1)
            }
            Some(Piece::App { head, arity }) => {
                let kind = x86_64::address(head).ok_or_else(|| self.unsupported(inst))?;
                let mut inner = Read::default();
                let mut next = at + 1;
                for _ in 0..*arity {
                    next = self.read(inst, pieces, next, bindings, &mut inner)?;
                }
                let mem = address(kind, &inner, self.gpr).ok_or_else(|| self.unsupported(inst))?;
                out.mem = Some(mem);
                Ok(next)
            }
            None => Err(self.unsupported(inst)),
        }
    }

    /// The register a value is in, materializing it if it is a constant that has not been put in
    /// one yet.
    ///
    /// A constant is written where it is wanted rather than where the IR defined it, and where it
    /// is wanted is a block that need not be the one the IR defined it in. So the register holding
    /// one is only good inside the block it was written into, and a second block that wants the
    /// same constant gets its own. Anything else is a register read where nothing wrote it: the
    /// IR guarantees a definition dominates its uses, and this moved the definition.
    ///
    /// Writing the number again is also the right answer and not merely the safe one. It is one
    /// instruction that reads nothing, which is cheaper than holding a register live across a
    /// branch for it, and it is what a rematerializing allocator would do with the value anyway.
    fn reg_of(&mut self, value: Value) -> Result<mir::Reg, Unsupported> {
        let constant = match self.source[value].def {
            Def::Result { inst, .. } => {
                (self.source[inst].opcode == Opcode::IConst).then_some(inst)
            }
            Def::Param { .. } => None,
        };
        let here = self.at.expect("a block is being filled");
        if let Some(reg) = self.regs[value.index()] {
            if constant.is_none() || self.written[value.index()] == Some(here) {
                return Ok(reg);
            }
        }
        if let Some(inst) = constant {
            // Cleared so that the register the constant is written into is a new one rather than
            // the one the block above wrote, which is still being read up there.
            self.regs[value.index()] = None;
            let matched = self
                .select(inst)
                .map(|(_, matched)| matched)
                .ok_or_else(|| self.unsupported(inst))?;
            self.emit(inst, &matched)?;
            // The same mark the loop over the instructions makes, and it has to be made here as
            // well because this is the only place a constant is ever selected: the loop skips one
            // where the IR wrote it, so a rule that lowers a constant fires from nowhere else and
            // would be reported as a rule nothing reaches.
            self.fired.mark(matched.rule);
            self.written[value.index()] = Some(here);
            return Ok(self.regs[value.index()].expect("a constant is written into a register"));
        }
        Ok(self.new_reg(value))
    }

    /// Which register file a value of that type lives in.
    ///
    /// The vector one for the two float widths the machine has scalar instructions for, and the
    /// general purpose one for everything else. A `long double` is in neither, and it is here
    /// rather than in the vector class on purpose: it would be put in a register that cannot hold
    /// it, and there is no rule that names one, so the instruction computing it is reported. The
    /// wrong class would make that a wrong program instead of a refused one.
    fn class_of(&self, ty: Type) -> RegClass {
        match crate::term::float_slot(ty) {
            Some(_) => self.conv.sse_class,
            None => self.gpr,
        }
    }

    /// A fresh register for a value, which is what the instruction computing it writes.
    fn new_reg(&mut self, value: Value) -> mir::Reg {
        if let Some(reg) = self.regs[value.index()] {
            return reg;
        }
        let reg = self.out.new_vreg(self.class_of(self.source[value].ty));
        self.regs[value.index()] = Some(reg);
        reg
    }

    fn unsupported(&self, inst: Inst) -> Unsupported {
        let data = &self.source[inst];
        Unsupported::Inst {
            inst,
            term: Terms::new(self.source, inst, PLAIN).name(inst),
            opcode: data.opcode,
            ty: data.first_result.map(|result| self.source[result].ty),
        }
    }
}

/// What the arguments of one replacement came to.
#[derive(Debug, Default)]
struct Read {
    regs: Vec<mir::Reg>,
    imm: Option<i64>,
    mem: Option<mir::Mem>,
}

/// The addressing mode an address constructor's arguments make.
///
/// One arm per constructor rather than a question asked of the kind, because what the arguments
/// mean is the whole of what tells the four apart: the same register is a base in one and an
/// index in another, and the same constant is a scale in one and a displacement in another.
fn address(kind: x86_64::Address, read: &Read, gpr: RegClass) -> Option<mir::Mem> {
    let mut regs = read.regs.iter().copied().map(|reg| mir::Operand::read(reg, gpr));
    match kind {
        x86_64::Address::BaseIndexScale => {
            let base = regs.next()?;
            let index = regs.next()?;
            Some(mir::Mem::at(base).indexed(index, u8::try_from(read.imm?).ok()?))
        }
        x86_64::Address::IndexScale => Some(mir::Mem {
            base: None,
            index: Some(regs.next()?),
            scale: u8::try_from(read.imm?).ok()?,
            disp: 0,
            symbol: None,
        }),
        x86_64::Address::Base => Some(mir::Mem::at(regs.next()?)),
        // The rule that writes this has a guard saying the constant fits, so a displacement that
        // does not is a rule and a target that disagree rather than a program this cannot compile.
        x86_64::Address::BaseOffset => {
            Some(mir::Mem { disp: i32::try_from(read.imm?).ok()?, ..mir::Mem::at(regs.next()?) })
        }
    }
}

/// The table this selector matches with.
///
/// One target for now, because one target has a rule file. Which table to use becomes a question
/// the moment a second one does, and the answer will be the target the session was given rather
/// than a constant here.
static TABLE: &Table = &crate::select::x86_64::TABLE;

#[cfg(test)]
mod tests {
    use rucc_ir::{
        Builder, CallInfo, Flags, InstData, MemInfo, MemOrder, Restrict, Signature, Type,
    };
    use rucc_regalloc::assign::Env;
    use rucc_target::x86_64::{FRAME, REGS, SYSV};

    use super::*;
    use crate::finish::finish;
    use crate::frame::{Frame, Incoming, Layout};

    /// A function of as many 64 bit parameters as the test wants, and the block they are in.
    fn blank(params: &[Type]) -> (Interner, Func, Block, Vec<Value>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let block = func.create_block();
        let values = params.iter().map(|&ty| func.append_param(block, ty)).collect();
        (names, func, block, values)
    }

    /// An ordinary access: not atomic, and aligned enough that nothing here has an opinion.
    /// Neither field reaches selection, which is the point of saying it once here.
    fn plain() -> MemInfo {
        MemInfo {
            size: 0,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        }
    }

    /// What the allocator is given: every integer register the convention offers except two, held
    /// back so that a move on an edge has somewhere to break a cycle and a spilled value has
    /// somewhere to be read into. Which two does not matter, and holding back the last two the
    /// convention would reach for leaves every expectation below unchanged.
    fn env() -> Env {
        const SCRATCH: [rucc_target::PhysReg; 2] = [x86_64::R10, x86_64::R11];
        let order: Vec<rucc_target::PhysReg> =
            SYSV.int_order.iter().copied().filter(|reg| !SCRATCH.contains(reg)).collect();
        Env::new().with(x86_64::GPR, &order, &SCRATCH)
    }

    /// The machine IR text a function lowers to.
    fn lower(names: &mut Interner, source: &Func) -> String {
        let out = func(source, names, &SYSV).expect("every instruction has a rule");
        mir::print_func(&out.func, names, &REGS)
    }

    #[test]
    fn an_addition_of_two_registers_is_one_instruction() {
        let i32 = Type::int(32);
        let (mut names, mut func, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut func, block);
        build.binary(Opcode::Add, args[0], args[1], Flags::default());

        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_32\n    \
             %1:gpr($rsi) = x64.arg_val_32\n    %2:gpr(reuse 1) = x64.add_rr_32 %0, %1\n}\n"
        );
    }

    #[test]
    fn a_constant_operand_becomes_an_immediate() {
        let i32 = Type::int(32);
        let (mut names, mut func, block, args) = blank(&[i32]);
        let mut build = Builder::new(&mut func, block);
        let seven = build.iconst(i32, 7);
        build.binary(Opcode::Add, args[0], seven, Flags::default());

        // The constant is in the instruction and nothing was written to hold it, which is what
        // materializing one where a register for it is wanted buys.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_32\n    \
             %1:gpr(reuse 1) = x64.add_ri_32 %0, 7\n}\n"
        );
    }

    #[test]
    fn a_constant_too_wide_for_an_immediate_goes_into_a_register() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64]);
        let mut build = Builder::new(&mut func, block);
        let big = build.iconst(i64, i128::from(i32::MAX) + 1);
        build.binary(Opcode::Add, args[0], big, Flags::default());

        // Nobody wrote this fallback down. The rule that takes an immediate has a guard that
        // turns a number this wide down, so it does not fire, and the next way of showing the
        // operand puts it in a register.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_64\n    \
             %1:gpr = x64.mov_ri_64 2147483648\n    %2:gpr(reuse 1) = x64.add_rr_64 %0, %1\n}\n"
        );
    }

    #[test]
    fn an_index_calculation_folds_into_an_address() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64, i64]);
        let mut build = Builder::new(&mut func, block);
        let four = build.iconst(i64, 4);
        let scaled = build.binary(Opcode::Mul, args[1], four, Flags::default());
        build.binary(Opcode::Add, args[0], scaled, Flags::default());

        // Three IR instructions and one machine instruction. The multiply is gone because the
        // rule that matched reached down and took it.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_64\n    \
             %1:gpr($rsi) = x64.arg_val_64\n    %2:gpr = x64.lea_64 [%0 + %1*4]\n}\n"
        );
    }

    #[test]
    fn an_instruction_read_twice_is_not_folded_into_either_reader() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64, i64]);
        let mut build = Builder::new(&mut func, block);
        let four = build.iconst(i64, 4);
        let scaled = build.binary(Opcode::Mul, args[1], four, Flags::default());
        let first = build.binary(Opcode::Add, args[0], scaled, Flags::default());
        build.binary(Opcode::Add, first, scaled, Flags::default());

        // Folding it into both would compute it twice, which is not a saving, so it stays where
        // it is and both readers read the register it wrote.
        let text = lower(&mut names, &func);
        assert!(text.contains("x64.lea_64 [%1*4]"), "{text}");
        assert_eq!(text.matches("x64.add_rr_64").count(), 2, "{text}");
    }

    #[test]
    fn a_shift_by_a_register_asks_for_it_in_cl() {
        let i32 = Type::int(32);
        let (mut names, mut func, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut func, block);
        build.binary(Opcode::Shl, args[0], args[1], Flags::default());

        // The fixed register is not in the rule. It is what the target says the instruction does
        // with its operands, and the allocator is what will act on it.
        let text = lower(&mut names, &func);
        assert!(text.contains("x64.shl_rcl_32 %0, %1($rcx)"), "{text}");
    }

    #[test]
    fn a_division_names_the_registers_and_the_register_it_destroys() {
        let i32 = Type::int(32);
        let (mut names, mut func, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut func, block);
        build.binary(Opcode::SDiv, args[0], args[1], Flags::default());

        // Two definitions, because a division writes the remainder whether anybody wanted it or
        // not, and the second one is early because it is destroyed before the operands are read.
        let text = lower(&mut names, &func);
        assert!(
            text.contains("%2:gpr($rax), early %3:gpr($rdx) = x64.idiv_quo_32 %0($rax), %1"),
            "{text}"
        );
    }

    #[test]
    fn a_load_reads_through_the_register_the_address_is_in() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64]);
        let mut build = Builder::new(&mut func, block);
        build.load(Type::int(32), args[0], plain(), Flags::default());

        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_64\n    \
             %1:gpr = x64.mov_rm_32 [%0]\n}\n"
        );
    }

    #[test]
    fn a_store_writes_no_register_and_the_value_it_writes_is_the_one_the_ir_gave_it() {
        let (mut names, mut func, block, args) = blank(&[Type::int(32), Type::int(64)]);
        let mut build = Builder::new(&mut func, block);
        build.store(args[0], args[1], plain(), Flags::default());

        // The value is the first parameter and the address is the second, and the instruction
        // takes them the other way round. Getting that backwards would compile to a store of the
        // address into the value, which is a program that runs and does the wrong thing.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_32\n    \
             %1:gpr($rsi) = x64.arg_val_64\n    x64.mov_mr_32 %0, [%1]\n}\n"
        );
    }

    #[test]
    fn an_address_with_a_constant_added_folds_into_the_access() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64]);
        let mut build = Builder::new(&mut func, block);
        let twelve = build.iconst(i64, 12);
        let field = build.binary(Opcode::Add, args[0], twelve, Flags::default());
        build.load(Type::int(64), field, plain(), Flags::default());

        // Two IR instructions and one machine instruction, which is what every read of a field
        // of a structure comes to.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_64\n    \
             %1:gpr = x64.mov_rm_64 [%0 + 12]\n}\n"
        );
    }

    #[test]
    fn a_displacement_too_wide_to_encode_leaves_the_addition_where_it_is() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64]);
        let mut build = Builder::new(&mut func, block);
        let big = build.iconst(i64, i128::from(i32::MAX) + 1);
        let far = build.binary(Opcode::Add, args[0], big, Flags::default());
        build.load(Type::int(32), far, plain(), Flags::default());

        // A displacement is signed and 32 bits. The rule that folds one has a guard that turns
        // this down, so the addition stays and the load reads through what it produced. Nobody
        // wrote that fallback: it is the next way of showing the operand.
        let text = lower(&mut names, &func);
        assert!(text.contains("x64.mov_rm_32 [%2]"), "{text}");
        assert!(text.contains("x64.add_rr_64"), "{text}");
    }

    #[test]
    fn a_store_of_a_value_that_was_loaded_is_two_instructions_and_no_arithmetic() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64, i64]);
        let mut build = Builder::new(&mut func, block);
        let got = build.load(Type::int(8), args[0], plain(), Flags::default());
        build.store(got, args[1], plain(), Flags::default());

        // A load feeding a store is the one place folding would be wrong: an x86-64 `mov` has at
        // most one memory operand, and there is no rule that takes two, so the load is left where
        // it is and the store reads the register it wrote.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_64\n    \
             %1:gpr($rsi) = x64.arg_val_64\n    %2:gpr = x64.mov_rm_8 [%0]\n    \
             x64.mov_mr_8 %2, [%1]\n}\n"
        );
    }

    #[test]
    fn an_access_at_a_width_no_rule_is_written_at_is_reported() {
        let i64 = Type::int(64);
        let (mut names, mut source, block, args) = blank(&[i64]);
        let mut build = Builder::new(&mut source, block);
        build.load(Type::int(128), args[0], plain(), Flags::default());

        // The width is the whole of what is wrong here, so the width is in the message: `load`
        // on its own is written about at every other width and would send a reader looking in
        // the wrong place.
        let failed = func(&source, &mut names, &SYSV).expect_err("nothing loads 128 bits");
        assert_eq!(failed.to_string(), "no rule lowers a `load` producing a `i128`");
    }

    #[test]
    fn a_return_asks_for_the_value_in_the_register_the_caller_reads() {
        let (mut names, mut func, block, args) = blank(&[Type::int(32)]);
        let mut build = Builder::new(&mut func, block);
        build.ret(&[args[0]]);

        // The register is not in the rule, the same way `cl` is not in the rule for a shift. It
        // is what the target says the instruction does with its operand, and the allocator is
        // what will act on it. There is no `ret` here, because giving the frame back has to
        // happen between this and leaving and the frame is not worked out yet.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_32\n    \
             x64.ret_val_32 %0($rax)\n}\n"
        );
    }

    #[test]
    fn a_return_of_two_values_asks_for_the_second_register_as_well() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64, i64]);
        let mut build = Builder::new(&mut func, block);
        build.ret(&[args[0], args[1]]);

        // `struct { long a, b; } f(long a, long b)`, after the front end has classified it. Both
        // halves are integers, so the second is in the second integer return register, and both
        // pseudos say so the same way the one for a single value does.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_64\n    \
             %1:gpr($rsi) = x64.arg_val_64\n    x64.ret_val_64 %0($rax)\n    \
             x64.ret_val2_64 %1($rdx)\n}\n"
        );
    }

    #[test]
    fn two_values_back_in_different_files_are_both_the_first_of_their_own() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut func, block, args) = blank(&[f64, Type::int(64)]);
        let mut build = Builder::new(&mut func, block);
        build.ret(&[args[0], args[1]]);

        // `struct { double a; long b; } f(double a, long b)`. The two files are counted apart, so
        // neither half is the second of anything and the `double` is in `xmm0` rather than in the
        // register a second `double` would have been in. Getting this wrong is not a crash: the
        // caller reads a register nobody wrote, and this is where that is ruled out.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:xmm($xmm0) = x64.arg_val_f64\n    \
             %1:gpr($rdi) = x64.arg_val_64\n    x64.ret_val_f64 %0($xmm0)\n    \
             x64.ret_val_64 %1($rax)\n}\n"
        );
    }

    #[test]
    fn two_of_the_same_file_back_take_the_first_two_of_it() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut func, block, args) = blank(&[f64, f64]);
        let mut build = Builder::new(&mut func, block);
        build.ret(&[args[0], args[1]]);

        // `struct { double x, y; } f(double x, double y)`, which is the vector half of the pair
        // above and counts in its own file the same way.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:xmm($xmm0) = x64.arg_val_f64\n    \
             %1:xmm($xmm1) = x64.arg_val_f64\n    x64.ret_val_f64 %0($xmm0)\n    \
             x64.ret_val2_f64 %1($xmm1)\n}\n"
        );
    }

    /// A function whose answer goes back through memory, with the pointer to the space for it in
    /// front of whatever else it takes. Only the signature says it is one.
    fn returning_through_memory(params: &[Type]) -> (Interner, Func, Block, Vec<Value>) {
        let mut names = Interner::new();
        let sret = Abi::Sret { size: 32, align: 8 };
        let mut signature = Signature::new().and_param(Param::with_abi(Type::PTR, sret));
        signature.params.extend(params.iter().copied().map(Param::new));
        let mut func = Func::new(names.intern("f"), signature);
        let block = func.create_block();
        let space = func.append_param(block, Type::PTR);
        let values = std::iter::once(space)
            .chain(params.iter().map(|&ty| func.append_param(block, ty)))
            .collect();
        (names, func, block, values)
    }

    #[test]
    fn the_space_a_return_through_memory_was_given_goes_back_in_the_first_return_register() {
        let (mut names, mut func, block, _) = returning_through_memory(&[]);
        Builder::new(&mut func, block).ret(&[]);

        // `struct big f(void)`, where `big` is too large to come back in registers. The `return`
        // carries nothing, because the value went into the space the caller handed over, and the
        // document still says that address comes back in `rax`. Nothing in the IR says it, so the
        // convention says it, and the pseudo is the one any other pointer return would use.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_64\n    \
             x64.ret_val_64 %0($rax)\n}\n"
        );
    }

    #[test]
    fn what_the_function_did_in_between_does_not_take_the_register_off_it() {
        let (mut names, mut func, block, args) = returning_through_memory(&[Type::int(32)]);
        let mut build = Builder::new(&mut func, block);
        build.store(args[1], args[0], plain(), Flags::default());
        build.ret(&[]);

        // The register is a read at the end and not a move at the start, so it is live across
        // everything between the two and the allocator has to keep it somewhere. In a function
        // with a call in it that somewhere is a callee saved register, and the address comes back
        // into `rax` here rather than whatever the last instruction happened to leave there. That
        // is issue #333, and a store is enough to show the value outlives the entry block.
        let text = lower(&mut names, &func);
        assert!(text.contains("x64.mov_mr_32 %1, [%0]"), "{text}");
        assert!(text.ends_with("    x64.ret_val_64 %0($rax)\n}\n"), "{text}");
    }

    #[test]
    fn a_pointer_that_is_only_a_pointer_is_not_given_back() {
        let (mut names, mut func, block, args) = blank(&[Type::PTR]);
        let mut build = Builder::new(&mut func, block);
        build.store(args[0], args[0], plain(), Flags::default());
        build.ret(&[]);

        // `void f(void **p)`. It takes a pointer first and returns nothing, which is the shape of
        // the one above and none of its meaning, and what tells them apart is the signature. A
        // `void` function leaves `rax` alone.
        assert!(!lower(&mut names, &func).contains("ret_val"));
    }

    #[test]
    fn a_return_of_a_constant_puts_it_in_a_register_first() {
        let (mut names, mut func, block, _) = blank(&[]);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        // No rule returns an immediate, so the plan that offers one is turned down and the next
        // one materializes it. That is `int main(void) { return 0; }` in full, once the epilogue
        // is appended to it.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr = x64.mov_ri_32 0\n    x64.ret_val_32 %0($rax)\n}\n"
        );
    }

    #[test]
    fn the_rule_that_writes_a_constant_down_is_recorded_as_a_rule_that_fired() {
        let (mut names, mut func, block, _) = blank(&[]);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        // The loop over the instructions passes a constant by, because a constant is written where
        // a register for it is first wanted rather than where the IR put it. So the only place a
        // rule about one is ever selected is the materialization, and a mark made in the loop
        // alone would report every rule about a constant as a rule nothing reaches.
        let out = super::func(&func, &mut names, &SYSV).expect("every instruction has a rule");
        let rules = &crate::select::x86_64::TABLE.rules;
        let fired: Vec<&str> = rules
            .iter()
            .enumerate()
            .filter(|(index, _)| out.fired.has(*index))
            .map(|(_, rule)| rule.pattern)
            .collect();
        assert!(fired.contains(&"(iconst.i32 k)"), "{fired:?}");
    }

    #[test]
    fn a_return_of_nothing_is_no_instruction_at_all() {
        let (mut names, mut func, block, _) = blank(&[]);
        let mut build = Builder::new(&mut func, block);
        build.ret(&[]);

        // Every part of leaving a function that returns nothing is the epilogue's, and the
        // epilogue goes in after allocation. A block with nothing in it is the right answer here
        // rather than a function that could not be lowered.
        assert_eq!(lower(&mut names, &func), "mfunc @f {\nblock0:\n}\n");
    }

    #[test]
    fn the_allocator_is_what_moves_the_answer_into_the_return_register() {
        let (mut names, mut source, block, _) = blank(&[]);
        let mut build = Builder::new(&mut source, block);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        let mut out = func(&source, &mut names, &SYSV).expect("every instruction has a rule").func;
        let env = env();
        let allocation = rucc_regalloc::run(&mut out, &env);
        let frame = Frame::of(&out, &allocation, &Layout::new(&SYSV, REGS));
        finish(&mut out, &allocation, &frame, &Stack::default(), &SYSV, &FRAME, &mut names);

        // `int main(void) { return 0; }` end to end. Nothing here asked for `rax`: the rule said
        // the value goes back, the target said where, and the allocator is what made it true. The
        // epilogue is what leaves, and this function needs no frame, so it is the return alone.
        //
        // Two instructions and no copy, which is what a hint buys. The return insists on `rax`,
        // so `rax` is the register the allocator tries first for the value the return reads, and
        // the constant is written straight into it.
        assert_eq!(
            mir::print_func(&out, &names, &REGS),
            "mfunc @f {\nblock0:\n    $rax = x64.mov_ri_32 0\n    \
             x64.ret_val_32 $rax($rax)\n    x64.ret\n}\n"
        );
    }

    #[test]
    fn a_function_of_two_arguments_is_a_whole_function_now() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::Add, args[0], args[1], Flags::default());
        build.ret(&[sum]);

        let mut out = func(&source, &mut names, &SYSV).expect("every instruction has a rule").func;
        let env = env();
        let allocation = rucc_regalloc::run(&mut out, &env);
        let frame = Frame::of(&out, &allocation, &Layout::new(&SYSV, REGS));
        finish(&mut out, &allocation, &frame, &Stack::default(), &SYSV, &FRAME, &mut names);

        // `int f(int a, int b) { return a + b; }` end to end, and this is the test the argument
        // side exists for. Before it there was no way to write one: the allocator refuses a
        // function whose entry block takes parameters, because there is no edge into an entry
        // block for the moves that give a block parameter its value to go on.
        //
        // One move, and it is the one the machine's addition needs rather than one the allocator
        // owes anybody. Each argument stays in the register it arrived in, because the pseudo
        // that defines it insists on that register and the allocator now tries it first, and the
        // sum stays in the register the addition wrote it to until the return reads it out. The
        // copy in front of a two address instruction is what makes its destination one of the
        // registers it reads, and the source operand keeps its own name because the destination
        // is what the encoder writes.
        assert_eq!(
            mir::print_func(&out, &names, &REGS),
            "mfunc @f {\nblock0:\n    $rdi($rdi) = x64.arg_val_32\n    \
             $rsi($rsi) = x64.arg_val_32\n    \
             $rdi(reuse 1) = x64.add_rr_32 $rdi, $rsi\n    $rax = x64.mov_rr_64 $rdi\n    \
             x64.ret_val_32 $rax($rax)\n    x64.ret\n}\n"
        );
    }

    #[test]
    fn an_argument_with_no_register_left_for_it_is_read_out_of_the_caller_s_stack() {
        let i64 = Type::int(64);
        let (mut names, mut source, block, args) = blank(&[i64; 7]);
        let mut build = Builder::new(&mut source, block);
        build.ret(&[args[6]]);

        let lowered = func(&source, &mut names, &SYSV).expect("the seventh is read from memory");

        // SysV passes six integers in registers and the seventh in the caller's memory, so six of
        // these are pseudos that encode to nothing and the seventh is a load that encodes to real
        // bytes. Its displacement is nothing here for the reason a local's is: there is no frame
        // yet. What the walk hands on is which instruction is waiting, and for how far up the
        // caller's argument area, which is the bottom of it because it is the first one there.
        assert_eq!(lowered.stack.arguments.len(), 1);
        assert_eq!(lowered.stack.arguments[0].1, 0);
        let text = mir::print_func(&lowered.func, &names, &REGS);
        assert!(text.contains("%6:gpr = x64.mov_rm_64 [$rsp]"), "{text}");
        assert_eq!(text.matches("x64.arg_val_64").count(), 6, "{text}");
    }

    #[test]
    fn the_frame_is_what_says_how_far_up_the_caller_s_stack_an_argument_is() {
        let i64 = Type::int(64);
        let (mut names, mut source, block, args) = blank(&[i64; 8]);
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::Add, args[6], args[7], Flags::default());
        build.ret(&[sum]);

        let lowered = func(&source, &mut names, &SYSV).expect("both are read from memory");
        let stack = lowered.stack;
        let mut out = lowered.func;
        let env = env();
        let allocation = rucc_regalloc::run(&mut out, &env);
        let layout = stack.layout(Layout::new(&SYSV, REGS));
        let frame = Frame::of(&out, &allocation, &layout);
        finish(&mut out, &allocation, &frame, &stack, &SYSV, &FRAME, &mut names);

        // A leaf that takes no frame, so the stack pointer never moves and the only thing between
        // it and the caller's arguments is the return address the call pushed. The seventh
        // parameter is at the bottom of the caller's argument area and the eighth is one word
        // further up, which is the eight bytes between the two offsets.
        let text = mir::print_func(&out, &names, &REGS);
        assert_eq!(frame.size(), 0);
        assert_eq!(frame.incoming(), Incoming::from_stack(8));
        assert!(text.contains("x64.mov_rm_64 [$rsp + 8]"), "{text}");
        assert!(text.contains("x64.mov_rm_64 [$rsp + 16]"), "{text}");
    }

    #[test]
    fn a_realigned_frame_reaches_the_caller_s_arguments_through_the_frame_pointer() {
        let i64 = Type::int(64);
        let (mut names, mut source, block, args) = blank(&[i64; 7]);
        let wide = slot(&mut source, block, 64, 32);
        let mut build = Builder::new(&mut source, block);
        build.store(args[6], wide, plain(), Flags::default());
        build.ret(&[args[6]]);

        let lowered = func(&source, &mut names, &SYSV).expect("every instruction has a rule");
        let stack = lowered.stack;
        let mut out = lowered.func;
        let env = env();
        let allocation = rucc_regalloc::run(&mut out, &env);
        let layout = stack.layout(Layout::new(&SYSV, REGS));
        let frame = Frame::of(&out, &allocation, &layout);
        finish(&mut out, &allocation, &frame, &stack, &SYSV, &FRAME, &mut names);

        // A local wanting thirty two byte alignment makes the prologue force the stack pointer,
        // which throws away how far the caller's stack was. So the load the lowering wrote off the
        // stack pointer is rewritten to read through the frame pointer, at the one distance that
        // survives: the word the prologue pushed the frame pointer into, and the return address
        // above it.
        let text = mir::print_func(&out, &names, &REGS);
        assert_eq!(frame.realign(), Some(32));
        assert_eq!(frame.incoming(), Incoming::from_frame(16));
        assert!(text.contains("x64.mov_rm_64 [$rbp + 16]"), "{text}");
        assert!(!text.contains("x64.mov_rm_64 [$rsp"), "{text}");
    }

    #[test]
    fn a_jump_is_the_edge_and_nothing_else() {
        let i32 = Type::int(32);
        let (mut names, mut source, entry, args) = blank(&[i32]);
        let next = source.create_block();
        let got = source.append_param(next, i32);
        Builder::new(&mut source, entry).jump(next, &[args[0]]);
        Builder::new(&mut source, next).ret(&[got]);

        // Two blocks and two instructions, and the jump is neither of them. What it was is the
        // arm on the first block, and what the arm carries is the argument it was called with.
        assert_eq!(
            lower(&mut names, &source),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_32 block1(%0)\n\n\
             block1(%1:gpr):\n    x64.ret_val_32 %1($rax)\n}\n"
        );
    }

    /// A constant is written where it is wanted rather than where the IR defined it, and two
    /// blocks wanting the same one is two places. Writing it once and reading it in both is a
    /// register read where nothing wrote it, unless the block it was written in happens to
    /// dominate the other, which nothing here checks and which the second arm of a branch never
    /// does. Each block gets its own copy of the number instead.
    #[test]
    fn a_constant_two_blocks_want_is_written_in_both_of_them() {
        let i32 = Type::int(32);
        let (mut names, mut source, entry, args) = blank(&[i32, i32]);
        let then = source.create_block();
        let other = source.create_block();
        let join = source.create_block();
        let got = source.append_param(join, i32);

        let mut build = Builder::new(&mut source, entry);
        let seven = build.iconst(i32, 7);
        let cond = build.icmp(rucc_ir::IntPred::Slt, args[0], args[1]);
        build.br_if(cond, then, &[], other, &[]);
        // Both arms want the seven in a register, because a block argument is never an immediate,
        // and neither arm dominates the other.
        Builder::new(&mut source, then).jump(join, &[seven]);
        Builder::new(&mut source, other).jump(join, &[seven]);
        Builder::new(&mut source, join).ret(&[got]);

        let text = lower(&mut names, &source);
        assert_eq!(text.matches("x64.mov_ri_32 7").count(), 2, "one seven per block: {text}");
    }

    /// An argument on an edge out of a block that leaves two ways is read after every instruction
    /// of the block is written, and reading one can write an instruction, which would land after
    /// the branch that has already jumped past it. The branch goes back on the end.
    #[test]
    fn a_constant_an_edge_wants_is_written_before_the_branch_and_not_after_it() {
        let i32 = Type::int(32);
        let (mut names, mut source, entry, args) = blank(&[i32, i32]);
        let then = source.create_block();
        let join = source.create_block();
        let got = source.append_param(join, i32);

        let mut build = Builder::new(&mut source, entry);
        let nine = build.iconst(i32, 9);
        let cond = build.icmp(rucc_ir::IntPred::Slt, args[0], args[1]);
        build.br_if(cond, then, &[], join, &[nine]);
        Builder::new(&mut source, then).jump(join, &[args[0]]);
        Builder::new(&mut source, join).ret(&[got]);

        let out = func(&source, &mut names, &SYSV).expect("every instruction has a rule").func;
        let entry = out.entry().expect("an entry block");
        let last = out.terminator(entry).expect("a block that leaves two ways has a branch");
        let branch = names.intern("x64.br_cond_8");
        assert_eq!(
            out[last].opcode,
            mir::Opcode::new(branch),
            "the branch is last: {}",
            mir::print_func(&out, &names, &REGS)
        );
    }

    #[test]
    fn a_conditional_branch_is_lowered_to_the_condition_and_nothing_about_where_it_goes() {
        let i32 = Type::int(32);
        let (mut names, mut source, entry, args) = blank(&[i32, i32]);
        let then = source.create_block();
        let other = source.create_block();
        let mut build = Builder::new(&mut source, entry);
        let cond = build.icmp(rucc_ir::IntPred::Slt, args[0], args[1]);
        build.br_if(cond, then, &[], other, &[]);
        Builder::new(&mut source, then).ret(&[args[0]]);
        Builder::new(&mut source, other).ret(&[args[1]]);

        // The comparison writes a byte and the branch reads it, and neither says a block. Both
        // arms are on the entry block, in the order the branch took them, so the arm that runs
        // when the condition holds is the first.
        assert_eq!(
            lower(&mut names, &source),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_32\n    \
             %1:gpr($rsi) = x64.arg_val_32\n    %2:gpr = x64.cmp_set_l_32 %0, %1\n    \
             x64.br_cond_8 %2, block1, block2\n\n\
             block1:\n    x64.ret_val_32 %0($rax)\n\n\
             block2:\n    x64.ret_val_32 %1($rax)\n}\n"
        );
    }

    /// A choice between two values, which is one instruction and no blocks at all.
    ///
    /// The arms come out the other way round from the IR, because a conditional move overwrites its
    /// destination and the destination is the arm taken when the condition does not hold. The
    /// condition arrives last for the same reason: it is read by the test in front of the move
    /// rather than by the move.
    #[test]
    fn a_select_is_lowered_to_a_test_and_a_conditional_move() {
        let i32 = Type::int(32);
        let (mut names, mut source, entry, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut source, entry);
        let cond = build.icmp(rucc_ir::IntPred::Slt, args[0], args[1]);
        let picked = build.select(cond, args[0], args[1]);
        build.ret(&[picked]);

        assert_eq!(
            lower(&mut names, &source),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_32\n    \
             %1:gpr($rsi) = x64.arg_val_32\n    %2:gpr = x64.cmp_set_l_32 %0, %1\n    \
             %3:gpr(reuse 1) = x64.test_cmov_ne_32 %1, %0, %2\n    \
             x64.ret_val_32 %3($rax)\n}\n"
        );
    }

    #[test]
    fn a_branch_over_a_block_is_a_whole_function_now() {
        let i32 = Type::int(32);
        let (mut names, mut source, entry, args) = blank(&[i32, i32]);
        let then = source.create_block();
        let other = source.create_block();
        let join = source.create_block();
        let got = source.append_param(join, i32);
        let mut build = Builder::new(&mut source, entry);
        let cond = build.icmp(rucc_ir::IntPred::Slt, args[0], args[1]);
        build.br_if(cond, then, &[], other, &[]);
        let mut build = Builder::new(&mut source, then);
        let sum = build.binary(Opcode::Add, args[0], args[1], Flags::default());
        build.jump(join, &[sum]);
        Builder::new(&mut source, other).jump(join, &[args[1]]);
        Builder::new(&mut source, join).ret(&[got]);

        // `int f(int a, int b) { if (a < b) return a + b; else return b; }` end to end, written
        // the way a front end writes it: both arms of the branch are blocks of their own and the
        // return is the block they meet at. No edge here is critical, because the two arms out of
        // the entry carry nothing and the two arms into the join each leave a block that goes
        // nowhere else, so each has its own end to put its move at.
        let mut out = func(&source, &mut names, &SYSV).expect("every instruction has a rule").func;
        assert_eq!(crate::split::critical(&mut out), 0, "no edge here is critical");
        let env = env();
        let allocation = rucc_regalloc::run(&mut out, &env);
        let frame = Frame::of(&out, &allocation, &Layout::new(&SYSV, REGS));
        finish(&mut out, &allocation, &frame, &Stack::default(), &SYSV, &FRAME, &mut names);

        // One epilogue, on the join, which is the one block the function leaves from, and the
        // moves that give the join its parameter are at the end of each arm. Every register is
        // physical and the branch is still a branch on a register, because turning it into a
        // `test` and a `jcc` is the block layout's and there is no block layout yet.
        let text = mir::print_func(&out, &names, &REGS);
        assert_eq!(text.matches("x64.ret\n").count(), 1, "{text}");
        assert!(text.contains("x64.br_cond_8"), "{text}");
        assert!(text.contains("x64.add_rr_32"), "{text}");
        assert!(!text.contains('%'), "{text}");
    }

    #[test]
    fn a_critical_edge_is_split_before_the_allocator_ever_sees_it() {
        let i32 = Type::int(32);
        let (mut names, mut source, entry, args) = blank(&[i32, i32]);
        let then = source.create_block();
        let join = source.create_block();
        let got = source.append_param(join, i32);
        let mut build = Builder::new(&mut source, entry);
        let cond = build.icmp(rucc_ir::IntPred::Slt, args[0], args[1]);
        build.br_if(cond, then, &[], join, &[args[1]]);
        Builder::new(&mut source, then).jump(join, &[args[0]]);
        let mut build = Builder::new(&mut source, join);
        let twice = build.binary(Opcode::Add, got, got, Flags::default());
        build.ret(&[twice]);

        // The else arm is critical: the entry block leaves two ways and the join is arrived at
        // two ways, and the arm carries a value. Without splitting it the allocator asserts,
        // because the move that gives the join its parameter would have to run at the end of a
        // block that also goes to the other arm.
        let mut out = func(&source, &mut names, &SYSV).expect("every instruction has a rule").func;
        assert_eq!(crate::split::critical(&mut out), 1);
        let env = env();
        let allocation = rucc_regalloc::run(&mut out, &env);
        let frame = Frame::of(&out, &allocation, &Layout::new(&SYSV, REGS));
        finish(&mut out, &allocation, &frame, &Stack::default(), &SYSV, &FRAME, &mut names);

        // The block the split added is where the move went, and it is the whole of that block.
        let text = mir::print_func(&out, &names, &REGS);
        assert_eq!(out.block_count(), 4, "{text}");
        assert_eq!(text.matches("x64.ret\n").count(), 1, "{text}");
    }

    #[test]
    fn a_call_passes_what_the_convention_says_and_takes_back_what_it_says() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32, i32]);
        let sig =
            source.add_signature(Signature::new().with_params(&[i32, i32]).with_returns(&[i32]));
        let callee = names.intern("g");
        let call = Builder::new(&mut source, block).call(callee, sig, &[args[0], args[1]]);
        let got = source[call].first_result.expect("an integer comes back");
        Builder::new(&mut source, block).ret(&[got]);

        // `int f(int a, int b) { return g(a, b); }`. The arguments arrived where the call wants
        // them, so what the call reads is what arrived, and the whole of the convention is in the
        // constraints rather than in a move.
        let text = lower(&mut names, &source);
        assert!(text.contains("= x64.call %0($rdi), %1($rsi), @g"), "{text}");
        assert!(text.contains("x64.ret_val_32 %2($rax)"), "{text}");
        // What the call writes is the value that comes back and then every register the callee is
        // free to destroy, in both classes, which is the whole of what stops the allocator from
        // leaving something in one of them.
        assert!(text.contains("%2:gpr($rax), $rcx, $rdx, $r8, $r9, $r10, $r11, $xmm0,"), "{text}");
        assert!(text.contains("$xmm15 = x64.call"), "{text}");
    }

    #[test]
    fn what_the_frame_owes_a_call_comes_back_with_the_function() {
        let i32 = Type::int(32);
        let sig = |source: &mut Func| source.add_signature(Signature::new().with_params(&[i32]));

        let (mut names, mut source, block, args) = blank(&[i32]);
        let sig = sig(&mut source);
        let callee = names.intern("g");
        Builder::new(&mut source, block).call(callee, sig, &[args[0]]);
        let out = func(&source, &mut names, &SYSV).expect("every instruction has a rule");

        // Nothing on the stack, so nothing owed, but not a leaf either: a function that calls
        // owes the callee an aligned stack pointer and may not use the red zone.
        assert_eq!(out.stack.calls, Some(0));
        let layout = out.stack.layout(Layout::new(&SYSV, REGS));
        assert!(!layout.leaf);
        assert_eq!(layout.outgoing, 0);

        // The same call under the other convention owes thirty two bytes for the callee to spill
        // its register arguments into, which is a fact about the convention and not about the call.
        let out = func(&source, &mut names, &x86_64::WIN64).expect("every instruction has a rule");
        assert_eq!(out.stack.calls, Some(32));

        // And a function that calls nothing is a leaf, which is what says it may use the red zone.
        let (mut names, mut source, block, args) = blank(&[i32]);
        Builder::new(&mut source, block).ret(&[args[0]]);
        let out = func(&source, &mut names, &SYSV).expect("every instruction has a rule");
        assert_eq!(out.stack.calls, None);
        assert!(out.stack.layout(Layout::new(&SYSV, REGS)).leaf);
    }

    #[test]
    fn a_value_that_outlives_a_call_is_not_left_where_the_call_destroys_it() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32]);
        let sig = source.add_signature(Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let callee = names.intern("g");
        let call = Builder::new(&mut source, block).call(callee, sig, &[args[0]]);
        let got = source[call].first_result.expect("an integer comes back");
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::Add, got, args[0], Flags::default());
        build.ret(&[sum]);

        // `int f(int a) { return g(a) + a; }`, which is the smallest program that asks the
        // question: `a` is read after the call and `rdi` is a register the call destroys.
        let lowered = func(&source, &mut names, &SYSV).expect("every instruction has a rule");
        let layout = lowered.stack.layout(Layout::new(&SYSV, REGS));
        let mut out = lowered.func;
        let env = env();
        let allocation = rucc_regalloc::run(&mut out, &env);
        let frame = Frame::of(&out, &allocation, &layout);
        finish(&mut out, &allocation, &frame, &Stack::default(), &SYSV, &FRAME, &mut names);

        // It went to a register the callee has to put back, and the prologue and epilogue are what
        // put it back, which is the whole bargain the two halves of a convention make.
        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("$rbx"), "{text}");
        assert!(!text.contains('%'), "{text}");
        assert_eq!(text.matches("x64.call").count(), 1, "{text}");
    }

    #[test]
    fn a_call_with_more_arguments_than_registers_writes_the_rest_into_the_outgoing_area() {
        let i64 = Type::int(64);
        let (mut names, mut source, block, args) = blank(&[i64]);
        let seven = vec![i64; 7];
        let sig = source.add_signature(Signature::new().with_params(&seven));
        let callee = names.intern("g");
        let passed = vec![args[0]; 7];
        Builder::new(&mut source, block).call(callee, sig, &passed);

        let lowered = func(&source, &mut names, &SYSV).expect("the seventh goes to memory");
        // The bytes the call needs are on the layout the frame is worked out from, so that the
        // frame reserves as many as the widest call in the function asked for.
        assert_eq!(lowered.stack.calls, Some(8));
        let text = mir::print_func(&lowered.func, &names, &REGS);
        assert!(text.contains("x64.mov_mr_64 %0, [$rsp]\n"), "{text}");
    }

    #[test]
    fn a_call_this_cannot_make_is_reported_rather_than_made() {
        let (mut names, mut source, block, _) = blank(&[]);
        let returns = [Type::float(rucc_ir::Float::F80), Type::int(64)];
        let sig = source.add_signature(Signature::new().with_returns(&returns));
        let callee = names.intern("g");
        Builder::new(&mut source, block).call(callee, sig, &[]);
        let failed = func(&source, &mut names, &SYSV).expect_err("a long double is on the x87");
        assert_eq!(failed.to_string(), "what this call gives back is on the x87 stack");
    }

    /// A `long double` on its own is a different answer, because on its own it comes back on the
    /// x87 stack rather than in a register, which is somewhere the call cannot be said to write.
    ///
    /// So the call gives back nothing at all and the value is taken off the stack by the `fstp`
    /// straight after it. That instruction has to be straight after it: the stack is one place and
    /// anything else that touched it before this ran would be looking at the value still on it.
    #[test]
    fn a_call_that_gives_back_a_long_double_takes_it_off_the_stack_at_once() {
        let (mut names, mut source, block, _) = blank(&[]);
        let long_double = Type::float(rucc_ir::Float::F80);
        let sig = source.add_signature(Signature::new().with_returns(&[long_double]));
        let callee = names.intern("g");
        Builder::new(&mut source, block).call(callee, sig, &[]);

        let lowered = func(&source, &mut names, &SYSV).expect("the value comes back in st0");
        let text = mir::print_func(&lowered.func, &names, &REGS);
        let after: Vec<&str> =
            text.lines().skip_while(|line| !line.contains("x64.call")).skip(1).collect();
        assert_eq!(after[0].trim(), "%0:gpr = x64.lea_64 [$rsp]", "{text}");
        assert_eq!(after[1].trim(), "x64.fstp_t [%0]", "{text}");
        // And the slot it went into is the sixteen bytes the type takes, like every other one.
        assert_eq!(lowered.stack.locals.len(), 1, "{text}");
        assert_eq!(lowered.stack.locals[0].size, X87_BYTES);
    }

    #[test]
    fn a_call_through_an_address_goes_through_the_register_the_address_is_in() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[Type::PTR, i32]);
        let sig = source.add_signature(Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let varargs = source.push_abis(&[]);
        let info = source.add_call(CallInfo { callee: None, signature: sig, varargs });
        let mut build = Builder::new(&mut source, block);
        let inst = InstData {
            args: build.func().push_values(&[args[0], args[1]]),
            extra: Extra::Call(info),
            ..InstData::new(Opcode::CallIndirect)
        };
        let called = build.inst(inst, &[i32]);
        let got = source[called].first_result.expect("an integer comes back");
        Builder::new(&mut source, block).ret(&[got]);

        // `int f(int (*g)(int), int a) { return g(a); }`. The first operand is the address and
        // the arguments are the ones behind it, and everything else about the call is what a call
        // to a name would have been.
        let text = lower(&mut names, &source);
        assert!(text.contains("= x64.call_reg %0, %1($rdi)"), "{text}");
        assert!(text.contains("x64.ret_val_32 %2($rax)"), "{text}");
        assert!(!text.contains("@g"), "a call through an address names nobody: {text}");
    }

    #[test]
    fn an_instruction_no_rule_covers_is_reported() {
        let (mut names, mut source, block, args) = blank(&[Type::PTR]);
        let mut build = Builder::new(&mut source, block);
        let operands = build.func().push_values(&[args[0]]);
        build.inst(InstData { args: operands, ..InstData::new(Opcode::Prefetch) }, &[]);

        // A hint about an address, which nothing writes an instruction for yet. Nothing about it
        // is a width or a register, so there is nothing for the message to add beyond the name.
        let failed = func(&source, &mut names, &SYSV).expect_err("no rule writes a prefetch");
        assert_eq!(failed.to_string(), "no rule lowers a `prefetch`");

        // A `prefetch` produces nothing, so there is no type in the message and nothing invents
        // one, and the instruction comes back so a caller can ask the function where it was.
        let inst = failed.inst().expect("the instruction it is about");
        assert_eq!(source[inst].opcode, Opcode::Prefetch);
    }

    /// A barrier is written by name here, and what it is depends on the ordering and on nothing
    /// else. `crate::expand` is where the reasoning about this machine's memory model lives.
    #[test]
    fn a_barrier_is_one_instruction_at_the_strongest_ordering_and_none_below_it() {
        for order in MemOrder::all().filter(|&order| order != MemOrder::NotAtomic) {
            let (mut names, mut source, block, _) = blank(&[]);
            let mut build = Builder::new(&mut source, block);
            build
                .inst(InstData { extra: Extra::Order(order), ..InstData::new(Opcode::Fence) }, &[]);

            let text = lower(&mut names, &source);
            assert_eq!(text.contains("x64.mfence"), order == MemOrder::SeqCst, "{order:?}: {text}");
        }
    }

    #[test]
    fn more_values_back_than_the_convention_has_registers_for_is_reported() {
        let i64 = Type::int(64);
        let (mut names, mut source, block, args) = blank(&[i64, i64, i64]);
        let mut build = Builder::new(&mut source, block);
        build.ret(&[args[0], args[1], args[2]]);

        // Two integers come back in `rax` and `rdx` and a third has nowhere to go, which is not a
        // gap in the rules but the convention saying no. The front end classifies before it gets
        // here, so this is the shape that would mean the classification went wrong.
        let failed = func(&source, &mut names, &SYSV).expect_err("only two come back");
        assert_eq!(
            failed.to_string(),
            "what this function gives back takes more registers than this convention has for it"
        );

        let inst = failed.inst().expect("the instruction it is about");
        assert_eq!(source[inst].opcode, Opcode::Return);
    }

    /// A refusal about a signature has no instruction, which is what makes it the one arm apart.
    ///
    /// Everything else is about something written somewhere in the body and hands it back so a
    /// caller can ask the function where it came from. A parameter arrives before the first
    /// instruction runs, so there is nothing in the body to point at and the message is about
    /// the function.
    #[test]
    fn a_refusal_about_a_parameter_has_no_instruction_to_point_at() {
        let missing = Unsupported::Argument { index: 0, missing: Missing::OnX87 };
        assert_eq!(missing.inst(), None);
    }

    /// An `alloca` of a fixed size, which is what every local whose address is taken becomes.
    fn slot(source: &mut Func, block: Block, size: u64, align: u32) -> Value {
        let info = MemInfo { size, align, ..plain() };
        let mut build = Builder::new(source, block);
        let mem = build.func().add_mem(info);
        build.value(InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    #[test]
    fn a_local_is_memory_in_the_frame_and_one_instruction_that_says_where() {
        let (mut names, mut source, block, _) = blank(&[]);
        let slot = slot(&mut source, block, 4, 4);
        let mut build = Builder::new(&mut source, block);
        let nine = build.iconst(Type::int(32), 9);
        build.store(nine, slot, plain(), Flags::default());
        let loaded = build.load(Type::int(32), slot, plain(), Flags::default());
        build.ret(&[loaded]);

        let lowered = func(&source, &mut names, &SYSV).expect("every instruction has a rule");

        // Four bytes on the list the frame is laid out from, and the one instruction that reads
        // where they went. Its displacement is nothing here because there is no frame yet, and
        // which instruction is waiting for which local is what `finish` is handed.
        assert_eq!(lowered.stack.locals, vec![Local { size: 4, align: 4 }]);
        assert_eq!(lowered.stack.addresses.len(), 1);
        assert_eq!(lowered.stack.addresses[0].1, 0);
        assert_eq!(
            mir::print_func(&lowered.func, &names, &REGS),
            "mfunc @f {\nblock0:\n    %0:gpr = x64.lea_64 [$rsp]\n    \
             %1:gpr = x64.mov_ri_32 9\n    x64.mov_mr_32 %1, [%0]\n    \
             %2:gpr = x64.mov_rm_32 [%0]\n    x64.ret_val_32 %2($rax)\n}\n"
        );
    }

    #[test]
    fn the_frame_is_what_fills_the_address_of_a_local_in() {
        let (mut names, mut source, block, _) = blank(&[]);
        let slot = slot(&mut source, block, 4, 4);
        let mut build = Builder::new(&mut source, block);
        let nine = build.iconst(Type::int(32), 9);
        build.store(nine, slot, plain(), Flags::default());
        let loaded = build.load(Type::int(32), slot, plain(), Flags::default());
        build.ret(&[loaded]);

        let lowered = func(&source, &mut names, &SYSV).expect("every instruction has a rule");
        let stack = lowered.stack;
        let mut out = lowered.func;
        let env = env();
        let allocation = rucc_regalloc::run(&mut out, &env);
        let layout = stack.layout(Layout::new(&SYSV, REGS));
        let frame = Frame::of(&out, &allocation, &layout);
        finish(&mut out, &allocation, &frame, &stack, &SYSV, &FRAME, &mut names);

        // `int f(void) { int x; x = 9; return x; }` with the address of `x` taken, end to end.
        // A leaf small enough to live in the red zone takes no frame at all, so the stack pointer
        // never moves and the four bytes are below it, which is what the negative offset is. The
        // instruction the lowering left with nothing in its displacement now has the answer in it.
        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("$rax = x64.lea_64 [$rsp - 8]"), "{text}");
        assert!(!text.contains("x64.sub_ri_64"), "{text}");
        assert_eq!(frame.size(), 0);
        assert_eq!(frame.local(0), Some(-8));
    }

    #[test]
    fn a_stack_slot_whose_size_is_not_known_until_it_runs_is_reported() {
        let i64 = Type::int(64);
        let (mut names, mut source, block, args) = blank(&[i64]);
        let info = MemInfo { size: 0, align: 16, ..plain() };
        let mut build = Builder::new(&mut source, block);
        let mem = build.func().add_mem(info);
        let size = build.func().push_values(&[args[0]]);
        let slot = build.value(
            InstData { args: size, extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) },
            Type::PTR,
        );
        Builder::new(&mut source, block).ret(&[slot]);

        // A variable length array. Growing the stack where the declaration stands means moving the
        // stack pointer in the middle of the function and reaching everything else through a
        // frame pointer afterwards, and the frame here lays out neither.
        let failed = func(&source, &mut names, &SYSV).expect_err("nothing grows the stack");
        assert_eq!(failed.to_string(), "nothing here grows the stack for a variable length array");
    }

    #[test]
    fn an_address_is_read_written_and_added_to_like_the_integer_it_is() {
        let (mut names, mut source, block, args) = blank(&[Type::PTR, Type::int(64)]);
        let mut build = Builder::new(&mut source, block);
        let stepped = build.func().push_values(&[args[0], args[1]]);
        let next =
            build.value(InstData { args: stepped, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        let loaded = build.load(Type::int(32), next, plain(), Flags::default());
        build.ret(&[loaded]);

        // `int f(int *p, long i) { return *(int *)((char *)p + i); }`. Nothing about this is new
        // in the rule set, which is the point: the two addresses arrive in registers because an
        // address is an integer as wide as one, and the arithmetic on them is the add it always
        // was, so every rule written about an add reaches it.
        //
        // The add stays its own instruction rather than folding into the address the load reads
        // from. Two registers with no scale on either is the one addressing mode the rules have no
        // load through, because the folds that exist are the displacement one and the scaled ones,
        // and this is neither. That is a peephole worth having and not a thing this changes.
        assert_eq!(
            lower(&mut names, &source),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_64\n    \
             %1:gpr($rsi) = x64.arg_val_64\n    %2:gpr(reuse 1) = x64.add_rr_64 %0, %1\n    \
             %3:gpr = x64.mov_rm_32 [%2]\n    x64.ret_val_32 %3($rax)\n}\n"
        );
    }

    /// The address of a file scope name, which is what every use of a global and every string
    /// literal starts from.
    fn address_of(source: &mut Func, block: Block, names: &mut Interner, name: &str) -> Value {
        let symbol = names.intern(name);
        let mut build = Builder::new(source, block);
        build.value(
            InstData { extra: Extra::Symbol(symbol), ..InstData::new(Opcode::GlobalAddr) },
            Type::PTR,
        )
    }

    #[test]
    fn the_address_of_a_name_is_one_instruction_carrying_the_name() {
        let (mut names, mut source, block, _) = blank(&[]);
        let counter = address_of(&mut source, block, &mut names, "counter");
        let mut build = Builder::new(&mut source, block);
        let loaded = build.load(Type::int(32), counter, plain(), Flags::default());
        build.ret(&[loaded]);

        // `extern int counter; int f(void) { return counter; }`. The address is an addressing mode
        // that names no register and carries the symbol, which is what the assembler writes
        // relative to `%rip` and what the object writer leaves a relocation for.
        assert_eq!(
            lower(&mut names, &source),
            "mfunc @f {\nblock0:\n    %0:gpr = x64.lea_64 [@counter]\n    \
             %1:gpr = x64.mov_rm_32 [%0]\n    x64.ret_val_32 %1($rax)\n}\n"
        );
    }

    /// A cast between a pointer and an integer, at whatever width the result is asked for.
    fn cast(source: &mut Func, block: Block, opcode: Opcode, from: Value, to: Type) -> Value {
        let mut build = Builder::new(source, block);
        let args = build.func().push_values(&[from]);
        build.value(InstData { args, ..InstData::new(opcode) }, to)
    }

    #[test]
    fn a_cast_between_a_pointer_and_an_integer_as_wide_is_no_instruction_at_all() {
        let (mut names, mut source, block, args) = blank(&[Type::PTR]);
        let number = cast(&mut source, block, Opcode::PtrToInt, args[0], Type::int(64));
        Builder::new(&mut source, block).ret(&[number]);

        // `long f(void *p) { return (long)p; }`. An address on this machine is an integer as wide
        // as the machine addresses, so the cast changes what the type system calls the value and
        // changes nothing about the value, and the register holding it is the one that held it.
        assert_eq!(
            lower(&mut names, &source),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_64\n    \
             x64.ret_val_64 %0($rax)\n}\n"
        );
    }

    #[test]
    fn a_null_pointer_is_a_constant_that_reaches_a_register_before_anything_reads_it() {
        let (mut names, mut source, block, _) = blank(&[]);
        let mut build = Builder::new(&mut source, block);
        let zero = build.iconst(Type::int(64), 0);
        let null = cast(&mut source, block, Opcode::IntToPtr, zero, Type::PTR);
        Builder::new(&mut source, block).ret(&[null]);

        // `void *f(void) { return 0; }`. The cast is nothing, and reading its operand is what
        // writes the zero down: a constant is materialized where it is wanted rather than where
        // the IR defined it, and without the read there would be no instruction at all.
        assert_eq!(
            lower(&mut names, &source),
            "mfunc @f {\nblock0:\n    %0:gpr = x64.mov_ri_64 0\n    x64.ret_val_64 %0($rax)\n}\n"
        );
    }

    #[test]
    fn the_five_linkages_the_ir_has_narrow_to_the_three_an_object_file_can_say() {
        let readings = [
            (Linkage::External, mir::Binding::Global),
            (Linkage::Common, mir::Binding::Global),
            (Linkage::Internal, mir::Binding::Local),
            (Linkage::Weak, mir::Binding::Weak),
            (Linkage::LinkOnce, mir::Binding::Weak),
        ];
        for (linkage, wanted) in readings {
            let (mut names, mut source, block, _) = blank(&[]);
            source.linkage = linkage;
            Builder::new(&mut source, block).ret(&[]);
            let out = func(&source, &mut names, &SYSV).expect("a return");
            // The narrowing is done here rather than where the object is written, because a
            // machine function is all the assembler and the writer are ever handed.
            assert_eq!(out.func.binding, wanted, "{linkage:?}");
        }
    }

    #[test]
    fn a_cast_between_a_pointer_and_a_narrower_integer_is_reported() {
        let (mut names, mut source, block, args) = blank(&[Type::PTR]);
        let number = cast(&mut source, block, Opcode::PtrToInt, args[0], Type::int(32));
        Builder::new(&mut source, block).ret(&[number]);

        // The front end never writes one: it casts at the address width and truncates or extends
        // around it, so both of those are the rules they always were. IR from somewhere else that
        // does write one is refused rather than compiled to a move that keeps the high half.
        let failed = func(&source, &mut names, &SYSV).expect_err("no rule narrows an address");
        assert_eq!(failed.to_string(), "no rule lowers a `ptrtoint` producing a `i32`");
    }

    /// The type this machine has no register for.
    fn long_double() -> Type {
        Type::float(rucc_ir::Float::F80)
    }

    #[test]
    fn a_double_widened_and_narrowed_again_goes_out_through_the_frame_and_back() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64]);
        let wide = cast(&mut source, block, Opcode::FPExt, args[0], long_double());
        let back = cast(&mut source, block, Opcode::FPTrunc, wide, f64);
        Builder::new(&mut source, block).ret(&[back]);

        // `double f(double d) { long double x = d; return x; }`. The x87 reads memory and nothing
        // else, so the value is written to the crossing slot, loaded at the format that widens it
        // and put in the slot the eighty bit value lives in. Coming back is the same three the
        // other way. Both slots are addressed by a `lea` with nothing in it yet, which is what
        // every address in a frame looks like here until `finish` has the numbers.
        assert_eq!(
            lower(&mut names, &source),
            "mfunc @f {\nblock0:\n    \
             %0:xmm($xmm0) = x64.arg_val_f64\n    \
             %1:gpr = x64.lea_64 [$rsp]\n    \
             %2:gpr = x64.lea_64 [$rsp]\n    \
             x64.movsd_mr %0, [%1]\n    \
             x64.fld_l [%1]\n    \
             x64.fstp_t [%2]\n    \
             %3:gpr = x64.lea_64 [$rsp]\n    \
             %4:gpr = x64.lea_64 [$rsp]\n    \
             x64.fld_t [%3]\n    \
             x64.fstp_l [%4]\n    \
             %5:xmm = x64.movsd_rm [%4]\n    \
             x64.ret_val_f64 %5($xmm0)\n}\n"
        );
    }

    #[test]
    fn a_long_double_has_sixteen_bytes_of_its_own_and_keeps_them() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64]);
        let wide = cast(&mut source, block, Opcode::FPExt, args[0], long_double());
        let once = cast(&mut source, block, Opcode::FPTrunc, wide, f64);
        let twice = cast(&mut source, block, Opcode::FPTrunc, wide, f64);
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::FAdd, once, twice, Flags::default());
        build.ret(&[sum]);

        let out = func(&source, &mut names, &SYSV).expect("every instruction is written");

        // Two slots and not four: sixteen bytes for the one eighty bit value, which is what the
        // psABI says one takes and is aligned to, and eight for the crossing, which every group
        // in the function shares because nothing is ever left in it. The value's slot is its own
        // for the whole function, so reading it twice reads the same sixteen bytes.
        assert_eq!(
            out.stack.locals,
            vec![Local { size: 8, align: 8 }, Local { size: 16, align: 16 }]
        );
    }

    #[test]
    fn an_integer_becomes_a_long_double_by_being_loaded_as_one() {
        let (mut names, mut source, block, args) = blank(&[Type::int(64)]);
        let wide = cast(&mut source, block, Opcode::SIToFP, args[0], long_double());
        let back =
            cast(&mut source, block, Opcode::FPTrunc, wide, Type::float(rucc_ir::Float::F64));
        Builder::new(&mut source, block).ret(&[back]);

        // `double f(long n) { long double x = n; return x; }`. `fild` is the same push at another
        // format, so the conversion is the load and there is no instruction that converts.
        let text = lower(&mut names, &source);
        assert!(text.contains("x64.mov_mr_64 %0, [%1]"), "{text}");
        assert!(text.contains("x64.fild_ll [%1]"), "{text}");
    }

    #[test]
    fn a_long_double_becoming_an_integer_cuts_towards_zero_with_the_control_word() {
        let (mut names, mut source, block, args) = blank(&[Type::float(rucc_ir::Float::F64)]);
        let wide = cast(&mut source, block, Opcode::FPExt, args[0], long_double());
        let whole = cast(&mut source, block, Opcode::FPToSI, wide, Type::int(32));
        Builder::new(&mut source, block).ret(&[whole]);

        // The one conversion here with no single instruction behind it. C cuts towards zero and
        // the unit rounds the way its control word says, so the word is saved, ORed with the two
        // bits that mean truncate, loaded, used and put back. Nine instructions for what `fisttp`
        // does in one, and `spec/10-backend.md` section 10.8 says why that one is not used.
        let text = lower(&mut names, &source);
        let group: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("x64.f") || line.contains("_16"))
            .collect();
        assert_eq!(
            group,
            [
                "x64.fld_l [%1]",
                "x64.fstp_t [%2]",
                "x64.fnstcw [%5]",
                "%6:gpr = x64.mov_rm_16 [%5]",
                "%7:gpr(reuse 1) = x64.or_ri_16 %6, 3072",
                "x64.mov_mr_16 %7, [%5 + 2]",
                "x64.fldcw [%5 + 2]",
                "x64.fld_t [%3]",
                "x64.fistp_l [%4]",
                "x64.fldcw [%5]",
            ],
            "{text}"
        );
    }

    #[test]
    fn a_long_double_is_read_and_written_as_the_bits_it_already_is() {
        let (mut names, mut source, block, args) = blank(&[Type::PTR, Type::PTR]);
        let mut build = Builder::new(&mut source, block);
        let value = build.load(long_double(), args[0], plain(), Flags::default());
        build.store(value, args[1], plain(), Flags::default());
        build.ret(&[]);

        // `void f(long double *a, long double *b) { *b = *a; }`. A copy is a push and a pop at the
        // format the value is already in, which neither converts nor looks: a signalling NaN stays
        // one and nothing is raised, which is the whole of what makes it a copy.
        let text = lower(&mut names, &source);
        let group: Vec<&str> =
            text.lines().map(str::trim).filter(|line| line.starts_with("x64.f")).collect();
        assert_eq!(
            group,
            ["x64.fld_t [%0]", "x64.fstp_t [%2]", "x64.fld_t [%3]", "x64.fstp_t [%1]"],
            "{text}"
        );
    }

    /// Two `long double` values, from two `double` parameters, and the instructions that made
    /// them, which every test below this one throws away.
    fn two_long_doubles(source: &mut Func, block: Block, args: &[Value]) -> (Value, Value) {
        let left = cast(source, block, Opcode::FPExt, args[0], long_double());
        let right = cast(source, block, Opcode::FPExt, args[1], long_double());
        (left, right)
    }

    /// The x87 instructions of a function, in order, with everything else dropped.
    fn stack_only(text: &str) -> Vec<&str> {
        text.lines().map(str::trim).filter(|line| line.contains("x64.f")).collect()
    }

    /// The two frame slots the last two addresses of a function were taken of, which in a
    /// comparison are the two operands in the order they go on the stack.
    fn pushed(out: &Lowered) -> Vec<usize> {
        let taken: Vec<usize> = out.stack.addresses.iter().map(|&(_, local)| local).collect();
        taken[taken.len() - 2..].to_vec()
    }

    #[test]
    fn adding_two_long_doubles_pushes_both_and_leaves_the_answer_in_a_slot() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64, f64]);
        let (left, right) = two_long_doubles(&mut source, block, &args);
        let sum =
            Builder::new(&mut source, block).binary(Opcode::FAdd, left, right, Flags::default());
        let back = cast(&mut source, block, Opcode::FPTrunc, sum, f64);
        Builder::new(&mut source, block).ret(&[back]);

        // `double f(double a, double b) { return (long double) a + (long double) b; }`. The last
        // four lines are the add: both operands pushed, the instruction that names neither of
        // them because they are the top two of a stack, and the answer taken off into its slot.
        let text = lower(&mut names, &source);
        assert_eq!(
            stack_only(&text),
            [
                "x64.fld_l [%2]",
                "x64.fstp_t [%3]",
                "x64.fld_l [%4]",
                "x64.fstp_t [%5]",
                "x64.fld_t [%6]",
                "x64.fld_t [%7]",
                "x64.fadd_p",
                "x64.fstp_t [%8]",
                "x64.fld_t [%9]",
                "x64.fstp_l [%10]",
            ],
            "{text}"
        );
    }

    #[test]
    fn a_subtraction_pushes_the_left_operand_first_so_it_is_the_one_subtracted_from() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64, f64]);
        let (left, right) = two_long_doubles(&mut source, block, &args);
        let less =
            Builder::new(&mut source, block).binary(Opcode::FSub, left, right, Flags::default());
        let back = cast(&mut source, block, Opcode::FPTrunc, less, f64);
        Builder::new(&mut source, block).ret(&[back]);

        // The left one goes on first, so it ends up under the right one, and `fsubp` takes the top
        // from the one below it. Which is `a - b` and is why the reversed mnemonic is never used
        // here: getting the order right at the push is the same answer for one fewer instruction
        // name to keep straight.
        let text = lower(&mut names, &source);
        assert_eq!(
            &stack_only(&text)[4..8],
            ["x64.fld_t [%6]", "x64.fld_t [%7]", "x64.fsub_p", "x64.fstp_t [%8]"],
            "{text}"
        );
        assert!(!text.contains("fsubr_p"), "{text}");
    }

    #[test]
    fn negating_a_long_double_turns_the_sign_over_and_reads_nothing() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64]);
        let wide = cast(&mut source, block, Opcode::FPExt, args[0], long_double());
        let flipped = Builder::new(&mut source, block).unary(Opcode::FNeg, wide, long_double());
        let back = cast(&mut source, block, Opcode::FPTrunc, flipped, f64);
        Builder::new(&mut source, block).ret(&[back]);

        // `fchs` and not a subtraction from zero, which would give a different answer at a negative
        // zero and would signal at a NaN. It does not read the value as a number at all.
        let text = lower(&mut names, &source);
        assert_eq!(
            &stack_only(&text)[2..5],
            ["x64.fld_t [%3]", "x64.fchs", "x64.fstp_t [%4]"],
            "{text}"
        );
    }

    #[test]
    fn comparing_two_long_doubles_puts_the_left_one_on_top() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64, f64]);
        let (left, right) = two_long_doubles(&mut source, block, &args);
        let mut build = Builder::new(&mut source, block);
        build.fcmp(FloatPred::Ogt, left, right, Flags::default());
        build.ret(&[]);

        // `a > b`. `fucomip` asks about the top of the stack against what is under it, so the
        // operand the predicate is about has to go on last, which is the other way round from the
        // arithmetic above. The pop that clears the loser and the byte that reads the flags are
        // both inside the one opcode.
        let out = func(&source, &mut names, &SYSV).expect("every instruction is written");
        let slots = pushed(&out);
        assert_eq!(slots, [2, 1], "the right operand goes on first and the left one on top");
        let text = mir::print_func(&out.func, &names, &REGS);
        assert_eq!(
            &stack_only(&text)[4..],
            ["x64.fld_t [%6]", "x64.fld_t [%7]", "%8:gpr = x64.fucomip_set_a"],
            "{text}"
        );
    }

    #[test]
    fn a_comparison_that_the_machine_has_backwards_swaps_the_two_pushes() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64, f64]);
        let (left, right) = two_long_doubles(&mut source, block, &args);
        let mut build = Builder::new(&mut source, block);
        build.fcmp(FloatPred::Olt, left, right, Flags::default());
        build.ret(&[]);

        // `a < b` is `b > a` and this machine has the one condition, so the same opcode runs with
        // the operands the other way round. The same trade the vector rules make, and it has to
        // be the same one: a `long double` comparison that picked a different condition from the
        // `double` comparison of the same two numbers would be wrong at exactly the unordered
        // cases the two conditions differ on.
        //
        // Which slot each push names is the whole of the difference from the test above, and the
        // text does not show it, since an address in a frame is a `lea` with nothing in it until
        // `finish` has the numbers. So the slots are what is read here.
        let out = func(&source, &mut names, &SYSV).expect("every instruction is written");
        let slots = pushed(&out);
        assert_eq!(slots, [1, 2], "the left operand goes on first and the right one on top");
        let text = mir::print_func(&out.func, &names, &REGS);
        assert_eq!(
            &stack_only(&text)[4..],
            ["x64.fld_t [%6]", "x64.fld_t [%7]", "%8:gpr = x64.fucomip_set_a"],
            "{text}"
        );
    }

    #[test]
    fn an_ordered_equal_needs_a_second_byte_to_put_the_two_conditions_together() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64, f64]);
        let (left, right) = two_long_doubles(&mut source, block, &args);
        let mut build = Builder::new(&mut source, block);
        build.fcmp(FloatPred::Oeq, left, right, Flags::default());
        build.ret(&[]);

        // Equal and ordered are two conditions and the flags carry both, so the opcode writes a
        // second register as well as the one the value is in and ANDs them together. Said here by
        // handing it a spare, since an instruction that wrote a register nothing knew about would
        // be an instruction the allocator could put a live value in the way of.
        let text = lower(&mut names, &source);
        assert!(text.contains("%8:gpr, %9:gpr = x64.fucomip_set_e_and_np"), "{text}");
    }

    #[test]
    fn a_comparison_that_is_never_asked_is_reported() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64, f64]);
        let (left, right) = two_long_doubles(&mut source, block, &args);
        let mut build = Builder::new(&mut source, block);
        build.fcmp(FloatPred::False, left, right, Flags::default());
        build.ret(&[]);

        // Always false is a constant and not a comparison, so there is no condition to pick and
        // nothing here folds it into one: an instruction that quietly agreed with it would hide
        // that the optimizer left a comparison in that it should have taken out.
        let failed = func(&source, &mut names, &SYSV).expect_err("no condition is always false");
        assert_eq!(failed.to_string(), "no rule lowers a `fcmp` producing a `i1`");
    }

    #[test]
    fn a_long_double_constant_is_the_bits_of_it_put_where_the_value_lives() {
        let (mut names, mut source, block, args) = blank(&[Type::PTR]);
        let mut build = Builder::new(&mut source, block);
        // `1.5L`, which is the leading bit and one more of significand, and an exponent of zero.
        let one_and_a_half = build.fconst(long_double(), 0x3fff_c000_0000_0000_0000);
        build.store(one_and_a_half, args[0], plain(), Flags::default());
        build.ret(&[]);

        // No x87 instruction at all. A slot holding one of these is the value, so a constant is
        // its ten bytes written where the value lives, and whatever reads it does the `fld`.
        let text = lower(&mut names, &source);
        assert!(text.contains("x64.mov_ri_64 -4611686018427387904"), "{text}");
        assert!(text.contains("x64.mov_ri_16 16383"), "{text}");
        assert!(text.contains("x64.mov_mr_16 %3, [%1 + 8]"), "{text}");
        // The six bytes above the ten are the padding that makes the type sixteen wide, and they
        // are unspecified rather than zero, so nothing writes them.
        assert_eq!(text.matches("x64.mov_mr").count(), 2, "{text}");
    }

    #[test]
    fn a_negative_long_double_constant_keeps_the_bit_above_its_exponent() {
        let (mut names, mut source, block, args) = blank(&[Type::PTR]);
        let mut build = Builder::new(&mut source, block);
        let minus = build.fconst(long_double(), 0xbfff_c000_0000_0000_0000);
        build.store(minus, args[0], plain(), Flags::default());
        build.ret(&[]);

        // `-1.5L`. The sign is the top bit of the two byte half, so the immediate that half is put
        // in a register with is above the signed range of sixteen bits and has to stay there: read
        // as a number it would be negative, and it is not a number, it is two bytes.
        let text = lower(&mut names, &source);
        assert!(text.contains("x64.mov_ri_16 49151"), "{text}");
    }

    #[test]
    fn a_long_double_crosses_an_edge_as_an_address_and_is_copied_where_it_lands() {
        let (mut names, mut source, block, args) = blank(&[Type::float(rucc_ir::Float::F64)]);
        let wide = cast(&mut source, block, Opcode::FPExt, args[0], long_double());
        let next = source.create_block();
        let param = source.append_param(next, long_double());
        Builder::new(&mut source, block).jump(next, &[wide]);
        Builder::new(&mut source, next).ret(&[param]);

        // What the edge carries is the address of the slot the value is already in, which is an
        // ordinary register the allocator has an opinion about. The block on the other side copies
        // the sixteen bytes into a slot of its own before anything reads them, so a second edge
        // handing over a second address would still leave one place for a reader to look.
        let text = lower(&mut names, &source);
        let second: Vec<&str> = text
            .lines()
            .skip_while(|line| !line.starts_with("block1"))
            .skip(1)
            .take(3)
            .map(str::trim)
            .collect();
        assert_eq!(
            second,
            ["x64.fld_t [%4]", "%5:gpr = x64.lea_64 [$rsp]", "x64.fstp_t [%5]"],
            "{text}"
        );
    }

    #[test]
    fn more_long_doubles_at_a_block_than_the_stack_is_deep_are_reported() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64]);
        let wide = cast(&mut source, block, Opcode::FPExt, args[0], long_double());
        let next = source.create_block();
        let params: Vec<Value> =
            (0..=X87_DEPTH).map(|_| source.append_param(next, long_double())).collect();
        let carried: Vec<Value> = params.iter().map(|_| wide).collect();
        Builder::new(&mut source, block).jump(next, &carried);
        Builder::new(&mut source, next).ret(&[params[0]]);

        // The copies go through the x87 stack so that every one of them is read before any of them
        // is written, which is what makes a block that swaps two of these right. Nine of them do
        // not fit on the stack, and copying the ninth before or after the rest is the order that
        // could be wrong, so it is refused instead.
        let failed = func(&source, &mut names, &SYSV).expect_err("nine do not fit on the stack");
        assert_eq!(
            failed.to_string(),
            "block1 takes 9 parameters of type `f80` and only 8 can cross an edge at once"
        );
        assert_eq!(failed.inst(), None);
    }
}
