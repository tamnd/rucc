//! The prologue, the epilogue, and the moves the allocator asked for.
//!
//! Design: `spec/10-backend.md` sections 10.4 and 10.7.
//!
//! [`crate::frame`] works out what a function's stack looks like and writes nothing. This is what
//! writes it. Three things are still missing from a function the allocator has finished with, and
//! all three of them are instructions no lowering rule chose:
//!
//! ```text
//!   the prologue     takes the frame the layout worked out, and puts away the registers a call
//!                    leaves alone that this function writes anyway
//!   the moves        every spill, every reload and every copy the allocator handed back as an
//!                    edit, in the place it said and in the order it said
//!   the epilogue     gives the frame back and puts the registers back, at the end of every block
//!                    the function returns from
//! ```
//!
//! There is a fourth thing and it is not an instruction but a number. The lowering wrote an
//! instruction for every `alloca` that computes the address of the memory it asked for, and could
//! not write how far into the frame that memory is, because when it ran there was no frame. So
//! the displacement of each of those is filled in here, out of the same [`Frame`] everything else
//! here reads, and off the same stack pointer every other offset in it is from.
//!
//! The loads that read the arguments the caller passed on the stack are waiting on the same number
//! and on one more. Those bytes are the caller's rather than this function's, and a frame that had
//! to force its own alignment cannot say how far away the caller's stack pointer was, so it reaches
//! back through the frame pointer instead. Which register a load reads through is therefore settled
//! here too, and it is the only base register in a finished function that was not settled by
//! whoever wrote the instruction.
//!
//! After this the function is one an encoder can read: every register is physical, every offset
//! into the frame is a constant, and the stack pointer is where the convention says it should be
//! at every instruction that could look.
//!
//! # Why the moves go in first
//!
//! Every offset the frame reports is from the stack pointer as it stands in the body of the
//! function. A spill written before the prologue exists would be written in front of the
//! instruction it belongs to and behind nothing, which is where the prologue then goes, so the
//! prologue ends up in front of it and the offsets stay true. Writing them the other way round
//! would put the first reload above the instruction that takes the frame, and it would read from
//! an address that is one frame out.
//!
//! # Where a return is
//!
//! A block that goes nowhere is a block the function leaves from. Mostly that is a return, and
//! the other kind is a block ending in `unreachable`, which is a point the front end says control
//! does not arrive at and which the lowering writes no instruction for. Both want the same thing
//! here. A return wants the epilogue because that is what a return is once the frame is known,
//! and an unreachable block wants it because the alternative is a function whose last instruction
//! falls into whatever the assembler put after it, which is worse than an epilogue nothing runs.
//! So the epilogue goes at the end of every block with an empty successor list, and there may be
//! several, because nothing here insists a function has one exit.
//!
//! # What is target-specific here
//!
//! The names, and only the names. Which instruction pushes a register and which one moves the
//! stack pointer is [`rucc_target::FrameInsts`], which the target says and this reads, so what
//! is written below is the shape of a prologue rather than any particular machine's. That is
//! `spec/10-backend.md` section 10.8 as it applies to the one pass that would otherwise be full
//! of `x64.` by hand.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_mir::{Block, BlockCall, CfiOp, Func, Inst, Mem, Opcode, Operand, Patch, Reg};
use rucc_regalloc::Allocation;
use rucc_regalloc::assign::Place;
use rucc_regalloc::rewrite::{At, Edit};
use rucc_target::{BranchInsts, CallRegs, FrameInsts, Guard, PhysReg, Probe, RegClass};

use crate::frame::Frame;
use crate::lower::Stack;

/// What the stack protector's check needs beyond the frame, in a function that has one.
///
/// Three things that come from three places, which is why they arrive together rather than being
/// looked up here. Where the word the canary is copied from lives is a fact about the runtime the
/// code is linked against. What a branch on a register is is a fact about the machine. And the two
/// registers are neither: they are the ones the allocator was told to hold back, which is a
/// decision about the allocator, and they are free at a return for exactly that reason.
#[derive(Debug, Clone, Copy)]
pub struct Protect<'a> {
    /// Where the word the canary is a copy of lives, and what to call when the copy has changed.
    pub guard: &'a Guard,
    /// What a branch on a register is, which is what the check ends its block with.
    pub branch: &'a BranchInsts,
    /// The two registers the check may use, which are two the allocator never handed out.
    pub scratch: [PhysReg; 2],
}

/// What a prologue that takes its frame a page at a time needs beyond the frame.
///
/// What `-fstack-clash-protection` asks for, and the same three kinds of thing [`Protect`] is:
/// one fact about the platform, one about the machine, and two registers that are neither. See
/// [`rucc_target::Probe`] for what the sequence is defending against.
#[derive(Debug, Clone, Copy)]
pub struct Probing<'a> {
    /// What touches a page and how far apart the pages are.
    pub probe: &'a Probe,
    /// What a branch on a register is, which is what the loop under a large frame ends with.
    pub branch: &'a BranchInsts,
    /// The two registers the sequence may use, which are two the allocator never handed out.
    pub scratch: [PhysReg; 2],
}

/// What a profiler's hook at the top of a function is, in a function that has one.
///
/// What `-pg` asks for. See [`rucc_target::Trace`] for why there are two of these and what each of
/// them lets the hook see. Only the name survives to here, because by this point the flag has been
/// read against the target and a prologue that has the name has everything it needs.
#[derive(Debug, Clone, Copy)]
pub struct Tracing {
    /// What is called, which is a routine the runtime provides and not one the program wrote.
    pub name: &'static str,
    /// Whether the call goes in front of the prologue rather than once the frame is taken.
    pub early: bool,
}

/// The room at the top of a function for something to be written over later, in a function that
/// was promised any.
///
/// What `-fpatchable-function-entry=` asks for. The room is a run of the shortest instruction the
/// machine has that does nothing, and what makes it worth reserving is that it is never run for
/// long: a tracer or a live patcher writes a jump or a call over it once the program is up, and
/// what it needs from the compiler is a known address and a known number of bytes.
///
/// Two counts because the room can be on either side of the function's own label. Only the half
/// after it is written here, since the stream starts at the label and there is nowhere in it to put
/// the other half; the half in front is carried through so that whatever lays the function down can
/// lay that many bytes ahead of the symbol.
#[derive(Debug, Clone, Copy)]
pub struct Padding {
    /// What the instruction that does nothing is called on this target.
    pub name: &'static str,
    /// How many of them go in front of the function's own label.
    pub before: u32,
    /// How many go after it.
    pub after: u32,
}

