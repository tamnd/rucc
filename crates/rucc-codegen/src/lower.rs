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
use rucc_ir::{Block, Def, Extra, Func, Inst, Opcode, Type, Value};
use rucc_mir as mir;
use rucc_target::x86_64;
use rucc_target::{CallRegs, RegClass};

use crate::abi::{self, Missing, Refused};
use crate::frame::{Layout, Local};
use crate::select::{Match, Piece, Rule, Table};
use crate::term::{MAX_ARGS, PLAIN, Plan, Shown, Term, Terms};
use crate::varargs;

/// The prefix a rule file puts in front of a machine term, which says which target it belongs
/// to and is not part of the opcode.
const PREFIX: &str = "x64.";

/// How wide an address is on this target, which is the width a cast between a pointer and an
/// integer has to be at for the cast to be nothing.
const ADDRESS_BITS: u32 = 64;

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
    /// A bulk copy or fill too large to write as moves, which is a call to the C library.
    ///
    /// Not an instruction no rule covers either. A copy of a size worth unrolling is unrolled
    /// before this runs and one that is not is a call, and a call needs a `memcpy` that a
    /// statically linked program has somewhere to get, which is the compiler runtime question in
    /// tamnd/rucc#277 and not a rule anybody could write here.
    Bulk {
        /// The `memcpy`, `memmove` or `memset`.
        inst: Inst,
        /// How many bytes it moves, which is what says it was too large.
        size: u64,
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
            | Unsupported::Dynamic { inst, .. }
            | Unsupported::Bulk { inst, .. } => Some(inst),
            Unsupported::Argument { .. } => None,
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
            Unsupported::Dynamic { .. } => {
                f.write_str("nothing here grows the stack for a variable length array")
            }
            Unsupported::Bulk { size, .. } => {
                write!(f, "a copy of {size} bytes is a call to the library and there is no runtime")
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
        Self {
            source,
            names,
            out: mir::Func::new(name),
            regs: vec![None; counts.values],
            written: vec![None; counts.values],
            blocks: vec![None; counts.blocks],
            uses,
            at: None,
            gpr: x86_64::GPR,
            conv,
            stack: Stack::default(),
            varargs: None,
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
        Ok(Lowered { func: self.out, stack: self.stack })
    }

    /// One block: its parameters, then every instruction in it that is not folded into another.
    fn block(&mut self, block: Block) -> Result<(), Unsupported> {
        let out = self.out_block(block);
        self.at = Some(out);
        if self.source.entry() == Some(block) {
            self.arrive(block, out)?;
        } else {
            for &param in self.source[block].params.iter() {
                let reg = self.out.append_param(out, self.class_of(self.source[param].ty));
                self.regs[param.index()] = Some(reg);
            }
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
                // A copy or a fill that [`crate::expand`] left where it was, which is one too
                // large to be worth unrolling or one whose fill byte is not a constant. Either
                // way what it should become is a call to the C library, so it is refused as that
                // rather than as a rule nobody wrote.
                Opcode::Memcpy | Opcode::Memmove | Opcode::Memset => {
                    let size = match self.source[inst].extra {
                        Extra::Mem(mem) => self.source[mem].size,
                        _ => 0,
                    };
                    return Err(Unsupported::Bulk { inst, size });
                }
                // A cast between a pointer and an integer of the same width, which on this
                // machine is every one the front end writes. No instruction at all, so no rule
                // could name one.
                Opcode::PtrToInt | Opcode::IntToPtr => {
                    self.rename(inst)?;
                    continue;
                }
                _ => {}
            }
            let matched = matched.ok_or_else(|| self.unsupported(inst))?;
            self.emit(inst, &matched)?;
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

        let mut args = Vec::with_capacity(values.len());
        for value in values.into_iter().skip(usize::from(indirect)) {
            args.push((self.source[value].ty, self.reg_of(value)?));
        }
        let signature = &self.source[info.signature];
        let variadic = signature.variadic;
        let returns = signature.return_types().next();
        // More than one value back is the convention's answer rather than a term's, the same way
        // a return of two values is, and nothing here has a name for it.
        if signature.return_types().count() > 1 {
            return Err(self.unsupported(inst));
        }

        let block = self.at.expect("a block is being filled");
        let what = abi::Calling { callee, args: &args, returns, variadic };
        let made = abi::call(&mut self.out, block, &what, self.conv, self.names)
            .map_err(|refused| Unsupported::Call { inst, refused })?;
        let calls = &mut self.stack.calls;
        *calls = Some(calls.unwrap_or(0).max(made.outgoing));
        if let (Some(result), Some(reg)) = (data.first_result, made.result) {
            self.regs[result.index()] = Some(reg);
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
                regs.push(self.reg_of(value)?);
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
        let types: Vec<Type> = params.iter().map(|&value| self.source[value].ty).collect();
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
    /// none. An unconditional jump is the third, and there is even less of it: the edge is on the
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
            Opcode::Return => self.source[data.args].is_empty(),
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
    use rucc_ir::{Builder, CallInfo, Flags, InstData, MemInfo, MemOrder, Signature, Type};
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
        MemInfo { size: 0, align: 1, order: MemOrder::NotAtomic, tbaa: None }
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
        // The copy is a register allocator that takes no hints. It hands `%0` a register at the
        // instruction that writes it, where it does not yet know that a later use insists on
        // `rax`, and `rax` is not free to hand out because that later use is holding it. So the
        // value goes somewhere else and is copied in. Every division and every shift by a
        // register already pays the same thing, and paying it once per return is what makes it
        // worth fixing rather than a new problem.
        assert_eq!(
            mir::print_func(&out, &names, &REGS),
            "mfunc @f {\nblock0:\n    $rcx = x64.mov_ri_32 0\n    $rax = x64.mov_rr_64 $rcx\n    \
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
        // Four moves that a good allocator writes none of, and it is the same allocator that
        // takes no hints as in the return above rather than anything new. It hands each argument
        // a register at the pseudo that defines it, without looking at the fixed register that
        // pseudo insists on, so every argument is copied straight back out of where it already
        // was. Issue #255 is this, and this function is the shortest program that shows what it
        // costs: one hint per argument and one per return would leave nothing here but the
        // addition. What the test is for meanwhile is that the answer is right, and it is: the
        // copy in front of a two address instruction is what makes its destination one of the
        // registers it reads, and the source operand keeps its own name because the destination
        // is what the encoder writes.
        assert_eq!(
            mir::print_func(&out, &names, &REGS),
            "mfunc @f {\nblock0:\n    $rdi($rdi) = x64.arg_val_32\n    \
             $rax = x64.mov_rr_64 $rdi\n    $rsi($rsi) = x64.arg_val_32\n    \
             $rcx = x64.mov_rr_64 $rsi\n    $rdx = x64.mov_rr_64 $rax\n    \
             $rdx(reuse 1) = x64.add_rr_32 $rax, $rcx\n    $rax = x64.mov_rr_64 $rdx\n    \
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
        let sig = source
            .add_signature(Signature::new().with_returns(&[Type::float(rucc_ir::Float::F80)]));
        let callee = names.intern("g");
        Builder::new(&mut source, block).call(callee, sig, &[]);
        let failed = func(&source, &mut names, &SYSV).expect_err("a long double is on the x87");
        assert_eq!(failed.to_string(), "what this call gives back is on the x87 stack");
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
        let i64 = Type::int(64);
        let (mut names, mut source, block, args) = blank(&[i64, i64]);
        let mut build = Builder::new(&mut source, block);
        build.ret(&[args[0], args[1]]);

        // Two values back at once. Where each of them goes is the convention's answer rather than
        // a term's, so the rule language has no name for it and no rule fires.
        let failed = func(&source, &mut names, &SYSV).expect_err("nothing returns two values");
        assert_eq!(failed.to_string(), "no rule lowers a `return`");

        // A `return` produces nothing, so there is no type in the message and nothing invents
        // one, and the instruction comes back so a caller can ask the function where it was.
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
}
