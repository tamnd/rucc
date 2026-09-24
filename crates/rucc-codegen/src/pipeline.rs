//! One IR function to one machine function, which is every pass in this crate in order.
//!
//! Design: `spec/10-backend.md` section 10.1, which is where the order comes from.
//!
//! Each pass here is written and tested on its own and each is useful on its own, but there is
//! exactly one order they run in and until now that order lived in the tests. A caller outside
//! this crate would have had to know that splitting critical edges comes after lowering and
//! before allocation, that the frame is worked out after allocation because the spill slots are
//! the largest thing in it, and that the prologue is written after the frame. None of that is a
//! decision a driver should be making, so it is written down once, here.
//!
//! # What comes out
//!
//! A function whose every register is physical, whose every offset into the frame is a constant,
//! and whose blocks are in the order they run in with the jumps that order needs. That is the
//! point at which a function is one an encoder could read, and there is nothing left in it that
//! is not an instruction of the machine it was compiled for.
//!
//! # What is still missing from the middle
//!
//! The optimizing path, all of it. What runs here is `spec/10-backend.md` section 10.3's fast
//! path: one rule per term, a linear scan, and a block order from the shape of the CFG rather
//! than from block frequency. No scheduling, and the redundant moves a coalescer would take out
//! are still in the output.

use rucc_base::Interner;
use rucc_cost::Goal;
use rucc_ir as ir;
use rucc_mir as mir;
use rucc_regalloc::assign::Env;
use rucc_target::{
    BitInsts, BranchInsts, CallRegs, FlagInsts, FrameInsts, MachineInsts, PhysReg, RegFile,
    ShortInsts, TargetInfo, TimingInsts, aarch64, x86_64,
};
use rucc_tuple::Arch;

use crate::bits;
use crate::choice;
use crate::combine;
use crate::compare;
use crate::copies;
use crate::coverage::Fired;
use crate::elsewhere::Elsewhere;
use crate::finish::{Convention, Padding, Probing, Protect, Tracing, finish};
use crate::fold;
use crate::frame::{self, Frame, Layout};
use crate::kept;
use crate::layout;
use crate::lower::{self, Unsupported};
use crate::lowering::{self, Lowerings};
use crate::pressure::{Cost, Pressure};
use crate::schedule;
use crate::select::{self, Selector};
use crate::shorten;
use crate::slots::{self, Slots};
use crate::split;
use crate::weights;

/// Everything about a machine that compiling a function for it needs.
///
/// The fields are different kinds of fact and they come from different places: where the
/// convention puts things, what registers the machine has, which instructions build a frame,
/// which instructions a branch becomes, and which registers the allocator may hand out. The last
/// one is not a target fact on its own, because holding a register back as scratch is a decision
/// about the allocator rather than about the machine, which is why it is built here rather than
/// in [`rucc_target`].
#[derive(Debug)]
pub struct Machine {
    /// Where the convention this function is compiled for puts things.
    pub conv: &'static CallRegs,
    /// The registers the machine has, which is what says how wide a spill slot of a class is.
    pub file: RegFile,
    /// The instructions that take a frame and give it back.
    pub insts: &'static FrameInsts,
    /// The instructions a branch becomes once the blocks are in an order.
    pub branch: &'static BranchInsts,
    /// How much of a register each of the machine's instructions reads and writes.
    pub bits: &'static BitInsts,
    /// What each of the machine's instructions leaves in the condition state.
    pub flags: &'static FlagInsts,
    /// What shape each of the machine's instructions is, which is what a pass proposing a new one
    /// has its proposal held against.
    pub shapes: &'static MachineInsts,
    /// How long each of the machine's instructions takes, and what it takes it on.
    pub timing: &'static TimingInsts,
    /// Which of the machine's instructions have a shorter spelling of the same answer.
    pub short: &'static ShortInsts,
    /// What the selector asks of the machine, which is the rules and the instructions it writes
    /// itself.
    pub selector: &'static Selector,
    /// What the allocator may hand out, and what it holds back.
    pub env: Env,
}

/// The scratch registers held back from the allocator on x86-64.
///
/// Two, because a move on an edge may have to break a cycle and a spilled value has to be read
/// into something, and those can want a register at the same instruction. Two is also what nearly
/// every instruction wants, including the one that looks larger: an instruction that reads two
/// spilled values and writes a third sends the answer back into a register an operand arrived in
/// rather than asking for one of its own, and `rewrite` says why that is allowed.
///
/// It is not two because two was enough to start with and nobody looked again. There is no third
/// to hold back. A scratch register has to be one the convention passes nothing in, since the
/// rewriter puts moves in wherever it likes, and one the callee does not owe back, since the
/// rewriter runs after the prologue has been decided and cannot ask for a register to be saved. On
/// SysV that is `r10`, `r11` and `rax`, and `rax` is not one to take: it is the return value, so
/// holding it back costs a move at every return in the program, which is a price paid everywhere
/// for a shape that turns up almost nowhere.
///
/// An instruction that wants a third is the indexed store with its base, its index and its value
/// all on the stack, which is tamnd/rucc#913. `rewrite` answers that one by borrowing a register
/// and putting back what was in it, which costs two memory accesses at the instruction that wanted
/// it and nothing anywhere else.
pub(crate) const SCRATCH: [PhysReg; 2] = [x86_64::R10, x86_64::R11];

/// The scratch registers held back from the allocator on AArch64. See [`Machine::aarch64`].
const AARCH64_SCRATCH: [PhysReg; 2] = [aarch64::X16, aarch64::X17];

/// How many of each class are held back.
const SCRATCH_COUNT: usize = SCRATCH.len();

/// The second register file's allocation order and the scratch registers taken out of it, which
/// are the last two in the order that the convention does not preserve.
fn held_back(conv: &CallRegs) -> (Vec<PhysReg>, Vec<PhysReg>) {
    let free: Vec<PhysReg> =
        conv.sse_order.iter().copied().filter(|&reg| !conv.preserves_sse(reg)).collect();
    let at = free.len().saturating_sub(SCRATCH_COUNT);
    let scratch: Vec<PhysReg> = free[at..].to_vec();
    let order = conv.sse_order.iter().copied().filter(|reg| !scratch.contains(reg)).collect();
    (order, scratch)
}

impl Machine {
    /// The x86-64 machine under that convention.
    ///
    /// Both files are offered. A value the selector produces is in one or the other, which is
    /// decided by its type: an integer and an address are general purpose and a `float` or a
    /// `double` is in a vector register, and the allocator is given each file separately because
    /// no move goes between them.
    #[must_use]
    pub fn x86_64(conv: &'static CallRegs) -> Self {
        let order: Vec<PhysReg> =
            conv.int_order.iter().copied().filter(|reg| !SCRATCH.contains(reg)).collect();
        // The vector file wants its own two, for the same two jobs, and they have to be two the
        // convention does not preserve: a scratch register is written by a move the rewriter puts
        // in, which is after the prologue has already been decided, so one the callee owes back
        // would be one nothing saved. That rules out the upper ten on Windows and nothing at all
        // on SysV, and taking the last two that are left lands on `xmm14` and `xmm15` there and on
        // `xmm4` and `xmm5` on Windows, neither of which any argument travels in.
        let (sse_order, sse_scratch) = held_back(conv);
        Self {
            conv,
            file: x86_64::REGS,
            insts: &x86_64::FRAME,
            branch: &x86_64::BRANCH,
            bits: &x86_64::BITS,
            flags: &x86_64::FLAGS,
            shapes: &x86_64::MACHINE,
            timing: &x86_64::TIMING,
            short: &x86_64::SHORT,
            selector: &select::x86_64::SELECTOR,
            env: Env::new().with(x86_64::GPR, &order, &SCRATCH).with(
                x86_64::XMM,
                &sse_order,
                &sse_scratch,
            ),
        }
    }