/// What the convention this function is compiled for says a frame is.
///
/// Seven answers to the one question, which is why they travel together: where it puts things,
/// which instructions build one, whether this function's carries a protector, whether it is taken a
/// page at a time, whether the function opens with a landing pad, whether it calls a profiler on
/// the way in, and how much room it opens with for a patcher. The last five are the only ones about
/// this function rather than about every function on the target, and they are here because what
/// they need is the other two and nothing else.
#[derive(Debug, Clone, Copy)]
pub struct Convention<'a> {
    /// Where the convention puts things.
    pub regs: &'a CallRegs,
    /// The instructions a prologue, an epilogue, a spill and a reload are made of on it.
    pub insts: &'a FrameInsts,
    /// What this function's stack protector needs, or `None` in a function with none.
    pub protect: Option<Protect<'a>>,
    /// What this function's probing prologue needs, or `None` when the frame is taken in one
    /// subtraction, which is what a command line that did not ask asks for.
    pub probe: Option<Probing<'a>>,
    /// What says an indirect branch may arrive at the top of this function, or `None` when the
    /// command line did not ask for one and on a target that has no such instruction.
    ///
    /// See [`rucc_target::FrameInsts::landing`]. A name rather than a flag because the flag has
    /// already been read against the target by the time this is built, and because a prologue that
    /// has the name has everything it needs.
    pub landing: Option<&'static str>,
    /// What this function's call to a profiler is, or `None` in one that makes none, which is every
    /// function on a command line that did not ask.
    pub trace: Option<Tracing>,
    /// What room this function opens with for a patcher, or `None` in one that was promised none,
    /// which is every function on a command line that did not ask.
    pub pad: Option<Padding>,
}

impl<'a> Convention<'a> {
    /// That convention, for a function with no stack protector, no probing, no landing pad, no
    /// call to a profiler and no room for a patcher, which is most of them.
    #[must_use]
    pub fn new(regs: &'a CallRegs, insts: &'a FrameInsts) -> Self {
        Self { regs, insts, protect: None, probe: None, landing: None, trace: None, pad: None }
    }
}

/// Which instruction each of the allocator's moves became.
///
/// A spill and a copy are both a `mov` once they are written, and so is an instruction the lowering
/// wrote that happens to move the same register to the same address. Telling them apart afterwards
/// by looking at them is guesswork, and a pass that guesses wrong about a store to a volatile
/// variable deletes a read the program insisted on. So what the allocator asked for is recorded as
/// it is written, and a later pass that is only allowed to touch the allocator's own moves has the
/// list rather than a heuristic. See [`crate::reload`], which is the one pass that reads this.
#[derive(Debug, Default)]
pub struct Moves(HashMap<Inst, Edit>);

impl Moves {
    /// What the allocator asked for at this instruction, or `None` at an instruction that is not
    /// one of its moves.
    #[must_use]
    pub fn at(&self, inst: Inst) -> Option<Edit> {
        self.0.get(&inst).copied()
    }

    /// Records that this instruction is what that move came to.
    pub fn record(&mut self, inst: Inst, edit: Edit) {
        self.0.insert(inst, edit);
    }
}

/// Writes the moves, the prologue and the epilogue into a function the allocator has finished
/// with.
///
/// Hands back which instruction each of the allocator's moves became, for the one pass that is
/// allowed to take one of them out again.
///
/// # Panics
///
/// Panics on a function with no blocks in it, on a frame whose slots or locals the allocation and
/// the lowering do not match, and on a move of a class the target did not say how to move. All of
/// them are the caller handing it a frame and a function that were not worked out from each other.
pub fn finish(
    func: &mut Func,
    allocation: &Allocation,
    frame: &Frame,
    stack: &Stack,
    convention: Convention<'_>,
    names: &mut Interner,
) -> Moves {
    let Convention { regs: conv, insts, protect, probe, landing, trace, pad } = convention;
    let entry = func.entry().expect("a function with a block in it");
    let returns: Vec<Block> = func.blocks().filter(|&block| func[block].succs.is_empty()).collect();

    // Before anything is written, because these are instructions the lowering already put in the
    // function and every one of them is somewhere the prologue is about to go in front of, which
    // is what makes an offset from the stack pointer the right thing to write into them. In a
    // frame that grows it is an offset from the frame pointer instead, so the base register is
    // rewritten the way an incoming argument's is, and for a version of the same reason.
    //
    // Added rather than assigned. The instruction named here is the `lea` the lowering wrote, or
    // whatever [`crate::fold`] folded that `lea` into, and a reader that took it brought a
    // displacement of its own: the address of a local is where the object starts and reading a
    // field of it is some way past that. Assigning would throw the field offset away and read the
    // front of the object every time.
    for &(inst, local) in &stack.addresses {
        let at = frame.local(local).expect("a local the frame was worked out from");
        let mem = func[inst].mem.expect("the address of a local is an address");
        func[mem].disp += at;
        if frame.grows() {
            rebase(func, inst, conv.frame_pointer);
        }
    }

    // The bytes a variable length array takes are already off the stack pointer by the time one of
    // these runs, so what is left to write is how far above the new stack pointer the array starts,
    // which is however much of the bottom of the frame belongs to the arguments of a call. That
    // area stays at the bottom wherever the bottom has moved to. Added rather than assigned for the
    // reason the loop above is: one of these folds into its readers like any other address, and a
    // reader that took it brought a displacement of its own.
    for &inst in &stack.dynamic {
        let mem = func[inst].mem.expect("the address of a growable local is an address");
        func[mem].disp += offset(frame.below());
    }

    // The same, one area further up, and through the frame pointer when that is what reaches it.
    // These are in the entry block ahead of everything, so the prologue still goes in front of
    // them, which is what makes both registers hold what these offsets are counted from.
    let incoming = frame.incoming();
    for &(inst, up) in &stack.arguments {
        let mem = func[inst].mem.expect("an argument read out of memory is read from an address");
        func[mem].disp += incoming.at + offset(up);
        if incoming.through_frame_pointer {
            rebase(func, inst, conv.frame_pointer);
        }
    }

    // Every offset the frame reports is from this one register, which is the stack pointer in an
    // ordinary frame and the frame pointer in one that moves the stack pointer while it runs.
    let base = if frame.grows() { conv.frame_pointer } else { conv.stack_pointer };
    let mut writer = Writer { func, conv, insts, names, base, ahead: None };

    let mut cursors: HashMap<At, Inst> = HashMap::new();
    let mut moves = Moves::default();
    for edit in &allocation.edits {
        let inst = writer.mov(edit, frame);
        writer.put(&mut cursors, edit.at, inst);
        moves.record(inst, *edit);
    }

    let prologue = writer.prologue(frame, protect, probe, landing, trace, pad);
    for &inst in prologue.iter().rev() {
        writer.func.prepend_inst(entry, inst);
    }
    for block in returns {
        // The check goes in front of the epilogue and takes the return with it. What is left in
        // the block the function used to return from is the check, and the block the epilogue then
        // goes in is the arm the canary was unchanged on.
        let block = match protect {
            Some(protect) => writer.check(block, frame, protect),
            None => block,
        };
        let epilogue = writer.epilogue(frame);
        for inst in epilogue {
            writer.func.append_inst(block, inst);
        }
    }

    // Last of everything, because the blocks a probing prologue made have to come in front of the
    // block the function used to begin with and the ones the protector's check makes are made
    // after that. Nothing has been laid out yet: `crate::layout` runs after this and puts every
    // block in its own order, and all this decides is which block the function is entered at.
    if let Some(ahead) = writer.ahead {
        let rest: Vec<Block> =
            writer.func.blocks().filter(|block| !ahead.contains(block)).collect();
        let order: Vec<Block> = ahead.into_iter().chain(rest).collect();
        writer.func.set_block_order(&order);
    }
    moves
}

/// How many pages a probing prologue touches one after another before it writes a loop instead.
///
/// Three, which is what gcc unrolls to. The loop is four instructions however many pages it walks
/// and a page written out is two, so three is the last size at which the straight line is no
/// longer than the loop, and the straight line has no branch in it and needs no register.
const UNROLLED: u32 = 3;