    /// The AArch64 machine under that convention.
    ///
    /// The scratch registers are `x16` and `x17`, which the convention already keeps out of the
    /// allocation order because a linker's veneer may write them between a call and the function
    /// it reaches. That is the property a scratch register wants: nothing lives in one across
    /// anything the compiler did not write, so a move the rewriter puts in can have it. The vector
    /// file's two are picked the way the x86 ones are, which lands on `v30` and `v31`.
    ///
    /// Nothing selects AArch64 instructions yet, so [`Machine::for_target`] does not return this.
    #[must_use]
    pub fn aarch64(conv: &'static CallRegs) -> Self {
        let order: Vec<PhysReg> =
            conv.int_order.iter().copied().filter(|reg| !AARCH64_SCRATCH.contains(reg)).collect();
        let (fp_order, fp_scratch) = held_back(conv);
        Self {
            conv,
            file: aarch64::REGS,
            insts: &aarch64::FRAME,
            branch: &aarch64::BRANCH,
            bits: &aarch64::BITS,
            flags: &aarch64::FLAGS,
            shapes: &aarch64::MACHINE,
            timing: &aarch64::TIMING,
            short: &aarch64::SHORT,
            selector: &select::aarch64::SELECTOR,
            env: Env::new().with(aarch64::GPR, &order, &AARCH64_SCRATCH).with(
                aarch64::FPR,
                &fp_order,
                &fp_scratch,
            ),
        }
    }

    /// The machine a target describes, or `None` when no backend in this crate covers it.
    ///
    /// [`TargetInfo`] already carries the convention, because the front end needs it to lay a
    /// `va_list` out, so the only thing this decides is which architecture's frame instructions
    /// and register file go with it. AArch64 and RISC-V are `None` until M6 fills them in, and a
    /// caller that gets one reports a target it cannot compile for rather than compiling wrongly.
    #[must_use]
    pub fn for_target(target: &TargetInfo) -> Option<Self> {
        let conv = target.call_regs?;
        match target.tuple.arch() {
            Arch::X86_64 => Some(Self::x86_64(conv)),
            _ => None,
        }
    }
}

/// Whether every function calls a profiler on the way in, and where that call goes.
///
/// What `-pg` asks for, with `-mfentry` and `-mno-fentry` choosing between the last two. The choice
/// has already been made against the target by the time this is built, which is why there is no
/// answer here for a command line that named neither.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Profile {
    /// It does not, which is what nearly every command line asks for.
    #[default]
    No,
    /// In front of the prologue, which is the hook a tracer can replace while the program runs.
    Early,
    /// Once the frame is taken, which is the hook that reads the frame pointer.
    Late,
}

/// How much room every function opens with for something to be written over it later.
///
/// What `-fpatchable-function-entry=` asks for, as the two halves a prologue deals in rather than
/// as the total and the part the flag is written in. The room can be on either side of the
/// function's own label and the two sides are not the same thing: what is after the label is inside
/// the function, which is what a patcher redirecting a call into it wants, and what is in front of
/// it is outside, which is where a patcher that needs a whole instruction it can reach from the
/// first one puts it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Room {
    /// How many bytes go after the function's own label.
    pub after: u32,
    /// How many go in front of it.
    pub before: u32,
}

impl Room {
    /// Whether any room at all was asked for, which is what decides whether a function gets one.
    ///
    /// `=0` is a command line that asked for none, and gcc takes it and writes nothing, so the
    /// question is about the numbers rather than about whether the flag was written.
    #[must_use]
    pub const fn any(self) -> bool {
        self.after > 0 || self.before > 0
    }
}

/// What the command line says, as opposed to what the machine says.
///
/// Most of it is about a frame, which is what this held to begin with, and the rest is passes being
/// asked for or turned off by name. [`Flags::goal`] is neither: it is the one thing here that no
/// flag names on its own and that every pass below selection may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    /// Whether every function keeps a frame pointer, which `-fno-omit-frame-pointer` asks for.
    pub frame_pointer: bool,
    /// Whether the red zone may be used, which `-mno-red-zone` and every kernel turns off.
    pub red_zone: bool,
    /// Whether a frame is taken a page at a time, which `-fstack-clash-protection` asks for.
    pub stack_clash: bool,
    /// Whether every address an indirect branch may arrive at opens with a landing pad, which
    /// `-fcf-protection=branch` asks for. That is every function, and every label of a function
    /// whose address the program took.
    pub landing: bool,
    /// Whether every function calls a profiler on the way in, which `-pg` asks for.
    pub profile: Profile,
    /// How much room every function opens with for a patcher, which
    /// `-fpatchable-function-entry=` asks for. See [`Room`].
    pub patch: Room,
    /// Whether the blocks are put in the order the weights say rather than in the order the
    /// shape of the graph says, which `-freorder-blocks` asks for and every level above `-O0`
    /// turns on. See [`crate::layout`].
    pub reorder: bool,
    /// Whether two locals that are never both wanted may be the same bytes, which
    /// `-fstack-reuse=none` turns off and `-O0` does not ask for. Spill slots share whatever this
    /// says, since a spill slot is not a variable and nothing can ask a debugger for one. See
    /// [`crate::slots`].
    pub reuse: bool,
    /// Whether the instructions of a block are put in the order the machine finishes soonest,
    /// which `-fschedule-insns2` asks for and every level from `-O2` turns on. See
    /// [`crate::schedule`].
    pub schedule: bool,
    /// Whether the head of every loop starts on a boundary of its own, which `-falign-loops` asks
    /// for and no level turns on by itself yet. See [`crate::layout::heads`].
    pub align_loops: bool,
    /// Whether the target's timing model is believed about the machine's units as well as about
    /// its latencies, which `-Zcycle-accurate-model=` says and the model itself answers otherwise.
    ///
    /// `None` is a command line that did not say, which is nearly every one, and then the model's
    /// own answer decides. It is here rather than only on the model because section 38.1 asks for
    /// a way to say the model is better or worse than it claims without editing the model, and
    /// because the measurement section 38.8 owes is the same corpus compiled both ways.
    pub accurate: Option<bool>,
    /// Whether the register allocator runs its own checks on a build that has assertions compiled
    /// out, which `-Zverify-each` asks for. See [`rucc_regalloc::run`].
    pub verify: bool,
    /// Whether the level asked for small code or for fast code.
    ///
    /// The level itself lives in `rucc-session`, which is above this crate, so what arrives here is
    /// the answer rather than the question. It is on the flags rather than on the [`Machine`]
    /// because it is not a fact about a machine: the same machine compiles the same function both
    /// ways, and which way is what the command line said.
    ///
    /// tamnd/rucc#741 is the issue about this not being here at all, and about `-Os` having been a
    /// shorter list of middle end passes and nothing else. [`crate::shorten`] is the first pass
    /// below selection to read it.
    pub goal: Goal,
}

impl Default for Flags {
    /// No frame pointer, the red zone allowed, the frame taken in one subtraction, no landing pad,
    /// no profiling, no room for a patcher, the blocks in the order the graph's shape gives,
    /// nothing in the frame sharing with anything, no scheduling, no loop padded to a boundary and
    /// code that is meant to be fast rather than small, which is what a convention that has a red
    /// zone says at `-O0` when nobody on the command line has said otherwise.
    fn default() -> Self {
        Self {
            frame_pointer: false,
            red_zone: true,
            stack_clash: false,
            landing: false,
            profile: Profile::No,
            patch: Room::default(),
            reorder: false,
            reuse: false,
            schedule: false,
            align_loops: false,
            accurate: None,
            verify: false,
            goal: Goal::Speed,
        }
    }
}

/// Compiles one function, from the IR the middle end produced to machine instructions.
///
/// The function is taken by reference that can be written through, because the first pass is an
/// IR to IR rewrite: a construct whose lowering is a new shape of control flow cannot be a rule,
/// since a rule replaces a term with a term and has nowhere to put a block. So the IR that reaches
/// selection is not quite the IR the middle end produced, and this is the only place that is true.
/// `--emit=ir` prints before any of this runs.
///
/// `elsewhere` is the one thing here that is a fact about the module rather than about the
/// function, and it is passed in rather than looked up because this only ever sees the one
/// function. What it decides is how the address of a name is come by, which is the difference
/// between an address this file can measure to and one only the linker knows.
///
/// # Errors
///
/// The first thing in it this cannot lower, which is what [`lower::func`] reports, and one thing
/// after it that is about the shape of the function rather than about an instruction, which is a
/// frame that grows while it runs in a function whose flags say no frame may. Everything else after
/// lowering works on machine instructions that exist, so it either runs or it is a bug in this
/// crate.
pub fn compile(
    source: &mut ir::Func,
    names: &mut Interner,
    machine: &Machine,
    elsewhere: &Elsewhere,
    flags: Flags,
) -> Result<mir::Func, Unsupported> {
    let (mut fired, mut pressure, mut lowerings) =
        (Fired::new(), Pressure::new(), Lowerings::new());
    compile_recording(
        source,
        names,
        machine,
        elsewhere,
        flags,
        &mut Recording { fired: &mut fired, pressure: &mut pressure, lowerings: &mut lowerings },
    )
}

/// Somewhere to put what a compilation did along the way, for the flags that ask.
///
/// One of these rather than three parameters, because they are one thing: a caller either wants
/// the measurements or does not, and a caller that does wants the same three to cover every
/// function of every file on the command line.
#[derive(Debug)]
pub struct Recording<'a> {
    /// Which lowering rules fired, for `-Zrule-coverage`.
    pub fired: &'a mut Fired,
    /// What the allocator had to put on the stack, for `-Zregister-pressure`.
    pub pressure: &'a mut Pressure,
    /// What the pre-selection lowering group did, for `-Zlowering`.
    pub lowerings: &'a mut Lowerings,
}