/// One function having its frame written into it.
/// Points an address the lowering left counted from the stack pointer at another register.
///
/// The base register is an operand of the instruction and the addressing mode holds where in the
/// operand vector it is, so the register is changed there and not in the mode.
fn rebase(func: &mut Func, inst: Inst, to: PhysReg) {
    let mem = func[inst].mem.expect("an address");
    let at = func[mem].base.expect("an address the lowering wrote a base register into");
    let operands = func[inst].operands;
    func[operands][usize::from(at)].reg = Reg::physical(to);
}

struct Writer<'a> {
    func: &'a mut Func,
    conv: &'a CallRegs,
    insts: &'a FrameInsts,
    names: &'a mut Interner,
    /// Which register every offset into the frame is counted from, which is the stack pointer
    /// unless the function moves it while it runs. See `Growing` in [`crate::frame`].
    base: PhysReg,
    /// The blocks a probing prologue made, which go in front of the one the function began with.
    ///
    /// Empty in every function whose frame is taken in one subtraction, which is every function
    /// on a command line that did not ask for the stack to be touched a page at a time and most
    /// of them on one that did. See [`Writer::pages`].
    ahead: Option<[Block; 2]>,
}

impl Writer<'_> {
    /// The instructions the prologue is, in the order they run.
    ///
    /// The order is the one the epilogue undoes and it is not free. The frame pointer is saved
    /// before anything else, so that it points at a fixed place whatever else happens. The
    /// registers are pushed before the alignment is forced, so that the epilogue can find them
    /// again from the frame pointer, since after the alignment is forced nothing else can. And the
    /// vector registers are stored last, because until the frame has been taken there is nowhere
    /// to store them.
    ///
    /// The landing pad is in front of all of it, because the address it makes reachable is the
    /// address of the function and the address of the function is where the first instruction is.
    /// It has to be written here rather than after the fact, since a probing prologue moves the
    /// instructions written so far into a block of its own and the pad has to move with them.
    ///
    /// The room a patcher was promised goes after the pad, because a patcher wants somewhere it can
    /// write a call that happens before anything else, and the pad is the one instruction that has
    /// to come first for a reason of its own.
    ///
    /// A profiler's hook goes next, or at the end when it is the kind that reads the frame pointer.
    /// The early one is in front of everything the frame does for a reason of its own: what makes
    /// it worth replacing while the program runs is that the stack at that instruction is exactly
    /// what a call leaves, and a prologue that had already run would have changed it.
    fn prologue(
        &mut self,
        frame: &Frame,
        protect: Option<Protect<'_>>,
        probe: Option<Probing<'_>>,
        landing: Option<&'static str>,
        trace: Option<Tracing>,
        pad: Option<Padding>,
    ) -> Vec<Inst> {
        let sp = self.conv.stack_pointer;
        let fp = self.conv.frame_pointer;
        let int = self.conv.int_class;
        let sse = self.conv.sse_class;
        let word = offset(self.conv.word);
        let mut out = Vec::new();
        // What the prologue wrote before it had described anything, which is what decides whether
        // there is a rule to remember at the end of it. Neither of these moves a register or takes
        // a frame, so a function whose whole prologue is one of them has no rows and must not be
        // given a pair of them that cancel out.
        let mut quiet = Vec::new();
        if let Some(name) = landing {
            let opcode = self.opcode(name);
            let inst = self.func.build_loose(opcode).finish();
            out.push(inst);
            quiet.push(inst);
        }
        // After the pad and in front of everything else, which is where gcc puts it. The pad is the
        // function's first instruction because the address an indirect branch may arrive at is the
        // address of the function, and the room comes next because what gets written over it is a
        // call and the point of that call is that it happens before the function has done anything.
        //
        // Nothing is described for any of it. A byte that does nothing does not move the stack
        // pointer, and what a patcher writes over it later is its own problem rather than this
        // function's: the rules here say what this function did, and it did nothing.
        if let Some(pad) = pad {
            let opcode = self.opcode(pad.name);
            let mut first = None;
            for _ in 0..pad.after {
                let inst = self.func.build_loose(opcode).finish();
                out.push(inst);
                quiet.push(inst);
                first.get_or_insert(inst);
            }
            self.func.patch = Some(Patch { before: pad.before, pad: opcode, after: first });
        }
        // Nothing is described for it and nothing needs to be: the call pushes a return address and
        // the hook pops it, so the frame is the same on both sides, and the hook preserves every
        // register because it is written in assembly for exactly this. That is also why the
        // allocator, which ran before any of this, never saw the call and did not have to.
        if let Some(trace) = trace.filter(|trace| trace.early) {
            let inst = self.hook(trace);
            out.push(inst);
            quiet.push(inst);
        }
        // How far the stack pointer is below the canonical frame address, and whether the address
        // is still counted from the stack pointer at all. It starts at the return address the
        // call itself pushed, which is the rule the CIE already states, so the first row here is
        // the first thing this function does on top of that.
        let mut below = offset(self.conv.return_address);
        let mut from_sp = true;
        if frame.frame_pointer() {
            let inst = self.push(fp);
            out.push(inst);
            below += word;
            self.row(inst, CfiOp::DefCfaOffset(below));
            self.saved(inst, int, fp, -below);
            let mov = self.opcode(self.insts.moves(int).expect("a move").mov);
            let inst = self.two(mov, fp, sp);
            out.push(inst);
            let number = self.dwarf(int, fp);
            self.row(inst, CfiOp::DefCfaRegister(number));
            from_sp = false;
        }
        for &reg in frame.saved_int() {
            let inst = self.push(reg);
            out.push(inst);
            below += word;
            if from_sp {
                self.row(inst, CfiOp::DefCfaOffset(below));
            }
            self.saved(inst, int, reg, -below);
        }
        if let Some(to) = frame.realign() {
            // Nothing is written for this and nothing can be. After it the stack pointer is a
            // rounded-down version of where it was rather than a fixed distance from it, which is
            // exactly what a rule cannot say. It is also why a frame that realigns is a frame
            // with a frame pointer: by here the address is already counted from that instead.
            assert!(!from_sp, "a frame that forces its own alignment has a frame pointer");
            let and = self.opcode(self.insts.align);
            out.push(self.arith(and, -i64::from(to)));
        }
        if frame.size() > 0 {
            self.take(&mut out, frame.size(), &mut below, from_sp, probe);
        }
        for save in frame.saved_sse() {
            let inst = self.store(sse, save.reg, save.at);
            out.push(inst);
            // Where it went is an offset from whichever register the frame counts from, and the
            // address is a constant above that register, so the two make one constant. In an
            // ordinary frame that register is the stack pointer and the constant is `below`. In one
            // that grows it is the frame pointer, which the address has been counted from since the
            // prologue pointed it at where it saved the caller's copy, so the constant is the two
            // words above it and nothing the prologue did afterwards changes it. A realigned frame
            // has no such constant at all and the rule is left out rather than guessed; the one
            // convention that realigns and the one that preserves a vector register are not the
            // same convention, so nothing reaches any of this today.
            if frame.realign().is_none() {
                let above =
                    if frame.grows() { word + offset(self.conv.return_address) } else { below };
                self.saved(inst, sse, save.reg, save.at - above);
            }
        }
        // Before the canary and after the frame, which is where gcc puts it. The hook reads the
        // frame pointer to find out who called this function, so it has to run once there is one,
        // and it is a call, so it has to run before anything the function is keeping in the frame
        // could be read back.
        if let Some(trace) = trace.filter(|trace| !trace.early) {
            let inst = self.hook(trace);
            out.push(inst);
        }
        // Last of everything, because it writes into the frame and there is no frame to write into
        // until the stack pointer has moved. Nothing is described for either instruction: they
        // write a slot rather than save a register, and no unwinder wants to put a canary back.
        if let Some(protect) = protect {
            let at = frame.canary().expect("a protected function has a slot for its canary");
            let [into, _] = protect.scratch;
            out.push(self.read_guard(into, protect.guard));
            out.push(self.store(self.conv.int_class, into, at));
        }
        // The rules the body runs under, kept so that each epilogue can put them back rather than
        // leaving the next block reading whatever the last one ended on. See `epilogue`.
        //
        // Nothing is kept in a function whose whole prologue is the pieces that describe nothing.
        // See `quiet` above.
        if let Some(&last) = out.last() {
            if !quiet.contains(&last) {
                self.row(last, CfiOp::RememberState);
            }
        }
        out
    }

    /// The call to a profiler's hook.
    ///
    /// No arguments and no result. Which function is being entered is not passed, because the hook
    /// reads its own return address to find out, and that is the whole reason the call is written
    /// rather than something cheaper.
    fn hook(&mut self, trace: Tracing) -> Inst {
        let call = self.opcode(self.insts.call);
        let symbol = self.names.intern(trace.name);
        self.func.build_loose(call).symbol(symbol).finish()
    }

    /// Takes the frame, which is one subtraction unless the command line asked for the stack to be
    /// touched a page at a time.
    ///
    /// `below` is how far the canonical frame address is above the stack pointer, and it comes
    /// back as what it is once the frame has been taken.
    fn take(
        &mut self,
        out: &mut Vec<Inst>,
        size: u32,
        below: &mut i32,
        from_sp: bool,
        probe: Option<Probing<'_>>,
    ) {
        let Some(probing) = probe.filter(|probing| size > probing.probe.interval) else {
            let inst = self.sub(size);
            out.push(inst);
            *below += offset(size);
            if from_sp {
                self.row(inst, CfiOp::DefCfaOffset(*below));
            }
            return;
        };
        // Every step but the last is a whole page and is followed by a touch, and the last is
        // whatever is left over, which is between one byte and one whole page. So the stack
        // pointer never moves further than a page without something being written where it landed,
        // and the unmapped page an operating system leaves below a stack cannot be stepped over.
        //
        // That is why the count is worked out from one less than the size. A frame that is an
        // exact number of pages gets one fewer touch than it has pages, and the step left over is
        // a whole page, which is a step that lands on the next page boundary rather than past it.
        // gcc touches that last page as well, so this is one instruction shorter on a frame whose
        // size is a multiple of the page and the same everywhere else.
        let interval = probing.probe.interval;
        let pages = (size - 1) / interval;
        let rest = size - pages * interval;
        let mut walked = false;
        if pages <= UNROLLED {
            for _ in 0..pages {
                let inst = self.sub(interval);
                out.push(inst);
                *below += offset(interval);
                if from_sp {
                    self.row(inst, CfiOp::DefCfaOffset(*below));
                }
                let touch = self.touch(probing.probe);
                out.push(touch);
            }
        } else {
            self.pages(out, pages, below, from_sp, probing);
            walked = from_sp;
        }
        let inst = self.sub(rest);
        out.push(inst);
        *below += offset(rest);
        if from_sp {
            // A loop leaves the address counted from the register the stack pointer was compared
            // against, since that is the one thing in it that holds still. This is where it goes
            // back to being counted from the stack pointer, and it is written behind this
            // instruction rather than behind the branch because a row is written behind an
            // instruction and the branch is not one that survives [`crate::layout`].
            let op = if walked {
                let number = self.dwarf(self.conv.int_class, self.conv.stack_pointer);
                CfiOp::DefCfa { reg: number, offset: *below }
            } else {
                CfiOp::DefCfaOffset(*below)
            };
            self.row(inst, op);
        }
    }

    /// The loop that takes a frame too large for the touches to be written one after another.
    ///
    /// Three blocks, and the first two are new and go in front of the one the function began with:
    ///
    /// ```text
    ///   what the function is entered at   everything the prologue did before this, and then the
    ///                                     address the stack pointer is walking down to
    ///   the loop                          one page, the touch, and the question of whether the
    ///                                     stack pointer has got there yet
    ///   what the function began with      the rest of the prologue, and then the body
    /// ```
    ///
    /// The instructions the prologue has written so far move into the first of them, because a
    /// block is entered at the top and they have to run before the loop does. Nothing is laid out
    /// here: which block comes first in memory is [`crate::layout`]'s answer, and all this decides
    /// is which one the function is entered at.
    fn pages(
        &mut self,
        out: &mut Vec<Inst>,
        pages: u32,
        below: &mut i32,
        from_sp: bool,
        probing: Probing<'_>,
    ) {
        let class = self.conv.int_class;
        let sp = self.conv.stack_pointer;
        let all = offset(pages * probing.probe.interval);
        let [limit, byte] = probing.scratch;

        let head = self.func.create_block();
        for &inst in out.iter() {
            self.func.append_inst(head, inst);
        }
        out.clear();
        // Where the stack pointer is walking down to, worked out before it starts moving. A loop
        // that counted down instead would need somewhere to keep the count, and this is somewhere
        // to keep it that the comparison can read without arithmetic.
        let lea = self.opcode(self.insts.lea);
        let inst = self.address(lea, limit, sp, -all);
        self.func.append_inst(head, inst);
        if from_sp {
            // The address is counted from that register for as long as the loop runs, and it has
            // to be: the stack pointer moves once an iteration, so no fixed distance from it is
            // true twice, and this register was written so that one distance is.
            let number = self.dwarf(class, limit);
            self.row(inst, CfiOp::DefCfa { reg: number, offset: *below + all });
        }

        let body = self.func.create_block();
        *self.func.succs_mut(head) = vec![BlockCall::to(body)];
        let inst = self.sub(probing.probe.interval);
        self.func.append_inst(body, inst);
        let touch = self.touch(probing.probe);
        self.func.append_inst(body, touch);
        let differ = self.opcode(self.insts.differ);
        let inst = self
            .func
            .build_loose(differ)
            .def(Reg::physical(byte), class)
            .uses(Reg::physical(sp), class)
            .uses(Reg::physical(limit), class)
            .finish();
        self.func.append_inst(body, inst);
        let cond = Opcode::new(
            self.names.intern(&format!("{}{}", probing.branch.prefix, probing.branch.cond)),
        );
        let inst = self.func.build_loose(cond).uses(Reg::physical(byte), class).finish();
        self.func.append_inst(body, inst);
        // The first arm is the one taken when the condition held, and the condition is that the
        // stack pointer and the address it is walking down to still differ, so the first arm is
        // another page.
        let began = self.func.entry().expect("a function with a block in it");
        *self.func.succs_mut(body) = vec![BlockCall::to(body), BlockCall::to(began)];
        *below += all;
        self.ahead = Some([head, body]);
    }

    /// Writes the page the stack pointer is on without changing what is there.
    fn touch(&mut self, probe: &Probe) -> Inst {
        let opcode = self.opcode(probe.inst);
        let base = Operand::read(Reg::physical(self.conv.stack_pointer), self.conv.int_class);
        self.func.build_loose(opcode).imm(0).mem(Mem::at(base)).finish()
    }

    /// Takes that many bytes off the stack pointer.
    fn sub(&mut self, bytes: u32) -> Inst {
        let sub = self.opcode(self.insts.sub);
        self.arith(sub, i64::from(bytes))
    }

    /// The stack protector's check, written at the end of a block the function returns from.
    ///
    /// Gives back the block the epilogue goes in, which is a new one: the check has to be the last
    /// thing the old block does, and what follows it is one of two arms rather than the return.
    ///
    /// ```text
    ///   block that returned      reload the slot, read the word again, compare, branch
    ///   the arm it changed on    call the function that does not come back, and nothing after
    ///   the arm it did not       the epilogue, which the caller writes into what this gives back
    /// ```
    ///
    /// The two registers are the ones the allocator was told to hold back, so nothing here has to
    /// ask what is live: a scratch register holds nothing at the end of a block, because the only
    /// thing that writes one is a move the rewriter put in and every one of those is read by the
    /// instruction it was put in front of.
    fn check(&mut self, block: Block, frame: &Frame, protect: Protect<'_>) -> Block {
        let class = self.conv.int_class;
        let at = frame.canary().expect("a protected function has a slot for its canary");
        let [ours, theirs] = protect.scratch;

        let inst = self.load(class, ours, at);
        self.func.append_inst(block, inst);
        let inst = self.read_guard(theirs, protect.guard);
        self.func.append_inst(block, inst);
        let differ = self.opcode(self.insts.differ);
        let inst = self
            .func
            .build_loose(differ)
            .def(Reg::physical(theirs), class)
            .uses(Reg::physical(ours), class)
            .uses(Reg::physical(theirs), class)
            .finish();
        self.func.append_inst(block, inst);

        let failed = self.func.create_block();
        let ok = self.func.create_block();
        let cond = Opcode::new(
            self.names.intern(&format!("{}{}", protect.branch.prefix, protect.branch.cond)),
        );
        let inst = self.func.build_loose(cond).uses(Reg::physical(theirs), class).finish();
        self.func.append_inst(block, inst);
        // The first arm is the one taken when the condition held, and the condition is that the
        // two words differ, so the first arm is the one the canary was overwritten on.
        *self.func.succs_mut(block) = vec![BlockCall::to(failed), BlockCall::to(ok)];

        let call = self.opcode(self.insts.call);
        let symbol = self.names.intern(protect.guard.fail);
        self.func.build(failed, call).symbol(symbol).finish();
        ok
    }

    /// Reads the word the canary is a copy of into a register.
    ///
    /// The address is a constant and names no register at all, because where the block a thread
    /// has to itself begins is something only the machine knows and the segment register is what
    /// holds it.
    fn read_guard(&mut self, into: PhysReg, guard: &Guard) -> Inst {
        let class = self.conv.int_class;
        let load = self.opcode(self.insts.moves(class).expect("a class to load").load);
        self.func
            .build_loose(load)
            .def(Reg::physical(into), class)
            .mem(Mem::in_segment(guard.segment, guard.at))
            .finish()
    }

    /// The instructions the epilogue is, in the order they run.
    ///
    /// The vector registers are read back while the stack pointer is still where the body left it,
    /// because that is what their offsets are from. Then the stack pointer goes back to the last
    /// register the prologue pushed, which is arithmetic when the prologue knew how far it had
    /// moved and a read of the frame pointer when it did not.
    fn epilogue(&mut self, frame: &Frame) -> Vec<Inst> {
        let sp = self.conv.stack_pointer;
        let fp = self.conv.frame_pointer;
        let int = self.conv.int_class;
        let sse = self.conv.sse_class;
        let word = self.conv.word;
        let described = !self.func.cfi.is_empty();
        let mut out = Vec::new();
        // Where the body left things, which is where every epilogue starts from.
        let mut below = offset(self.conv.return_address)
            + offset(word) * self.pushes(frame)
            + offset(frame.size());
        let from_sp = !frame.frame_pointer();
        for save in frame.saved_sse() {
            let inst = self.load(sse, save.reg, save.at);
            out.push(inst);
            if frame.realign().is_none() {
                self.restored(inst, sse, save.reg);
            }
        }
        let pushed = u32::try_from(frame.saved_int().len()).expect("a frame");
        if frame.frame_pointer() {
            // No row for either of these. The address is counted from the frame pointer here and
            // this is what moves the stack pointer rather than the frame pointer, so the rule that
            // was true before it is still true after it.
            if pushed == 0 {
                let mov = self.opcode(self.insts.moves(int).expect("a move").mov);
                out.push(self.two(mov, sp, fp));
            } else {
                let lea = self.opcode(self.insts.lea);
                let back = -offset(word * pushed);
                out.push(self.address(lea, sp, fp, back));
            }
        } else if frame.size() > 0 {
            let add = self.opcode(self.insts.add);
            let inst = self.arith(add, i64::from(frame.size()));
            out.push(inst);
            below -= offset(frame.size());
            self.row(inst, CfiOp::DefCfaOffset(below));
        }
        for &reg in frame.saved_int().iter().rev() {
            let inst = self.pop(reg);
            out.push(inst);
            self.restored(inst, int, reg);
            below -= offset(word);
            if from_sp {
                self.row(inst, CfiOp::DefCfaOffset(below));
            }
        }
        if frame.frame_pointer() {
            let inst = self.pop(fp);
            out.push(inst);
            self.restored(inst, int, fp);
            // The frame pointer holds the caller's value again, so the address goes back to being
            // counted from the stack pointer, which by now is at the return address.
            let number = self.dwarf(int, sp);
            self.row(inst, CfiOp::DefCfa { reg: number, offset: offset(self.conv.return_address) });
        }
        let ret = self.opcode(self.insts.ret);
        let inst = self.func.build_loose(ret).finish();
        out.push(inst);
        // These take effect at the address just past the return, which is where the next block
        // begins, and the next block is body again. Popping the body's rules and pushing them
        // straight back leaves the stack one deep however many blocks the function returns from,
        // which is what makes one remembering in the prologue enough for all of them.
        if described {
            self.row(inst, CfiOp::RestoreState);
            self.row(inst, CfiOp::RememberState);
        }
        out
    }

    /// How many general purpose registers the prologue put on the stack, the frame pointer
    /// included.
    fn pushes(&self, frame: &Frame) -> i32 {
        let saved = i32::try_from(frame.saved_int().len()).expect("a frame");
        saved + i32::from(frame.frame_pointer())
    }

    /// One row of the unwind table, taking effect after that instruction.
    fn row(&mut self, inst: Inst, op: CfiOp) {
        self.func.cfi.push((inst, op));
    }

    /// A row saying the caller's copy of that register is that far from the canonical frame
    /// address, which is below it and so is negative.
    fn saved(&mut self, inst: Inst, class: RegClass, reg: PhysReg, from_cfa: i32) {
        let number = self.dwarf(class, reg);
        self.row(inst, CfiOp::Offset { reg: number, offset: from_cfa });
    }

    /// A row saying that register holds what the caller left in it again.
    fn restored(&mut self, inst: Inst, class: RegClass, reg: PhysReg) {
        let number = self.dwarf(class, reg);
        self.row(inst, CfiOp::Restore(number));
    }

    /// What an unwind table calls that register.
    fn dwarf(&self, class: RegClass, reg: PhysReg) -> u16 {
        self.conv.dwarf(class, reg).expect("a register a frame saves is one the table can name")
    }

    /// One edit as the instruction that makes it true.
    fn mov(&mut self, edit: &Edit, frame: &Frame) -> Inst {
        let moves = self.insts.moves(edit.class).expect("a class the target says how to move");
        match (edit.mov.to, edit.mov.from) {
            (Place::Reg(to), Place::Reg(from)) => {
                let mov = self.opcode(moves.mov);
                self.func
                    .build_loose(mov)
                    .def(Reg::physical(to), edit.class)
                    .uses(Reg::physical(from), edit.class)
                    .finish()
            }
            (Place::Reg(to), Place::Slot(slot)) => {
                let at = self.slot(frame, slot);
                self.load(edit.class, to, at)
            }
            (Place::Slot(slot), Place::Reg(from)) => {
                let at = self.slot(frame, slot);
                self.store(edit.class, from, at)
            }
            // The allocator expands this into two moves through a register of its own, because a
            // machine that could do it in one is not a machine any of this is written for.
            (Place::Slot(_), Place::Slot(_)) => {
                unreachable!("a move from one stack slot straight into another")
            }
        }
    }

    /// Puts an instruction where an edit says it goes, after whatever earlier edits went there.
    ///
    /// The edits at one place are in the order they have to be made in, so each one goes behind
    /// the last, and the first of them is what the place itself means.
    fn put(&mut self, cursors: &mut HashMap<At, Inst>, at: At, inst: Inst) {
        if let Some(cursor) = cursors.get_mut(&at) {
            self.func.insert_after(*cursor, inst);
            *cursor = inst;
            return;
        }
        match at {
            At::Before(before) => self.func.insert_before(before, inst),
            At::After(after) => self.func.insert_after(after, inst),
            At::StartOf(block) => self.func.prepend_inst(block, inst),
            // Behind everything in the block. A block the allocator puts an edge's moves at the
            // end of is one with a single edge out of it, and an edge like that is not an
            // instruction here: [`crate::layout`] writes the jump it becomes after this has run.
            // So the last instruction is an ordinary one, which may still be waiting on moves of
            // its own that have to be made before the edge's are.
            At::EndOf(block) => self.func.append_inst(block, inst),
        }
        cursors.insert(at, inst);
    }

    /// Where a spill slot is, from the stack pointer in the body of the function.
    fn slot(&self, frame: &Frame, slot: u32) -> i32 {
        frame.slot(slot).expect("a slot the frame was worked out from")
    }

    /// Reads a register out of the frame.
    fn load(&mut self, class: RegClass, reg: PhysReg, at: i32) -> Inst {
        let load = self.opcode(self.insts.moves(class).expect("a class to load").load);
        let base = Operand::read(Reg::physical(self.base), self.conv.int_class);
        self.func
            .build_loose(load)
            .def(Reg::physical(reg), class)
            .mem(Mem::at(base).plus(at))
            .finish()
    }

    /// Writes a register into the frame.
    fn store(&mut self, class: RegClass, reg: PhysReg, at: i32) -> Inst {
        let store = self.opcode(self.insts.moves(class).expect("a class to store").store);
        let base = Operand::read(Reg::physical(self.base), self.conv.int_class);
        self.func
            .build_loose(store)
            .uses(Reg::physical(reg), class)
            .mem(Mem::at(base).plus(at))
            .finish()
    }

    /// Puts a general purpose register on the stack.
    fn push(&mut self, reg: PhysReg) -> Inst {
        let push = self.opcode(self.insts.push);
        self.func.build_loose(push).uses(Reg::physical(reg), self.conv.int_class).finish()
    }

    /// Takes a general purpose register back off the stack.
    fn pop(&mut self, reg: PhysReg) -> Inst {
        let pop = self.opcode(self.insts.pop);
        self.func.build_loose(pop).def(Reg::physical(reg), self.conv.int_class).finish()
    }

    /// One general purpose register written with another.
    fn two(&mut self, opcode: Opcode, to: PhysReg, from: PhysReg) -> Inst {
        let class = self.conv.int_class;
        self.func
            .build_loose(opcode)
            .def(Reg::physical(to), class)
            .uses(Reg::physical(from), class)
            .finish()
    }

    /// Two-address arithmetic on the stack pointer, which reads it and writes it back.
    fn arith(&mut self, opcode: Opcode, value: i64) -> Inst {
        let class = self.conv.int_class;
        let sp = Reg::physical(self.conv.stack_pointer);
        self.func.build_loose(opcode).def(sp, class).uses(sp, class).imm(value).finish()
    }

    /// One register written with an address rather than with what is at it.
    fn address(&mut self, opcode: Opcode, to: PhysReg, base: PhysReg, disp: i32) -> Inst {
        let class = self.conv.int_class;
        let base = Operand::read(Reg::physical(base), class);
        self.func
            .build_loose(opcode)
            .def(Reg::physical(to), class)
            .mem(Mem::at(base).plus(disp))
            .finish()
    }

    /// The opcode of that name, in the machine IR's spelling, which is the target's prefix and
    /// then the name the target gave.
    fn opcode(&mut self, name: &str) -> Opcode {
        Opcode::new(self.names.intern(&format!("{}{name}", self.insts.prefix)))
    }
}