/// The same compilation, with what it did along the way recorded.
///
/// Two functions rather than one that takes options, because a caller that does not want the
/// numbers should not have to say so. What each field of the [`Recording`] is for is on the field,
/// and all of them are added to rather than replaced, so a caller passes the same one for every
/// function of a module and every module of a command line and gets the answer for all of them.
///
/// # Errors
///
/// The same as [`compile`]. A function that was refused contributes nothing to any of them, since
/// a function that did not compile is not evidence about what a rule set or a frame would have
/// done.
pub fn compile_recording(
    source: &mut ir::Func,
    names: &mut Interner,
    machine: &Machine,
    elsewhere: &Elsewhere,
    flags: Flags,
    recording: &mut Recording<'_>,
) -> Result<mir::Func, Unsupported> {
    // Everything the machine has no rule for, rewritten into things it has, as one group rather
    // than as a dozen lines here. What is in the group and what the order between its members is
    // for are both in `crate::lowering`, which is where a new lowering is added.
    let counting = recording.lowerings.wanted();
    let ran = lowering::group(source, names, machine.conv, counting);
    if counting {
        let called = names.resolve(source.name).to_owned();
        recording.lowerings.record(&called, ran);
    }
    // The function the program said it writes the whole of itself, which is what decides most of
    // the frame below rather than being one more thing in it. Read here rather than beside the rest
    // of the layout because the refusal a few lines down is the earliest thing that asks.
    let naked = source.attrs.set.contains(ir::AttrSet::NAKED);
    let lowered = lower::func(source, names, machine.selector, machine.conv, elsewhere)?;
    recording.fired.merge(&lowered.fired);
    let lower::Lowered { mut func, mut stack, blocks, .. } = lowered;
    // Straight after selection, because this is the last moment the machine blocks and the IR
    // blocks still stand one for one, and the pass that reads the numbers is the very last one
    // there is. See `crate::weights`.
    if flags.reorder {
        weights::carry(source, &blocks, &mut func);
    }
    // The one thing a frame that grows while it runs cannot be asked for, which is a refusal rather
    // than wrong code.
    if let Some(inst) = stack.grown_at {
        // And the one thing a naked function cannot be asked for either, from the other side of the
        // same fact. A frame that grows is reached from a frame pointer the prologue establishes,
        // and there is no prologue here, so the address the array hands out would be counted from a
        // register holding whatever the caller left in it.
        if naked {
            return Err(Unsupported::Dynamic { inst, growing: lower::Growing::Naked });
        }
        // The lowering refuses a variable length array that asks for more alignment than a call
        // leaves the stack pointer on. A fixed local asking for it in the same function is the same
        // refusal arrived at from the other side: the prologue would force the alignment, and
        // forcing it and moving the stack pointer afterwards are two frames that each want the one
        // register that still reaches the rest of the frame. See `Growing` in [`crate::frame`].
        if stack.locals.iter().any(|local| local.align > machine.conv.stack_align) {
            return Err(Unsupported::Dynamic { inst, growing: lower::Growing::Aligned });
        }
    }

    // Before the fold below, which is the order section 37.6 puts the two in. A widening this takes
    // out is one whose readers are sent to its source, and one of those readers may be an address
    // computation, so asking which bits are read first means the fold sees the addresses as they
    // will be rather than as they were.
    bits::dead(&mut func, machine.bits, machine.shapes, names);

    // After selection, because the address instruction and the one that reads it are both machine
    // instructions only once selection has written them, and before allocation, because what makes
    // the pair safe to put together is that a virtual register is written once. The addresses into
    // the frame and into the caller's argument area go through it like anything else, and the two
    // lists `finish` reads are rewritten as they do, so an address that ends up inside its reader
    // is still an address the frame layout knows to write an offset into.
    let mut pending = fold::Pending {
        addresses: &mut stack.addresses,
        arguments: &mut stack.arguments,
        dynamic: &mut stack.dynamic,
    };
    fold::addresses(&mut func, machine.insts, machine.shapes, names, &mut pending);

    // After that fold rather than before it, because what this puts inside an arithmetic
    // instruction is a load's addressing mode and a load whose address is still a `lea` in front of
    // it has nothing in its own mode worth carrying. Before allocation for the reason the fold is:
    // a virtual register is written once, which is the whole of why the value the load produced
    // cannot have changed between the two instructions this joins.
    // The run that reads a place, computes on it and writes it back goes first, because it is three
    // instructions the selector wrote and taking the load out of the middle one first would leave
    // the same run written a second way.
    combine::stores(&mut func, machine.shapes, machine.flags, names, &mut pending);
    combine::loads(&mut func, machine.shapes, names, &mut pending);

    // Whether this function carries a canary is the front end's answer, because what
    // `-fstack-protector` asks about is the kind of local a function has and the types are gone by
    // here. What the machine does about it is this crate's answer, and a target with nowhere to
    // keep the word a canary is copied from does nothing, which is what the driver refuses a
    // command line over before any of this runs.
    // Not in a naked function, whatever the command line asked of every function. The canary is a
    // word the prologue copies into the frame and the check at the end reads back, so a function
    // with neither has nowhere to put it and nowhere to read it from. gcc leaves one out too.
    let protect = source.attrs.set.contains(ir::AttrSet::STACK_PROTECT) && !naked;
    let guard = protect.then_some(machine.conv.guard.as_ref()).flatten();
    // Nothing at all on a target with no hook to call, which is the same answer the protector gives
    // on a target with nowhere to keep its word, and the driver refuses the command line over it
    // before any of this runs.
    let profile = match machine.conv.trace {
        Some(_) => flags.profile,
        None => Profile::No,
    };
    let base = stack.layout(Layout::new(machine.conv, machine.file));
    let layout = Layout {
        // The later hook reads the frame pointer to find out who called this function, so a
        // function that calls it is given one whether or not anything else asked. A function that
        // asked where its own frame is has the same claim on one, and for a plainer reason: the
        // register is the answer.
        //
        // And not at all in a naked function, whatever any of that says. Establishing one is two
        // instructions of a prologue there is none of, and a function that saves the machine state
        // by hand is usually saving the frame pointer among it, which is what micropython's
        // `nlr_push` does on its third line.
        frame_pointer: !naked
            && (flags.frame_pointer
                || profile == Profile::Late
                || stack.walks_frames
                || stack.saves_place),
        // And not in a naked function either, which is not about what the red zone costs but about
        // what the refusal below has to be able to see. A local small enough to live below the
        // stack pointer takes no bytes off it, so the frame comes out empty and a function that
        // wanted somewhere to keep something would be told it asked for nothing. Taking the red
        // zone away makes every local show up as bytes, and bytes are what gets refused.
        red_zone: flags.red_zone && !naked,
        protect: guard.is_some(),
        naked,
        // A protected function calls the one that does not come back, on the arm where the check
        // failed, so it is not a leaf however few calls the program wrote in it. That is what
        // takes the red zone away from it and what makes its frame leave the stack pointer where
        // a call needs it. The later hook is a call in the same position and costs the same.
        //
        // The earlier one is not, and this is the one place the difference shows. It runs before
        // the prologue has written anything, so the bytes below the stack pointer it uses are ones
        // this function has not put anything in yet, and a leaf that keeps its locals down there
        // stays a leaf. gcc leaves it alone too.
        leaf: base.leaf && guard.is_none() && profile != Profile::Late,
        ..base
    };

    // Before allocation as well, and asked here rather than where it is used because what it asks
    // is whether anything but the branch reads the byte a comparison wrote. A virtual register is
    // written once and a physical one is not, so after allocation that question no longer has an
    // answer.
    let fusable = layout::fusable(&func, machine.branch, names);
    // The same question about the selects on a comparison's byte, asked here for the same reason.
    let choosable = choice::fusable(&func, machine.branch, names);

    // In front of the splitting below, because what it does is take the values off the edges out of
    // a computed `goto` and the splitting has no answer for one of those: the block they leave ends
    // in a jump already, so neither end of the edge is somewhere a move can go.
    split::indirect(&mut func, machine.branch, machine.insts, names);

    // And after it, because what it puts a pad at is the block an address names and the pass above
    // is what settles which block that is. The pad the prologue opens with is written much later,
    // with the rest of the prologue, since the address it answers for is the function's own.
    //
    // Nothing at all on a target with nothing that marks an address as one an indirect branch may
    // arrive at, which is the same answer the stack protector gives on a target with nowhere to
    // keep its word, and the driver refuses the command line over it before any of this runs.
    let landing = flags.landing.then_some(machine.insts.landing).flatten();
    split::pads(&mut func, machine.insts, landing, names);

    // Before allocation, because an edge that carries values into a block arrived at more than
    // one way, out of a block that leaves more than one way, has nowhere to put the moves those
    // values turn into, and the allocator asserts rather than guessing.
    split::critical(&mut func);

    // Before allocation, because how far the address of a local gets is a question about values and
    // a value is written once only until the allocator's rewrite has been through. What is done
    // with the answer waits until afterwards, since the liveness it is read against is the
    // allocator's. See [`crate::slots`].
    //
    // Only asked at all where the locals are allowed to share, since this is the whole of what says
    // whether a local may. The spill slots are laid out either way and this says nothing about
    // them.
    let reach = flags
        .reuse
        .then(|| slots::reach(&func, &stack.addresses, stack.locals.len(), machine.insts, names));

    // The instructions as they are now, for the locals the front end kept in values. The
    // allocator's liveness is counted along this order and the rewrite is about to put spills,
    // reloads and edge moves in among them, so the list has to be taken before it runs. Only in a
    // function that named something, since a function that named nothing has no use for it. See
    // [`crate::kept`].
    //
    // Or where a local the program declared may share its bytes, which is only where there is a
    // `reach`, since a local that shares is in the frame over part of the function and the part is
    // asked about the same way.
    let line = (!func.named.is_empty() || (reach.is_some() && !stack.declared.is_empty()))
        .then(|| kept::before(&func));

    let called = names.resolve(func.name).to_owned();
    let allocation = rucc_regalloc::run(&mut func, &machine.env, &called, flags.verify);
    recording.pressure.record(&called, Cost::of(&allocation));

    // After allocation, because the largest area in most frames is the spill slots and nothing
    // knows how many of those there are until the allocator has finished running out of registers,
    // and because a spill slot cannot be shared with a local until it is known there is one.
    let widths = frame::widths(&layout, &allocation);
    let share = Slots::share(&func, reach.as_ref(), &allocation, &stack.locals, &widths);
    let layout = Layout { share: Some(&share), ..layout };
    let frame = Frame::of(&func, &allocation, &layout);
    // The one thing a naked function cannot be given. Everything else the attribute asks for is
    // something left out, and leaving something out always works; bytes are the one thing the body
    // may want that only a prologue provides. A local, a spilled value and the arguments of a call
    // are the three ways to want them, and the answer to all three is the same sentence.
    if naked && frame.size() > 0 {
        return Err(Unsupported::Naked { bytes: frame.size() });
    }

    // Here because this is where the two halves of the answer are both in hand: which local is
    // which declaration came down from selection, and where a local is was settled a line ago.
    // Nothing further on could work it out, since the frame is not carried past this function and
    // an offset in a finished instruction says nothing about what the bytes it reaches are for.
    //
    // Whatever the command line said about debugging information, because the list is one entry
    // per local the program named and a function has tens of those at most. Asking the flags would
    // cost more to thread down here than the list costs to build.
    //
    // A local that went in beside something else is left off, because its bytes are its own only
    // where it is wanted and an answer good at every address would have a debugger print whatever
    // took its place. It gets stretches instead, at the end with the locals kept in values.
    func.locals = stack
        .declared
        .iter()
        .filter(|&&(local, _)| share.shared(local).is_none())
        .filter_map(|&(local, decl)| Some((decl, frame.from_frame_base(local)?)))
        .collect();
    let framed: Vec<(u32, i32, &[rucc_regalloc::live::Range])> = stack
        .declared
        .iter()
        .filter_map(|&(local, decl)| {
            Some((decl, frame.from_frame_base(local)?, share.shared(local)?))
        })
        .collect();
    func.sharing = framed.iter().map(|&(decl, _, _)| decl).collect();

    let scratch = machine.env.scratch(machine.conv.int_class);
    let protect = guard.map(|guard| Protect {
        guard,
        branch: machine.branch,
        scratch: [scratch[0], scratch[1]],
    });
    // A target with no instruction that touches a page without changing it does nothing about the
    // flag, which is the same answer the protector gives on a target with nowhere to keep its word.
    // Every target this crate has a back end for has one.
    //
    // Or where the platform reaches the pages of every frame whatever the command line said, which
    // is Windows. The prologue there calls a routine rather than walking, but a frame that grows
    // while it runs is walked in the body either way: the routine takes its size in a register the
    // allocator hands out and destroys two more, which is answerable in a prologue and not in the
    // middle of a function, and the walk needs nothing but the two registers already held back.
    let probe = (flags.stack_clash || machine.conv.chkstk.is_some())
        .then_some(machine.insts.probe.as_ref())
        .flatten()
        .map(|probe| Probing { probe, branch: machine.branch, scratch: [scratch[0], scratch[1]] });
    let trace = machine.conv.trace.and_then(|trace| match profile {
        Profile::No => None,
        Profile::Early => Some(Tracing { name: trace.early, early: true }),
        Profile::Late => Some(Tracing { name: trace.late, early: false }),
    });
    // And once more for the room a patcher was promised, which is a run of the shortest
    // instruction that does nothing and so needs the target to have one. Nothing is written on a
    // target that does not, rather than a run of something longer: the flag counts bytes, and a
    // patcher writing over the room starts at its front and wants every byte in it to be a place
    // it could have started at.
    let pad = flags.patch.any().then_some(machine.insts.pad).flatten().map(|name| Padding {
        name,
        before: flags.patch.before,
        after: flags.patch.after,
    });
    let convention = Convention {
        protect,
        probe,
        landing,
        trace,
        pad,
        ..Convention::new(machine.conv, machine.insts)
    };
    let moves = finish(&mut func, &allocation, &frame, &stack, convention, names);

    // After the moves are written, because a spill and the reload of it are written by different
    // decisions of the allocator and what stands between the two is settled by the function they
    // both went into. Before the layout, because the layout is where the instruction sequence
    // stops being something a pass may edit.
    copies::clean(&mut func, &moves, machine.shapes, machine.insts, machine.conv, names);

    // After the allocator's moves have been cleaned up, because a schedule chosen around a move
    // that is about to be taken out is a schedule built around an instruction that is not in the
    // output. Before the layout, because the layout is the freeze: it writes the jumps the block
    // order needs and it puts a comparison and the branch that reads it together, and neither
    // survives an instruction being moved in afterwards. That is section 38.6's placement, and the
    // reason it is after allocation rather than before is in [`crate::schedule`].
    if flags.schedule {
        schedule::insts(
            &mut func,
            machine.timing,
            machine.shapes,
            machine.flags,
            names,
            flags.accurate.unwrap_or(machine.timing.accurate),
            &fusable,
        );
    }

    // Last, because everything before this finds the blocks a function returns from by looking
    // for the ones that go nowhere, and after this a block that falls through goes nowhere too.
    layout::blocks(&mut func, machine.branch, names, &fusable, flags.reorder);

    // After the layout for the reason the branches wait for it: a select that reads what a
    // comparison left is a pair with nothing allowed between, and nothing past here puts anything
    // there. Before the compare pass, since the comparison this keeps is one that pass may find
    // was already made.
    choice::moves(&mut func, machine.branch, machine.flags, machine.shapes, names, &choosable);

    // After the layout rather than before it, which is the whole of what makes it safe. What a
    // comparison leaves for the instruction behind it to read is not a register and nothing may
    // come between the two, and the layout is the other pass that writes such a pair. Running
    // here means there is nothing left that could put an instruction in the middle of one.
    compare::redundant(&mut func, machine.flags, machine.shapes, names);

    // After that rather than before it, because a comparison it takes out is a write of the
    // condition state that is gone with it, and this pass is asking which writes of that state are
    // read. Running in front would see writes the output does not have and turn down rewrites that
    // are allowed. Nothing here moves an instruction or changes a block, so being behind the
    // layout's freeze costs it nothing.
    shorten::shorter(&mut func, machine.short, machine.flags, machine.shapes, names, flags.goal);

    // Once the blocks will not move again, since a head is a block a jump runs backwards to and
    // which way a jump runs is the layout's answer. Nothing below adds or takes out a block.
    if flags.align_loops {
        func.heads = layout::heads(&func);
    }

    // Last of all, because a stretch is named by the instructions at either end of it and every
    // pass above is free to take an instruction out or move one. The frame is wanted here as well
    // as above, since a value the allocator spilled is in the frame over its stretch rather than in
    // a register, and it is the same distance from the call frame address the locals were given.
    func.kept = match line {
        Some(line) => kept::of(&func, &line, &allocation, &frame, &framed),
        None => Vec::new(),
    };
    Ok(func)
}

#[cfg(test)]
mod tests {
    use rucc_ir::{Builder, Flags as IrFlags, Func, Opcode, Restrict, Signature, Type};
    use rucc_target::x86_64::{REGS, SYSV, WIN64};

    use super::*;

    /// A function of two integers, and the block to fill.
    fn blank(params: &[Type]) -> (Interner, Func, ir::Block, Vec<ir::Value>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let block = func.create_block();
        let values = params.iter().map(|&ty| func.append_param(block, ty)).collect();
        (names, func, block, values)
    }

    /// The AArch64 machine holds back the two registers a veneer may write and two vector
    /// registers nothing is passed in, and hands out everything else the convention orders.
    #[test]
    fn the_aarch64_machine_holds_back_what_a_veneer_writes() {
        use rucc_target::aarch64::{self, AAPCS64, FPR, GPR, v};
        let machine = Machine::aarch64(&AAPCS64);
        assert_eq!(machine.env.scratch(GPR), [aarch64::X16, aarch64::X17]);
        assert_eq!(machine.env.scratch(FPR), [v(30), v(31)]);
        assert_eq!(machine.env.order(GPR), AAPCS64.int_order);
        assert_eq!(machine.env.order(FPR).len(), 30);
        assert_eq!(machine.insts.prefix, "a64.");
        assert_eq!(machine.timing.prefix, machine.shapes.prefix);
    }