/// A distance in a frame, as the signed number every offset is.
fn offset(bytes: u32) -> i32 {
    i32::try_from(bytes).expect("a frame under two gigabytes")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, print_func};
    use rucc_regalloc::assign::Env;
    use rucc_target::x86_64::{BRANCH, FRAME, GPR, PROBE, R10, R11, REGS, SYSV, WIN64, XMM, xmm};

    use super::*;
    use crate::frame::{Layout, Local};

    /// An environment offering that many of the convention's registers, with everything after
    /// them held back as scratch.
    fn env(conv: &CallRegs, count: usize) -> Env {
        Env::new().with(GPR, &conv.int_order[..count], &conv.int_order[count..])
    }

    /// A function of that many values, every one written before any is read, allocated with that
    /// many registers to hand out. The same shape the frame layout's own tests are written
    /// against, so that a frame here is one that has already been checked there.
    fn pressure(conv: &CallRegs, values: usize, count: usize) -> (Func, Allocation, Interner) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let regs: Vec<Reg> = (0..values).map(|_| func.new_vreg(GPR)).collect();
        for &reg in &regs {
            func.build(block, opcode).def(reg, GPR).finish();
        }
        for &reg in &regs {
            func.build(block, opcode).uses(reg, GPR).finish();
        }
        let allocation = rucc_regalloc::run(&mut func, &env(conv, count), "test");
        (func, allocation, names)
    }

    /// The function with its frame written into it, as the lines a dump would show.
    fn written(
        func: &mut Func,
        allocation: &Allocation,
        layout: &Layout<'_>,
        names: &mut Interner,
    ) -> Vec<String> {
        with_protector(func, allocation, layout, None, names)
    }

    /// The same, for a function the caller has decided is protected or is not.
    fn with_protector(
        func: &mut Func,
        allocation: &Allocation,
        layout: &Layout<'_>,
        protect: Option<Protect<'_>>,
        names: &mut Interner,
    ) -> Vec<String> {
        let convention = Convention { protect, ..Convention::new(layout.conv, &FRAME) };
        under(func, allocation, layout, convention, names)
    }

    /// The same, for a function whose frame the caller has decided is taken a page at a time.
    fn with_probing(
        func: &mut Func,
        allocation: &Allocation,
        layout: &Layout<'_>,
        probe: Option<Probing<'_>>,
        names: &mut Interner,
    ) -> Vec<String> {
        let convention = Convention { probe, ..Convention::new(layout.conv, &FRAME) };
        under(func, allocation, layout, convention, names)
    }

    /// The function with its frame written into it under that convention.
    fn under(
        func: &mut Func,
        allocation: &Allocation,
        layout: &Layout<'_>,
        convention: Convention<'_>,
        names: &mut Interner,
    ) -> Vec<String> {
        let frame = Frame::of(func, allocation, layout);
        finish(func, allocation, &frame, &Stack::default(), convention, names);
        print_func(func, names, &REGS)
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.trim().to_string())
            .collect()
    }

    /// Just the lines the frame put in, which is every line that is not the function it was
    /// given and not the shape of the dump around it.
    fn added(lines: &[String]) -> Vec<&str> {
        lines
            .iter()
            .map(String::as_str)
            .filter(|line| !line.contains("x64.nop"))
            .filter(|line| !line.starts_with("mfunc") && !line.starts_with("block") && *line != "}")
            .collect()
    }

    #[test]
    fn a_function_that_needs_no_frame_is_given_a_return_and_nothing_else() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 2, 4);
        let lines = written(&mut func, &allocation, &Layout::new(&SYSV, REGS), &mut names);

        // Two values and four registers, so nothing is spilled, nothing is saved and the stack
        // pointer never moves. A prologue of nothing is the right prologue for that.
        assert_eq!(added(&lines), ["x64.ret"]);
    }

    #[test]
    fn a_spill_is_a_store_and_a_reload_is_a_load() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 4, 2);
        let lines = written(&mut func, &allocation, &Layout::new(&SYSV, REGS), &mut names);

        // Two registers for four values, so two of them go to the stack. The store goes behind the
        // instruction that wrote the value and the load in front of the one that wants it, both at
        // the offsets the frame gave, which are below the stack pointer because a small leaf
        // function is entitled to the red zone.
        assert_eq!(
            lines,
            [
                "mfunc @f {",
                "block0:",
                "$rax = x64.nop",
                "$rcx = x64.nop",
                "$rdx = x64.nop",
                "x64.mov_mr_64 $rdx, [$rsp - 16]",
                "$rdx = x64.nop",
                "x64.mov_mr_64 $rdx, [$rsp - 8]",
                "x64.nop $rax",
                "x64.nop $rcx",
                "$rdx = x64.mov_rm_64 [$rsp - 16]",
                "x64.nop $rdx",
                "$rdx = x64.mov_rm_64 [$rsp - 8]",
                "x64.nop $rdx",
                "x64.ret",
                "}",
            ]
        );
    }

    #[test]
    fn the_frame_the_prologue_takes_is_the_frame_the_epilogue_gives_back() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 4, 2);
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { red_zone: false, ..base };
        let lines = written(&mut func, &allocation, &layout, &mut names);

        // The same function told it may not use the red zone takes sixteen bytes instead, and
        // every offset moves above the stack pointer to match.
        assert_eq!(
            added(&lines),
            [
                "$rsp = x64.sub_ri_64 $rsp, 16",
                "x64.mov_mr_64 $rdx, [$rsp]",
                "x64.mov_mr_64 $rdx, [$rsp + 8]",
                "$rdx = x64.mov_rm_64 [$rsp]",
                "$rdx = x64.mov_rm_64 [$rsp + 8]",
                "$rsp = x64.add_ri_64 $rsp, 16",
                "x64.ret",
            ]
        );
    }

    #[test]
    fn the_registers_the_prologue_pushes_come_back_in_the_opposite_order() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 13, 13);
        let lines = written(&mut func, &allocation, &Layout::new(&SYSV, REGS), &mut names);

        // Four registers a call leaves alone, pushed in the convention's order and popped in the
        // other one, which is the only order that gets each of them its own value back.
        assert_eq!(
            added(&lines),
            [
                "x64.push_64 $rbx",
                "x64.push_64 $r12",
                "x64.push_64 $r13",
                "x64.push_64 $r14",
                "$r14 = x64.pop_64",
                "$r13 = x64.pop_64",
                "$r12 = x64.pop_64",
                "$rbx = x64.pop_64",
                "x64.ret",
            ]
        );
    }

    #[test]
    fn a_function_that_keeps_a_frame_pointer_sets_it_up_and_leaves_by_it() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 4, 2);
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { frame_pointer: true, red_zone: false, ..base };
        let lines = written(&mut func, &allocation, &layout, &mut names);

        // The frame pointer is saved before anything else and points at where it was saved, so the
        // epilogue reaches the stack pointer through it rather than by counting the frame back.
        assert_eq!(
            added(&lines),
            [
                "x64.push_64 $rbp",
                "$rbp = x64.mov_rr_64 $rsp",
                "$rsp = x64.sub_ri_64 $rsp, 16",
                "x64.mov_mr_64 $rdx, [$rsp]",
                "x64.mov_mr_64 $rdx, [$rsp + 8]",
                "$rdx = x64.mov_rm_64 [$rsp]",
                "$rdx = x64.mov_rm_64 [$rsp + 8]",
                "$rsp = x64.mov_rr_64 $rbp",
                "$rbp = x64.pop_64",
                "x64.ret",
            ]
        );
    }

    #[test]
    fn a_realigned_frame_forces_the_alignment_after_it_has_pushed_what_it_saves() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 13, 13);
        let locals = [Local { size: 64, align: 32 }];
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { locals: &locals, ..base };
        let lines = written(&mut func, &allocation, &layout, &mut names);

        // Forcing the alignment throws away how far the stack pointer had moved, so the registers
        // are pushed before it happens and the epilogue counts back from the frame pointer to find
        // them. The frame pointer is required here whatever the flags said.
        assert_eq!(
            added(&lines),
            [
                "x64.push_64 $rbp",
                "$rbp = x64.mov_rr_64 $rsp",
                "x64.push_64 $rbx",
                "x64.push_64 $r12",
                "x64.push_64 $r13",
                "x64.push_64 $r14",
                "$rsp = x64.and_ri_64 $rsp, -32",
                "$rsp = x64.sub_ri_64 $rsp, 64",
                "$rsp = x64.lea_64 [$rbp - 32]",
                "$r14 = x64.pop_64",
                "$r13 = x64.pop_64",
                "$r12 = x64.pop_64",
                "$rbx = x64.pop_64",
                "$rbp = x64.pop_64",
                "x64.ret",
            ]
        );
    }

    #[test]
    fn every_block_the_function_returns_from_gets_an_epilogue() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let left = func.create_block();
        let right = func.create_block();
        func.build(head, opcode).finish();
        *func.succs_mut(head) = vec![BlockCall::to(left), BlockCall::to(right)];
        func.build(left, opcode).finish();
        func.build(right, opcode).finish();
        let allocation = rucc_regalloc::run(&mut func, &env(&SYSV, 4), "test");
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { leaf: false, ..base };
        let lines = written(&mut func, &allocation, &layout, &mut names);

        // Both ways out get the frame given back, and the block that goes somewhere gets nothing,
        // because a block with an edge out of it is not a block anything returns from.
        assert_eq!(
            lines,
            [
                "mfunc @f {",
                "block0:",
                "$rsp = x64.sub_ri_64 $rsp, 8",
                "x64.nop block1, block2",
                "block1:",
                "x64.nop",
                "$rsp = x64.add_ri_64 $rsp, 8",
                "x64.ret",
                "block2:",
                "x64.nop",
                "$rsp = x64.add_ri_64 $rsp, 8",
                "x64.ret",
                "}",
            ]
        );
    }

    #[test]
    fn a_protected_function_writes_the_canary_last_and_checks_it_before_it_returns() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 4, 2);
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { leaf: false, protect: true, ..base };
        let guard = SYSV.guard.as_ref().expect("this convention has somewhere to keep the word");
        // The two the real pipeline holds back, which are held back in the environment above too:
        // it hands out the first two of the convention's order and keeps everything after them.
        let protect = Protect { guard, branch: &BRANCH, scratch: [R10, R11] };
        let lines = with_protector(&mut func, &allocation, &layout, Some(protect), &mut names);

        // The read of the word and the store into the slot come after the stack pointer has moved,
        // because there is no slot to store into until it has. The check is the last thing the
        // block that returned does and the epilogue is on the arm the canary was unchanged on, so
        // a function whose canary changed never gives its frame back and never returns.
        assert_eq!(
            added(&lines),
            [
                "$rsp = x64.sub_ri_64 $rsp, 24",
                "$r10 = x64.mov_rm_64 [fs:40]",
                "x64.mov_mr_64 $r10, [$rsp + 16]",
                "x64.mov_mr_64 $rdx, [$rsp]",
                "x64.mov_mr_64 $rdx, [$rsp + 8]",
                "$rdx = x64.mov_rm_64 [$rsp]",
                "$rdx = x64.mov_rm_64 [$rsp + 8]",
                "$r10 = x64.mov_rm_64 [$rsp + 16]",
                "$r11 = x64.mov_rm_64 [fs:40]",
                "$r11 = x64.cmp_set_ne_64 $r10, $r11",
                "x64.br_cond_8 $r11, block1, block2",
                "x64.call @__stack_chk_fail",
                "$rsp = x64.add_ri_64 $rsp, 24",
                "x64.ret",
            ]
        );
    }

    #[test]
    fn a_frame_that_fits_in_one_page_is_taken_in_one_subtraction_even_when_pages_are_touched() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 2, 4);
        let locals = [Local { size: 4088, align: 16 }];
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { leaf: false, locals: &locals, ..base };
        let probing = Probing { probe: &PROBE, branch: &BRANCH, scratch: [R10, R11] };
        let lines = with_probing(&mut func, &allocation, &layout, Some(probing), &mut names);

        // A frame of one page cannot step over the page below it, because the far end of it is the
        // near end of that page and anything written there is written to a page that is there. So
        // the flag costs such a function nothing, which is most functions.
        assert_eq!(
            added(&lines),
            ["$rsp = x64.sub_ri_64 $rsp, 4088", "$rsp = x64.add_ri_64 $rsp, 4088", "x64.ret",]
        );
    }

    #[test]
    fn a_probing_prologue_touches_every_page_of_a_frame_a_few_pages_deep() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 2, 4);
        let locals = [Local { size: 9000, align: 16 }];
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { leaf: false, locals: &locals, ..base };
        let probing = Probing { probe: &PROBE, branch: &BRANCH, scratch: [R10, R11] };
        let lines = with_probing(&mut func, &allocation, &layout, Some(probing), &mut names);

        // A page of the stack pointer's own, then the touch that says the page is there, and only
        // then the next one, which is the whole of the defence: nothing here ever moves the stack
        // pointer further than one page without writing where it landed. The last subtraction is
        // the remainder and is smaller than a page, so it needs no touch of its own, and it exists
        // in every frame because the count of pages is taken off one less than the size.
        assert_eq!(
            added(&lines),
            [
                "$rsp = x64.sub_ri_64 $rsp, 4096",
                "x64.or_mi_8 [$rsp], 0",
                "$rsp = x64.sub_ri_64 $rsp, 4096",
                "x64.or_mi_8 [$rsp], 0",
                "$rsp = x64.sub_ri_64 $rsp, 808",
                "$rsp = x64.add_ri_64 $rsp, 9000",
                "x64.ret",
            ]
        );
    }

    #[test]
    fn a_probing_prologue_deeper_than_that_walks_the_pages_in_a_loop() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 2, 4);
        let locals = [Local { size: 100_000, align: 16 }];
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { leaf: false, locals: &locals, ..base };
        let probing = Probing { probe: &PROBE, branch: &BRANCH, scratch: [R10, R11] };
        let lines = with_probing(&mut func, &allocation, &layout, Some(probing), &mut names);

        // Twenty-four pages, which is more than a straight line is worth, so the prologue works out
        // where it is going first and then walks there. The whole listing rather than the added
        // lines, because what matters as much as the instructions is that the two blocks the walk
        // is made of come in front of the block the function began with: the body the allocator
        // filled is block2 here and it was block0 before this ran.
        assert_eq!(
            lines,
            [
                "mfunc @f {",
                "block0:",
                "$r10 = x64.lea_64 [$rsp - 98304], block1",
                "block1:",
                "$rsp = x64.sub_ri_64 $rsp, 4096",
                "x64.or_mi_8 [$rsp], 0",
                "$r11 = x64.cmp_set_ne_64 $rsp, $r10",
                "x64.br_cond_8 $r11, block1, block2",
                "block2:",
                "$rsp = x64.sub_ri_64 $rsp, 1704",
                "$rax = x64.nop",
                "$rcx = x64.nop",
                "x64.nop $rax",
                "x64.nop $rcx",
                "$rsp = x64.add_ri_64 $rsp, 100008",
                "x64.ret",
                "}",
            ]
        );
    }

    #[test]
    fn a_vector_register_a_windows_call_preserves_is_stored_and_read_back() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        // An instruction that writes one of the vector registers Windows preserves, which is what
        // a rule for something that has to use it produces.
        func.build(block, opcode).operand(Operand::write(Reg::physical(xmm(6)), XMM)).finish();
        let allocation = rucc_regalloc::run(&mut func, &env(&WIN64, 4), "test");
        let lines = written(&mut func, &allocation, &Layout::new(&WIN64, REGS), &mut names);

        // No machine here pushes a vector register, so it is stored into the frame rather than
        // pushed, and the frame has to be taken before there is anywhere to put it.
        assert_eq!(
            added(&lines),
            [
                "$rsp = x64.sub_ri_64 $rsp, 24",
                "x64.movaps_mr $xmm6, [$rsp]",
                "$xmm6 = x64.movaps_rm [$rsp]",
                "$rsp = x64.add_ri_64 $rsp, 24",
                "x64.ret",
            ]
        );
    }
}