    #[test]
    fn an_addition_compiles_for_aarch64_end_to_end() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::Add, args[0], args[1], IrFlags::default());
        build.ret(&[sum]);

        let machine = Machine::aarch64(&rucc_target::aarch64::AAPCS64);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");
        let text = mir::print_func(&out, &names, &rucc_target::aarch64::REGS);
        panic!("{text}");
    }

    #[test]
    fn a_function_comes_out_with_no_virtual_register_left_in_it() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::Add, args[0], args[1], IrFlags::default());
        build.ret(&[sum]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        // `int f(int a, int b) { return a + b; }` end to end. A leaf that spills nothing needs no
        // frame at all, so there is no prologue to see. The one move left is the one the machine's
        // addition needs, since the sum is written into the register the left operand was read
        // from and the return wants it in `rax`.
        assert_eq!(
            mir::print_func(&out, &names, &REGS),
            "mfunc @f {\n\
             block0:\n    \
             $rdi($rdi) = x64.arg_val_32\n    \
             $rsi($rsi) = x64.arg_val_32\n    \
             $rdi(reuse 1) = x64.add_rr_32 $rdi, $rsi\n    \
             $rax = x64.mov_rr_64 $rdi\n    \
             x64.ret_val_32 $rax($rax)\n    \
             x64.ret\n\
             }\n"
        );
    }

    #[test]
    fn a_declared_local_comes_out_saying_how_far_below_the_call_frame_address_it_is() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32]);
        let mut build = Builder::new(&mut source, block);
        let info = rucc_ir::MemInfo {
            size: 4,
            align: 4,
            order: rucc_ir::MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mem = build.func().add_mem(info);
        let slot = build.value(
            ir::InstData { extra: ir::Extra::Mem(mem), ..ir::InstData::new(Opcode::Alloca) },
            Type::PTR,
        );
        build.func().declare_mem(mem, 5);
        build.store(args[0], slot, info, IrFlags::default());
        let loaded = build.load(i32, slot, info, IrFlags::default());
        build.ret(&[loaded]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        // `int f(int a) { int x = a; return x; }` with the address of `x` taken, so it is four
        // bytes in the frame. A leaf this small lives in the red zone, so the stack pointer never
        // moves. What is below it is a whole word, since everything a frame holds is counted in
        // words whether or not it fills one, and the call frame address is one more word above the
        // stack pointer for the return address the call pushed.
        assert_eq!(out.locals, vec![(5, -16)]);
    }

    #[test]
    fn a_local_kept_in_a_value_comes_out_saying_which_register_holds_it_and_over_what() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32]);
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::Add, args[0], args[0], IrFlags::default());
        build.func().declare_value(sum, 5);
        build.ret(&[sum]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        // `int f(int a) { int x = a + a; return x; }` with nothing taking the address of `x`, so
        // it never reaches the frame and the only answer about it is a register. The sum is
        // written by the addition and read by the move that puts it where the return wants it, so
        // the stretch is one instruction long and it is the move rather than the addition.
        assert_eq!(out.kept.len(), 1, "one stretch: {:?}", out.kept);
        assert_eq!(out.kept[0].decl, 5);
        assert!(matches!(out.kept[0].at, mir::Where::Reg { .. }), "a register: {:?}", out.kept[0]);
        assert!(out.locals.is_empty(), "nothing in the frame: {:?}", out.locals);
    }

    /// What `-Zlowering` is built out of, and the reason it is worth a test here rather than only
    /// in `crate::lowering`: the group has to be the thing this pipeline runs. A lowering added to
    /// a line of this function instead of to `Step::GROUP` would still work and would still be
    /// untested, and the record coming back with one entry per member is what catches it.
    #[test]
    fn every_member_of_the_lowering_group_is_run_by_the_compilation_and_says_what_it_did() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32]);
        let mut build = Builder::new(&mut source, block);
        let swapped = build.unary(Opcode::Bswap, args[0], i32);
        build.ret(&[swapped]);

        let mut lowerings = Lowerings::asked(true);
        compile_recording(
            &mut source,
            &mut names,
            &Machine::x86_64(&SYSV),
            &Elsewhere::default(),
            Flags::default(),
            &mut Recording {
                fired: &mut Fired::new(),
                pressure: &mut Pressure::new(),
                lowerings: &mut lowerings,
            },
        )
        .expect("every instruction has a rule");

        assert_eq!(lowerings.functions(), 1);
        let listing = lowerings.listing();
        assert!(listing.contains("lowering f\n"), "{listing}");
        for step in lowering::Step::GROUP {
            assert!(listing.contains(step.name()), "{} did not run: {listing}", step.name());
        }
        // The byte reversal went through the group rather than reaching the selector, which has no
        // rule for one.
        assert!(listing.contains("bytes"), "{listing}");
        assert!(!listing.contains("left 1"), "something the group answers for survived: {listing}");
    }

    /// What `-Zrule-coverage` is built out of: the rules a compilation fired, recorded as it went.
    /// The second function adds to the first rather than replacing it, which is what makes one of
    /// these files the answer for a whole command line rather than for whichever function was last.
    #[test]
    fn which_rules_lowered_a_function_is_something_the_compilation_can_be_asked_for() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::Add, args[0], args[1], IrFlags::default());
        build.ret(&[sum]);

        let machine = Machine::x86_64(&SYSV);
        let mut fired = Fired::new();
        compile_recording(
            &mut source,
            &mut names,
            &machine,
            &Elsewhere::default(),
            Flags::default(),
            &mut Recording {
                fired: &mut fired,
                pressure: &mut Pressure::new(),
                lowerings: &mut Lowerings::asked(true),
            },
        )
        .expect("every instruction has a rule");
        let one = fired.count();
        assert!(one > 0, "an add and a return went through the table and nothing was recorded");

        let listing = fired.listing(&select::x86_64::TABLE);
        assert_eq!(listing.lines().filter(|line| line.starts_with("fired ")).count(), one);
        assert!(
            listing.contains(&format!("{one} of ")),
            "{}",
            listing.lines().next().unwrap_or("")
        );

        // The same rules again plus the ones a subtraction needs, into the same record.
        let (mut names, mut source, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut source, block);
        let difference = build.binary(Opcode::Sub, args[0], args[1], IrFlags::default());
        build.ret(&[difference]);
        compile_recording(
            &mut source,
            &mut names,
            &machine,
            &Elsewhere::default(),
            Flags::default(),
            &mut Recording {
                fired: &mut fired,
                pressure: &mut Pressure::new(),
                lowerings: &mut Lowerings::asked(true),
            },
        )
        .expect("every instruction has a rule");
        assert!(fired.count() > one, "a subtraction is not an addition");
    }

    #[test]
    fn a_function_that_calls_takes_a_frame_and_gives_it_back() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32]);
        let sig = source.add_signature(Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let callee = names.intern("g");
        let call = Builder::new(&mut source, block).call(callee, sig, &[args[0]]);
        let got = source[call].first_result.expect("an integer comes back");
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::Add, got, args[0], IrFlags::default());
        build.ret(&[sum]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        // `int f(int a) { return g(a) + a; }`. Not a leaf, so the stack pointer moves and the
        // register the value that outlives the call went to is one the prologue saves.
        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.push_64 $rbx"), "{text}");
        assert!(text.contains("$rbx = x64.pop_64"), "{text}");
        assert!(text.contains("x64.call $rdi($rdi), @g"), "{text}");
        assert!(!text.contains('%'), "{text}");
    }

    #[test]
    fn the_other_convention_is_the_same_function_somewhere_else() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::Add, args[0], args[1], IrFlags::default());
        build.ret(&[sum]);

        let machine = Machine::x86_64(&WIN64);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        // The arguments arrive in `rcx` and `rdx` here rather than in `rdi` and `rsi`, which is
        // the whole of what changed, and it changed because the convention was asked.
        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("$rcx($rcx) = x64.arg_val_32"), "{text}");
        assert!(text.contains("$rdx($rdx) = x64.arg_val_32"), "{text}");
        assert!(!text.contains("$rdi"), "{text}");
    }

    #[test]
    fn a_function_with_a_branch_in_it_goes_through_every_pass() {
        let i32 = Type::int(32);
        let (mut names, mut source, entry, args) = blank(&[i32, i32]);
        let then = source.create_block();
        let join = source.create_block();
        let got = source.append_param(join, i32);
        let mut build = Builder::new(&mut source, entry);
        let cond = build.icmp(rucc_ir::IntPred::Slt, args[0], args[1]);
        build.br_if(cond, then, &[], join, &[args[1]]);
        Builder::new(&mut source, then).jump(join, &[args[0]]);
        Builder::new(&mut source, join).ret(&[got]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        // The else arm is a critical edge carrying a value, so a block that nothing lowered is in
        // there, which is the pass between lowering and allocation doing its job. Without it the
        // allocator would have asserted rather than compiled this.
        assert_eq!(out.block_count(), 4);

        // `int f(int a, int b) { return a < b ? a : b; }` end to end, and the last pass is what
        // this pins. The branch became a test and one jump, and it is the jump taken when the
        // condition failed, because the arm the condition is true for is the block laid out next
        // and a block falls into the block laid out next. The other arm is the empty block the
        // edge splitting left, which is where the move the edge carries ended up, and it falls
        // into the join as well. What is left is one jump in the whole function. Both arms write
        // the join's parameter straight into `rax`, because the return at the bottom insists on
        // that register and the moves the edges carry are free to name it.
        let text = mir::print_func(&out, &names, &REGS);
        assert_eq!(
            text,
            "mfunc @f {\n\
             block0:\n    \
             $rdi($rdi) = x64.arg_val_32\n    \
             $rsi($rsi) = x64.arg_val_32\n    \
             x64.cmp_rr_32 $rdi, $rsi\n    \
             x64.jcc_ge block2, block1\n\
             \nblock1:\n    \
             $rax = x64.mov_rr_64 $rdi\n    \
             x64.jmp block3\n\
             \nblock2:\n    \
             $rax = x64.mov_rr_64 $rsi, block3\n\
             \nblock3:\n    \
             x64.ret_val_32 $rax($rax)\n    \
             x64.ret\n\
             }\n"
        );
    }

    /// A loop that swaps its two values round every time it goes, which is `gcd`, and which is
    /// the smallest program that caught two ways of losing a value. Both were found by running
    /// what came out rather than by reading it, and both are pinned here rather than only where
    /// they were fixed, because what is wrong with either of them is only visible in the whole
    /// function.
    #[test]
    fn a_loop_that_carries_its_values_round_keeps_all_of_them() {
        let i32 = Type::int(32);
        let (mut names, mut source, entry, args) = blank(&[i32, i32]);
        let head = source.create_block();
        let body = source.create_block();
        let exit = source.create_block();
        let left = source.append_param(head, i32);
        let right = source.append_param(head, i32);
        Builder::new(&mut source, entry).jump(head, &[args[0], args[1]]);
        let mut build = Builder::new(&mut source, head);
        let zero = build.iconst(i32, 0);
        let more = build.icmp(rucc_ir::IntPred::Ne, right, zero);
        build.br_if(more, body, &[], exit, &[left]);
        let mut build = Builder::new(&mut source, body);
        let rest = build.binary(Opcode::SRem, left, right, IrFlags::default());
        build.jump(head, &[right, rest]);
        let result = source.append_param(exit, i32);
        Builder::new(&mut source, exit).ret(&[result]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        // `int gcd(int a, int b) { while (b) { int t = a % b; a = b; b = t; } return a; }`. Two
        // things in here were wrong and each of them returned three from a program that gcc
        // returns forty two from.
        //
        // The first is in the entry block. The move the edge into the loop asks for writes `rsi`,
        // and the second argument has to be taken out of `rsi` before it does. An edit at the end
        // of a block used to go in front of the last instruction, on the reasoning that the last
        // instruction is the branch, and the block's jump is not an instruction until the layout
        // has run, so it went in front of the `arg_val` whose own move had not been made yet.
        //
        // The second is in the loop body. A division writes both a quotient and a remainder, and
        // only the remainder is wanted here, so the quotient is a value nothing reads. It used to
        // be given the same register as the remainder, because a value written early was live at
        // one point and that point was in front of where the remainder was written. The copy that
        // takes the quotient nowhere then landed on top of the remainder. The remainder is written
        // early as well now, which is a separate thing the target has to say and is why both
        // answers read `early` here: `rdx` is filled by the sign extension before the division
        // reads its divisor, so nothing else may be sitting in it at that point either.
        //
        // What asks whether the second argument is zero reads as a test rather than a comparison
        // because `crate::shorten` runs last and writes the shorter of the two, which asks the
        // machine the same thing and leaves the same condition state for the jump behind it.
        assert_eq!(
            mir::print_func(&out, &names, &REGS),
            "mfunc @f {\n\
             block0:\n    \
             $rdi($rdi) = x64.arg_val_32\n    \
             $rsi($rsi) = x64.arg_val_32\n    \
             $rcx = x64.mov_rr_64 $rdi, block1\n\
             \nblock1:\n    \
             x64.test_rr_32 $rsi\n    \
             x64.jcc_e block3, block2\n\
             \nblock2:\n    \
             $rax = x64.mov_rr_64 $rcx\n    \
             early $rdx($rdx), early $rax($rax) = x64.idiv_rem_32 $rax($rax), $rsi\n    \
             $rdi = x64.mov_rr_64 $rax\n    \
             $rcx = x64.mov_rr_64 $rsi\n    \
             $rsi = x64.mov_rr_64 $rdx\n    \
             x64.jmp block1\n\
             \nblock3:\n    \
             $rax = x64.mov_rr_64 $rcx\n    \
             x64.ret_val_32 $rax($rax)\n    \
             x64.ret\n\
             }\n"
        );
    }

    /// `spec/10-backend.md` section 10.1 says `--emit=mir-final` round-trips, and a function with
    /// a branch in it is the one where that is worth checking: after the layout has run, where a
    /// jump goes is nowhere in the instruction, so the text has to carry it on the block and the
    /// parser has to put it back on the block it came off.
    #[test]
    fn a_function_that_has_been_laid_out_reads_back_as_the_same_function() {
        let i32 = Type::int(32);
        let (mut names, mut source, entry, args) = blank(&[i32, i32]);
        let then = source.create_block();
        let join = source.create_block();
        let got = source.append_param(join, i32);
        let mut build = Builder::new(&mut source, entry);
        let cond = build.icmp(rucc_ir::IntPred::Slt, args[0], args[1]);
        build.br_if(cond, then, &[], join, &[args[1]]);
        Builder::new(&mut source, then).jump(join, &[args[0]]);
        Builder::new(&mut source, join).ret(&[got]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        let read = rucc_mir::parse(&text, &mut names, &REGS).expect("what the printer wrote");
        assert_eq!(mir::print(&read, &names, &REGS), text);
    }

    #[test]
    fn a_function_this_cannot_lower_is_reported_rather_than_compiled() {
        let f80 = Type::float(rucc_ir::Float::F80);
        let (mut names, mut source, block, args) = blank(&[f80, Type::int(64)]);
        Builder::new(&mut source, block).ret(&args);

        // One of these comes back on the x87 stack and a pair comes back in a pair of registers,
        // and there is no pair with that stack in it. So this is refused rather than lowered, and
        // it is the convention that refuses it rather than anything about the instructions.
        let machine = Machine::x86_64(&SYSV);
        let failed =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect_err("a long double cannot come back beside another value");
        assert_eq!(failed.to_string(), "what this function gives back is on the x87 stack");
    }

    /// A `long double` in and a `long double` out, which is the whole of what the convention says
    /// about the type and is two different answers rather than one.
    ///
    /// It arrives in the caller's argument area, so what the parameter is is the address of the
    /// bytes and the function reads them where they are. It goes back on the x87 stack, so the
    /// return is an `fld` and nothing else, and the value is still on that stack when the function
    /// returns, which is the one time anything here leaves it that way.
    ///
    /// The addresses are gone from the instruction listing, which is [`crate::fold`]: an argument's
    /// address is a `lea` off the stack pointer and the `fld` that reads it has room for that
    /// address itself, so the offset the frame layout works out is written into the `fld`.
    #[test]
    fn a_long_double_arrives_in_memory_and_goes_back_on_the_x87_stack() {
        let f80 = Type::float(rucc_ir::Float::F80);
        let (mut names, mut source, block, args) = blank(&[f80, f80]);
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::FAdd, args[0], args[1], IrFlags::default());
        build.ret(&[sum]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        // The two parameters, sixteen bytes apart, read out of the caller's frame rather than out
        // of a register, and the answer left on the stack by the last instruction in the function.
        assert!(text.contains("x64.fld_t [$rsp + 32]"), "{text}");
        assert!(text.contains("x64.fld_t [$rsp + 48]"), "{text}");
        assert!(!text.contains("x64.lea_64"), "an address every reader took is gone: {text}");
        assert!(!text.contains("x64.ret_val"), "nothing comes back in a register: {text}");
        // What comes after the `fld` is the epilogue, which gives the frame back and touches
        // nothing in the unit, so the value is where the caller looks for it when the `ret` runs.
        let end: Vec<&str> = text.lines().rev().skip(1).take(3).map(str::trim).collect();
        assert_eq!(end, ["x64.ret", "$rsp = x64.add_ri_64 $rsp, 24", "x64.fld_t [$rsp]"], "{text}");
    }

    /// The whole of the second register class, end to end: two floats arrive in vector registers,
    /// the arithmetic happens in one, and the answer goes back in the register the convention
    /// names. Nothing here touches the general purpose file, which is the point.
    #[test]
    fn a_float_is_added_in_the_register_file_it_arrives_in() {
        let f32 = Type::float(rucc_ir::Float::F32);
        let (mut names, mut source, block, args) = blank(&[f32, f32]);
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::FAdd, args[0], args[1], ir::Flags::default());
        build.ret(&[sum]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.addss_rr"), "{text}");
        assert!(text.contains("$xmm0"), "{text}");
        assert!(!text.contains("$rax"), "{text}");
    }

    /// A float moved between a register and memory, which is the instruction that decides which
    /// file the value is in and is a different one from the `mov` that moves the same four bytes.
    #[test]
    fn a_float_read_from_memory_and_written_back_uses_the_scalar_moves() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[Type::PTR, f64]);
        let mut build = Builder::new(&mut source, block);
        let info = rucc_ir::MemInfo {
            size: 8,
            align: 8,
            order: rucc_ir::MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let read = build.load(f64, args[0], info, ir::Flags::default());
        let sum = build.binary(Opcode::FAdd, read, args[1], ir::Flags::default());
        build.store(sum, args[0], info, ir::Flags::default());
        build.ret(&[sum]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.movsd_rm"), "{text}");
        assert!(text.contains("x64.movsd_mr"), "{text}");
        // Not the aligned whole register move, which is what a spill uses and is the one
        // instruction here that would read and write more than the program asked for.
        assert!(!text.contains("x64.movaps_rm"), "{text}");
        assert!(!text.contains("x64.movaps_mr"), "{text}");
    }

    /// The same journey at the format the machine only moves, which is the whole of what it can do
    /// with one: in from memory, back out to memory, in and out of a register, and back to the
    /// caller.
    ///
    /// No arithmetic, because there is no instruction for any and every one of them is a call to
    /// the runtime. What this says is that the value gets where a call would need it to be.
    #[test]
    fn a_quad_float_read_from_memory_and_written_back_uses_the_whole_register_move() {
        let quad = Type::float(rucc_ir::Float::F128);
        let (mut names, mut source, block, args) = blank(&[Type::PTR, quad]);
        let mut build = Builder::new(&mut source, block);
        let info = rucc_ir::MemInfo {
            size: 16,
            align: 16,
            order: rucc_ir::MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let read = build.load(quad, args[0], info, ir::Flags::default());
        build.store(args[1], args[0], info, ir::Flags::default());
        build.ret(&[read]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.movaps_rm"), "{text}");
        assert!(text.contains("x64.movaps_mr"), "{text}");
        assert!(text.contains("x64.arg_val_f128"), "{text}");
        assert!(text.contains("x64.ret_val_f128"), "{text}");
        // In the vector file and not the general purpose one, which is where the two eightbytes
        // of this value would have gone if it had been classified as a pair of integers.
        assert!(text.contains("$xmm0"), "{text}");
        assert!(!text.contains("gpr($rax)"), "{text}");
    }

    /// Both conversions between an unsigned word and a `long double`, all the way to instructions.
    ///
    /// What the rewrite writes and what the x87 group in [`crate::lower`] has are two lists put
    /// together in two different files, and this is where they meet. The rewrite is free to write
    /// any instruction it likes at any width, and at this width almost none of them can be
    /// lowered, so a correction written the way the narrower ones are written would pass its own
    /// tests next door and fail here.
    #[test]
    fn an_unsigned_word_and_a_long_double_convert_into_each_other() {
        let f80 = Type::float(rucc_ir::Float::F80);
        let (mut names, mut source, block, args) = blank(&[Type::PTR, Type::int(64)]);
        let mut build = Builder::new(&mut source, block);
        let info = rucc_ir::MemInfo {
            size: 16,
            align: 16,
            order: rucc_ir::MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let wide = build.unary(Opcode::UIToFP, args[1], f80);
        build.store(wide, args[0], info, ir::Flags::default());
        let read = build.load(f80, args[0], info, ir::Flags::default());
        let back = build.unary(Opcode::FPToUI, read, Type::int(64));
        build.ret(&[back]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        // The signed conversions in both directions, the constants that correct them, and the
        // multiply that takes a correction or leaves it. Nothing here reaches a wide register.
        assert!(text.contains("x64.fild_ll"), "the integer goes in as a signed one: {text}");
        assert!(text.contains("x64.fistp_ll"), "and comes back out as one: {text}");
        assert!(text.contains("x64.fmul_p"), "the correction is taken or not: {text}");
        assert!(text.contains("x64.fadd_p"), "and applied one way: {text}");
        assert!(text.contains("x64.fsubr_p"), "and the other: {text}");
        assert!(!text.contains("xmm"), "no part of this is in a vector register: {text}");
    }

    /// A value carried from one register file to the other, which is what a conversion is. The
    /// instruction reads one file and writes the other, and the allocator has to know that: a
    /// conversion whose operands were both said to be in one file would put the answer in a
    /// register the next instruction cannot reach.
    #[test]
    fn a_conversion_carries_the_value_into_the_other_register_file() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64]);
        let mut build = Builder::new(&mut source, block);
        let whole = build.unary(Opcode::FPToSI, args[0], Type::int(32));
        let back = build.unary(Opcode::SIToFP, whole, f64);
        build.ret(&[back]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        // The conversion that cuts towards zero rather than the one that rounds, which is what C
        // means by the cast, and the argument and the answer in the register the convention names.
        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.cvttsd2si_32"), "{text}");
        assert!(text.contains("x64.cvtsi2sd_32"), "{text}");
        assert!(text.contains("$xmm0"), "{text}");
    }

    /// The other way of putting a float and a number together, which keeps every bit rather than
    /// the value and is what a program reading the bits of a `double` asks for.
    #[test]
    fn a_bitcast_between_the_files_is_the_move_that_changes_no_bit() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64]);
        let mut build = Builder::new(&mut source, block);
        let bits = build.unary(Opcode::Bitcast, args[0], Type::int(64));
        build.ret(&[bits]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.movq_from_xmm"), "{text}");
        assert!(!text.contains("cvt"), "{text}");
    }

    /// A comparison whose answer the machine has a condition for, which is most of them.
    #[test]
    fn a_float_comparison_is_the_compare_and_the_byte_a_condition_sets() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64, f64]);
        let mut build = Builder::new(&mut source, block);
        let less = build.fcmp(rucc_ir::FloatPred::Olt, args[0], args[1], ir::Flags::default());
        let wide = build.unary(Opcode::ZExt, less, Type::int(32));
        build.ret(&[wide]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        // Less than is greater than with the operands the other way round, and the machine has no
        // condition for the first, so the rule that fires is the one that swaps them.
        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.ucomisd_set_a"), "{text}");
    }

    /// The two comparisons that are not one condition. An ordered equality is the flag that means
    /// equal or unordered and the flag that says it was ordered, so the instruction writes a
    /// second byte and reads it back, and what this is about is that the second byte gets a
    /// register of its own rather than the one the answer is in.
    #[test]
    fn an_equality_between_floats_gets_a_register_for_the_byte_it_needs_twice() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64, f64]);
        let mut build = Builder::new(&mut source, block);
        let same = build.fcmp(rucc_ir::FloatPred::Oeq, args[0], args[1], ir::Flags::default());
        let wide = build.unary(Opcode::ZExt, same, Type::int(32));
        build.ret(&[wide]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        let line = text
            .lines()
            .find(|line| line.contains("x64.ucomisd_set_e_and_np"))
            .expect("the rule for an ordered equality fired");
        let written: Vec<&str> = line
            .split_once('=')
            .expect("the instruction writes something")
            .0
            .split(',')
            .map(str::trim)
            .collect();
        assert_eq!(written.len(), 2, "{line}");
        assert_ne!(written[0], written[1], "{line}");
    }

    /// A float literal, which is the last float thing a C program writes that had no lowering.
    /// The rewrite that puts it in reach is in `expand`, and what this is about is that the two
    /// halves meet: the constant is spelled in a general purpose register and moved across.
    #[test]
    fn a_float_constant_is_the_bits_in_a_register_and_the_move_that_carries_them_over() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, _) = blank(&[]);
        let mut build = Builder::new(&mut source, block);
        let half = build.fconst(f64, 0x3fe0_0000_0000_0000);
        build.ret(&[half]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.mov_ri_64"), "{text}");
        assert!(text.contains("x64.movq_to_xmm"), "{text}");
    }

    /// A negation, which is the sign bit flipped and nothing else touched, so what the machine
    /// does is an exclusive or in a general purpose register rather than any float instruction.
    #[test]
    fn a_negation_is_the_sign_bit_flipped_and_no_float_instruction_at_all() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64]);
        let mut build = Builder::new(&mut source, block);
        let less = build.unary(Opcode::FNeg, args[0], f64);
        build.ret(&[less]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.xor_rr_64"), "{text}");
        assert!(!text.contains("sub"), "a negation is not a subtraction: {text}");
    }

    #[test]
    fn the_flags_reach_the_frame() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32]);
        Builder::new(&mut source, block).ret(&[args[0]]);

        let machine = Machine::x86_64(&SYSV);
        let flags = Flags { frame_pointer: true, profile: Profile::No, ..Flags::default() };
        let out = compile(&mut source, &mut names, &machine, &Elsewhere::default(), flags)
            .expect("every instruction has a rule");

        // A function that keeps a frame pointer keeps it whether it needed one or not, which is
        // what `-fno-omit-frame-pointer` is for and is the only thing this test is about.
        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.push_64 $rbp"), "{text}");
        assert!(text.contains("$rbp = x64.mov_rr_64 $rsp"), "{text}");
    }

    #[test]
    fn a_target_says_which_machine_it_is_and_which_convention_it_uses() {
        let triple = |text: &str| text.parse::<rucc_target::Triple>().expect("a triple");
        let info = TargetInfo::new(triple("x86_64-unknown-linux-gnu"));
        let machine = Machine::for_target(&info).expect("x86-64 is the target this crate covers");
        assert!(std::ptr::eq(machine.conv, &SYSV));

        let info = TargetInfo::new(triple("x86_64-pc-windows-msvc"));
        let machine = Machine::for_target(&info).expect("x86-64 is the target this crate covers");
        assert!(std::ptr::eq(machine.conv, &WIN64));

        // Not a target this crate has a backend for, and saying so is the whole point: a caller
        // that got a machine here would compile x86-64 instructions for an AArch64 program.
        let info = TargetInfo::new(triple("aarch64-unknown-linux-gnu"));
        assert!(Machine::for_target(&info).is_none());
    }
}
