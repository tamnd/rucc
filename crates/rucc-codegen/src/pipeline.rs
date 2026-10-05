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

use rucc_base::hash::Map;
use rucc_base::{Interner, Symbol};
use rucc_cost::Goal;
use rucc_ir as ir;
use rucc_mir as mir;
use rucc_regalloc::assign::Env;
use rucc_target::{
    BitInsts, BranchInsts, CallRegs, CodeModel, FlagInsts, FrameInsts, Isa, MachineInsts, PhysReg,
    RegFile, ShortInsts, Speculation, TargetInfo, TimingInsts, aarch64, x86, x86_64,
};
use rucc_tuple::Arch;

use crate::abi;
use crate::bits;
use crate::bytes;
use crate::choice;
use crate::cold;
use crate::combine;
use crate::compare;
use crate::copies;
use crate::coverage::Fired;
use crate::elsewhere::Elsewhere;
use crate::finish::{Convention, Padding, Probing, Protect, Tracing, far, finish};
use crate::fold;
use crate::frame::{self, Frame, Layout};
use crate::kept;
use crate::layout;
use crate::lifetimes;
use crate::lower::{self, Unsupported};
use crate::lowering::{self, Lowerings};
use crate::pressure::{Cost, Pressure};
use crate::schedule;
use crate::select::{self, Selector};
use crate::shorten;
use crate::slots::{self, Slots};
use crate::split;
use crate::tail;
use crate::thunks;
use crate::trailing;
use crate::usage::{StackUsage, Usage};
use crate::weights;
use crate::zero::{self, Zeroing};
pub use rucc_regalloc::Allocator;

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
    /// The same with the frame pointer offered last, for a function that keeps none, on a machine
    /// where that register can hold anything the others can. `None` where it cannot. See
    /// [`frame::keeps_frame_pointer`].
    pub spare: Option<Env>,
    /// [`Machine::env`] and [`Machine::spare`], in that order, with the general purpose scratch
    /// registers handed out as well, for a function that turns out not to need them. `None` on a
    /// machine whose scratch registers are ones the callee would have to save. See
    /// [`rucc_regalloc::run_either`].
    pub wide: Option<[Env; 2]>,
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
pub(crate) const AARCH64_SCRATCH: [PhysReg; 2] = [aarch64::X16, aarch64::X17];

/// The scratch registers held back from the allocator on i386.
///
/// Neither of the two that x86-64 uses exists here, and every register a call may destroy is one
/// an instruction insists on: `eax` and `edx` are the result and the two halves of a division, and
/// `ecx` is the count of a shift. So the two held back are `esi` and `edi`, which a call preserves
/// and which [`crate::frame`] saves when a reload writes one. That leaves `eax`, `ecx`, `edx` and
/// `ebx` to the allocator, which is also every register with a byte form on this machine.
pub(crate) const X86_SCRATCH: [PhysReg; 2] = [x86::ESI, x86::EDI];

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
        let env = |order: &[PhysReg]| {
            Env::new().with(x86_64::GPR, order, &SCRATCH).with(
                x86_64::XMM,
                &sse_order,
                &sse_scratch,
            )
        };
        // The frame pointer last, so a function only reaches for it once every other register is
        // in use, which is also the only time the push and the pop it costs are worth paying.
        // tamnd/rucc#2777.
        let spared: Vec<PhysReg> = order.iter().copied().chain([conv.frame_pointer]).collect();
        // Every register the convention hands out, `r10` and `r11` where it put them, which is
        // ahead of the ones a call preserves. tamnd/rucc#1994.
        let wide = |order: &[PhysReg]| {
            Env::new().with(x86_64::GPR, order, &[]).with(x86_64::XMM, &sse_order, &sse_scratch)
        };
        let every: Vec<PhysReg> = conv.int_order.to_vec();
        let spared_every: Vec<PhysReg> =
            every.iter().copied().chain([conv.frame_pointer]).collect();
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
            env: env(&order),
            spare: Some(env(&spared)),
            wide: Some([wide(&every), wide(&spared_every)]),
        }
    }

    /// The AArch64 machine under that convention.
    ///
    /// The scratch registers are `x16` and `x17`, which the convention already keeps out of the
    /// allocation order because a linker's veneer may write them between a call and the function
    /// it reaches. That is the property a scratch register wants: nothing lives in one across
    /// anything the compiler did not write, so a move the rewriter puts in can have it. The vector
    /// file's two are picked the way the x86 ones are, which lands on `v30` and `v31`.
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
            spare: None,
            wide: None,
        }
    }

    /// The machine for i386 under that convention.
    ///
    /// x86-64's tables for everything the two machines share, and the i386 ones for the frame, the
    /// branches, the shapes and the selector, which are what keep a sixty four bit instruction out.
    /// The vector registers are all caller saved, so two of them are held back the way they are on
    /// x86-64.
    #[must_use]
    pub fn x86(conv: &'static CallRegs) -> Self {
        let order: Vec<PhysReg> =
            conv.int_order.iter().copied().filter(|reg| !X86_SCRATCH.contains(reg)).collect();
        let (sse_order, sse_scratch) = held_back(conv);
        Self {
            conv,
            file: x86::REGS,
            insts: &x86::FRAME,
            branch: &x86::BRANCH,
            bits: &x86_64::BITS,
            flags: &x86_64::FLAGS,
            shapes: &x86::MACHINE,
            timing: &x86_64::TIMING,
            short: &x86_64::SHORT,
            selector: &select::x86::SELECTOR,
            env: Env::new().with(x86::GPR, &order, &X86_SCRATCH).with(
                x86::XMM,
                &sse_order,
                &sse_scratch,
            ),
            // Not on this machine. The four registers it allocates are the four with a byte form,
            // and `ebp` has none.
            spare: None,
            // Nor this. Its scratch registers are two a call preserves.
            wide: None,
        }
    }

    /// The two registers the stack protector's canary goes through, the first on the way in and
    /// both in the check on the way out.
    ///
    /// The scratch registers, which hold nothing at a return, on every machine but i386. There the
    /// scratch registers are `esi` and `edi`, which have no low byte for the check's `setne` and
    /// which a call preserves, and the registers a call does not preserve are the answer in `eax`
    /// and `edx`. So the canary goes in through `esi`, which the prologue saves for it, and the
    /// check reads the guard into `ecx`, which holds nothing at a return and has a low byte. `edx`
    /// is never one of them, since it holds the top half of a `long long` answer.
    #[must_use]
    pub fn guarded(&self) -> [PhysReg; 2] {
        if std::ptr::eq(self.selector, &select::x86::SELECTOR) {
            return [x86::ESI, x86::ECX];
        }
        let scratch = self.env.scratch(self.conv.int_class);
        [scratch[0], scratch[1]]
    }

    /// The same machine for a function written in another calling convention, which is what an
    /// `__attribute__((ms_abi))` function on Linux or an `__attribute__((sysv_abi))` one on
    /// Windows is compiled against. `None` when the platform has no such convention.
    ///
    /// Only the convention changes. The allocator's order and the scratch registers are worked out
    /// again from it, because which registers the function owes back is the thing that differs,
    /// and a scratch register has to be one it does not owe.
    ///
    /// The boundary the stack is kept on is the command line's rather than the convention's, so the
    /// answer keeps the boundary this machine has, it keeps the vector registers out of the
    /// arguments if this machine does, and its canary is where this machine's is. See
    /// [`rucc_target::CallRegs::aligned_to`], [`rucc_target::CallRegs::without_vectors`] and
    /// [`rucc_target::CallRegs::guarded_by`].
    #[must_use]
    pub fn under(&self, convention: rucc_target::Convention) -> Option<Self> {
        let mut conv = self.conv.under(convention)?.aligned_to(self.conv.stack_align);
        if self.conv.sse_args.is_empty() {
            conv = conv.without_vectors();
        }
        if let Some(guard) = self.conv.guard {
            conv = conv.guarded_by(guard);
        }
        if std::ptr::eq(self.selector, &select::aarch64::SELECTOR) {
            Some(Self::aarch64(conv))
        } else if std::ptr::eq(self.selector, &select::x86::SELECTOR) {
            Some(Self::x86(conv))
        } else {
            Some(Self::x86_64(conv))
        }
    }

    /// The same machine as an x86 interrupt handler sees it, with an error code below its frame
    /// or without one. See [`rucc_target::CallRegs::interrupted`] for what changes.
    #[must_use]
    pub fn interrupted(&self, code: bool) -> Self {
        let conv = self.conv.interrupted(code);
        if std::ptr::eq(self.selector, &select::aarch64::SELECTOR) {
            Self::aarch64(conv)
        } else if std::ptr::eq(self.selector, &select::x86::SELECTOR) {
            Self::x86(conv)
        } else {
            Self::x86_64(conv)
        }
    }

    /// The machine a target describes, or `None` when no backend in this crate covers it.
    ///
    /// [`TargetInfo`] already carries the convention, because the front end needs it to lay a
    /// `va_list` out, so the only thing this decides is which architecture's frame instructions
    /// and register file go with it. RISC-V is `None` until it has a rule file, and a caller that
    /// gets one reports a target it cannot compile for rather than compiling wrongly.
    #[must_use]
    pub fn for_target(target: &TargetInfo) -> Option<Self> {
        let conv = target.call_regs?;
        match target.tuple.arch() {
            Arch::X86_64 => Some(Self::x86_64(conv)),
            Arch::Aarch64 => Some(Self::aarch64(conv)),
            Arch::X86 => Some(Self::x86(conv)),
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

/// What `-mrecord-mcount` and `-mnop-mcount` ask of the call `-pg` writes. Both are x86 flags and
/// both do nothing on a function that has no such call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mcount {
    /// Whether the call's address goes in `__mcount_loc`.
    pub record: bool,
    /// Whether the call is a five byte nop instead.
    pub nop: bool,
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
    /// Where the code and static data are promised to be, which `-mcmodel=` says. Under the kernel
    /// model an address of a name wanted as a value is written as a number, and one with an index
    /// added to it is one instruction. See [`crate::fold::absolute`].
    pub code_model: CodeModel,
    /// Whether a frame is taken a page at a time, which `-fstack-clash-protection` asks for.
    pub stack_clash: bool,
    /// Whether every address an indirect branch may arrive at opens with a landing pad, which
    /// `-fcf-protection=branch` asks for. That is every function, and every label of a function
    /// whose address the program took.
    pub landing: bool,
    /// Whether the pad at the top of a function is only written in one that asks for it with
    /// `cf_check`, which `-mmanual-endbr` says. The pads at labels are not affected.
    pub manual_endbr: bool,
    /// Whether returns are checked against a shadow stack as well, which `-fcf-protection=return`
    /// and `full` ask for. With [`Self::landing`] it keeps a call that can come back by a jump
    /// from becoming one. See [`crate::tail::mark`].
    pub shadow_stack: bool,
    /// What the x86 speculation hardening flags ask of indirect branches and returns. See
    /// [`crate::thunks`].
    pub speculation: Speculation,
    /// Whether a `switch` may become a jump table, which `-fno-jump-tables` turns off. A jump
    /// through a table is an indirect jump, which a retpoline build pays a thunk for and an IBT
    /// build has to land on a pad for. See [`crate::switch::lowered`].
    pub jump_tables: bool,
    /// Whether the machine has a conditional move. Off only for an i386 `-march=` that names a
    /// processor before the Pentium Pro, where a select is a branch. See [`crate::forks`].
    pub cmov: bool,
    /// The extensions a function with no `target` attribute is built for, which `-march=` and the
    /// `-m` flags say. What reads it is the lowering of the bit counts, which leaves a count for
    /// the selector where the processor has the instruction for it. See [`crate::expand::counts`].
    pub isa: Isa,
    /// Whether every function calls a profiler on the way in, which `-pg` asks for.
    pub profile: Profile,
    /// What else is done with that call: listed in `__mcount_loc` for `-mrecord-mcount`, and
    /// written as a nop for `-mnop-mcount`. See [`rucc_mir::Mcount`].
    pub mcount: Mcount,
    /// The hook the call goes to in place of the target's, which `-mfentry-name=` names, and
    /// which a function's own `fentry_name` overrides.
    pub fentry_name: Option<Symbol>,
    /// The section the call is listed in in place of `__mcount_loc`, which `-mfentry-section=`
    /// names, and which a function's own `fentry_section` overrides.
    pub fentry_section: Option<Symbol>,
    /// How much room every function opens with for a patcher, which
    /// `-fpatchable-function-entry=` asks for. See [`Room`].
    pub patch: Room,
    /// Whether the blocks are put in the order the weights say rather than in the order the
    /// shape of the graph says, which `-freorder-blocks` asks for and every level above `-O0`
    /// turns on. See [`crate::layout`].
    pub reorder: bool,
    /// Whether the blocks that are not expected to run are put in a part of the function of their
    /// own, which `-freorder-blocks-and-partition` asks for. See [`crate::cold`]. Only a target
    /// whose listing can write the second part asks for it, which is x86-64 ELF.
    pub partition: bool,
    /// Whether two locals that are never both wanted may be the same bytes, which
    /// `-fstack-reuse=none` turns off and `-O0` does not ask for. Spill slots share whatever this
    /// says, since a spill slot is not a variable and nothing can ask a debugger for one. See
    /// [`crate::slots`].
    pub reuse: bool,
    /// Whether the instructions of a block are put in the order the machine finishes soonest,
    /// which `-fschedule-insns2` asks for and every level from `-O2` turns on. See
    /// [`crate::schedule`].
    pub schedule: bool,
    /// Whether the head of every hot loop is found, so that the loop can be padded to stay inside
    /// one line, which `-falign-loops` asks for and no level turns on by itself yet. See
    /// [`crate::layout::heads`].
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
    /// Which register allocator decides where the values go, which `-Zregalloc=` says. See
    /// [`rucc_regalloc::Allocator`].
    pub allocator: Allocator,
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
    /// The shape `-Zswitch=` forces on every `switch`, which is `None` unless somebody is
    /// measuring what each shape costs. See [`crate::switch::Force`].
    pub switch: Option<crate::switch::Force>,
    /// Whether a call in tail position becomes a jump, which `-foptimize-sibling-calls` asks for
    /// and `-O2` and `-Os` turn on. See [`crate::tail`].
    pub sibling: bool,
    /// Whether the build writes debugging information, which `-g` asks for. Where each local is
    /// over which instructions is worked out only then, since nothing else reads it. See
    /// [`crate::kept`].
    pub debug: bool,
    /// Whether a value may be kept in a vector register, which `-mno-sse` on x86-64 and
    /// `-mgeneral-regs-only` on either machine turn off. Every `float`, `double` and vector is one
    /// of those, and a function with one in it is refused. See [`lower::off_registers`].
    pub vector: bool,
    /// Whether a value may be kept on the x87 stack, which `-mno-80387` turns off. That is the
    /// eighty bit `long double`, and a function with one in it is refused too.
    pub x87: bool,
    /// Whether a `long double` may be returned on the x87 stack, which `-mno-fp-ret-in-387` turns
    /// off. A function returning one is refused even with the stack on.
    pub x87_return: bool,
    /// Which registers `-fzero-call-used-regs=` has every `ret` clear, `None` for `skip`, which a
    /// function's `zero_call_used_regs` attribute replaces for that function. See [`crate::zero`].
    pub zero: Option<Zeroing>,
    /// The last of the extensions that change how the zeroing above clears a vector register
    /// that the unit may use, which is AVX or AVX-512F on x86-64. See [`crate::zero::Extension`].
    pub zero_extension: zero::Extension,
    /// Whether a function whose last instruction is a call gets a trap after it, so that the
    /// address the call returns to is inside the function. x86-64 Windows asks for it, whose
    /// unwinder finds a frame's record by that address. See [`crate::trailing`].
    pub trailing: bool,
}

impl Default for Flags {
    /// No frame pointer, the red zone allowed, the frame taken in one subtraction, no landing pad,
    /// no branch rewritten for speculation, jump tables allowed, no profiling, no room for a patcher, the blocks in the order the graph's shape gives,
    /// nothing in the frame sharing with anything, no scheduling, no loop padded to a boundary and
    /// code that is meant to be fast rather than small, with no debugging information and every register file in use, which is
    /// what a convention that has a red zone says at `-O0` when nobody on the command line has said
    /// otherwise.
    fn default() -> Self {
        Self {
            frame_pointer: false,
            red_zone: true,
            code_model: CodeModel::Small,
            stack_clash: false,
            landing: false,
            shadow_stack: false,
            manual_endbr: false,
            speculation: Speculation::default(),
            jump_tables: true,
            cmov: true,
            isa: Isa::NONE,
            profile: Profile::No,
            mcount: Mcount::default(),
            fentry_name: None,
            fentry_section: None,
            patch: Room::default(),
            reorder: false,
            partition: false,
            reuse: false,
            schedule: false,
            align_loops: false,
            accurate: None,
            verify: false,
            allocator: Allocator::Single,
            goal: Goal::Speed,
            switch: None,
            sibling: false,
            debug: false,
            vector: true,
            x87: true,
            x87_return: true,
            zero: None,
            zero_extension: zero::Extension::Sse,
            trailing: false,
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
    let (mut fired, mut pressure, mut lowerings, mut stack) =
        (Fired::new(), Pressure::new(), Lowerings::new(), StackUsage::new());
    compile_recording(
        source,
        names,
        machine,
        elsewhere,
        flags,
        &mut Recording {
            fired: &mut fired,
            pressure: &mut pressure,
            lowerings: &mut lowerings,
            stack: &mut stack,
        },
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
    /// How much stack each function takes, for `-fstack-usage`.
    pub stack: &'a mut StackUsage,
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
///
/// # Panics
///
/// When a function laid out as a leaf because its only calls were tail calls is left with one of
/// them still a call, which is a bug in this crate rather than anything the program did.
pub fn compile_recording(
    source: &mut ir::Func,
    names: &mut Interner,
    machine: &Machine,
    elsewhere: &Elsewhere,
    flags: Flags,
    recording: &mut Recording<'_>,
) -> Result<mir::Func, Unsupported> {
    // A function the program wrote in the platform's other calling convention is compiled against
    // that convention from the first pass to the last: its parameters arrive where it says, the
    // registers it owes back are the ones it says, and its frame has the room it says. The calls
    // it makes each name their own convention, so they are unaffected by this.
    let foreign;
    let machine = match source.signature().convention {
        rucc_target::Convention::Target => machine,
        convention => {
            foreign = machine
                .under(convention)
                .ok_or(Unsupported::Unported { inst: None, what: lower::Unported::Convention })?;
            &foreign
        }
    };
    // An x86 interrupt handler is compiled against the convention the processor rather than a
    // caller hands it, which differs in where its frame starts and in having no red zone, and the
    // answer here is whether one more word, the error code, sits under that frame. A handler
    // takes it when it was written with two parameters. Everything else about the function keeps
    // its convention, and a handler owes back every register it writes, which is `saves_all`.
    let interrupt = (source.attrs.set.contains(ir::AttrSet::INTERRUPT)
        && machine.insts.iret.is_some())
    .then(|| source.signature().params.len() == 2);
    let saves_all = interrupt.is_some() || source.attrs.set.contains(ir::AttrSet::SAVES_ALL);
    let interrupted;
    let machine = match interrupt {
        Some(code) => {
            interrupted = machine.interrupted(code);
            &interrupted
        }
        None => machine,
    };
    // Everything the machine has no rule for, rewritten into things it has, as one group rather
    // than as a dozen lines here. What is in the group and what the order between its members is
    // for are both in `crate::lowering`, which is where a new lowering is added.
    let counting = recording.lowerings.wanted();
    let switching = (flags.goal, flags.switch, flags.jump_tables, flags.cmov);
    // The bit counts the selector is left to answer, which are the ones it has a rule for and the
    // processor the function is built for has the extension of. A `target` attribute on the
    // function says what it is built for in place of the command line.
    let isa = source.target.unwrap_or(flags.isa);
    let counts: Vec<rucc_target::CountInst> =
        machine.selector.counts.iter().copied().filter(|count| count.on(isa)).collect();
    let ran = lowering::group(source, names, machine.conv, switching, &counts, counting);
    if !ran.switches.is_empty() {
        let called = names.resolve(source.name).to_owned();
        recording.lowerings.switched(&called, &ran.switches);
    }
    if counting {
        let called = names.resolve(source.name).to_owned();
        recording.lowerings.record(&called, ran);
    }
    // The function the program said it writes the whole of itself, which is what decides most of
    // the frame below rather than being one more thing in it. Read here rather than beside the rest
    // of the layout because the refusal a few lines down is the earliest thing that asks.
    let naked = source.attrs.set.contains(ir::AttrSet::NAKED);
    // The branches a function said to leave plain, which the thunks pass at the end is told of.
    let mut speculation = flags.speculation;
    if source.attrs.set.contains(ir::AttrSet::RETURN_KEEP) {
        speculation.returns = rucc_target::Thunk::Keep;
    }
    if source.attrs.set.contains(ir::AttrSet::INDIRECT_KEEP) {
        speculation.indirect = rucc_target::Thunk::Keep;
    }
    let zeroing = zeroing(flags.zero, source.attrs.set);
    // Last thing before selection, because a `tail_call` ends its block and every lowering above
    // is written against blocks that end the way the middle end left them. Only on a machine that
    // can jump to a name, since the call stays a call on one that cannot.
    // Not in a function that owes back every register either. A jump to another function hands
    // that function the registers to write as it likes, and what this one promised is that none
    // of them would be different when it returns, so the call stays a call and the registers are
    // put back after it. An interrupt handler also returns with `iretq`, which a jump would skip.
    if flags.sibling && machine.insts.away.is_some() && !saves_all {
        let guarded = flags.landing
            && flags.shadow_stack
            && !source.attrs.set.contains(ir::AttrSet::INDIRECT_RETURN);
        tail::mark(source, names, elsewhere, guarded);
    }
    // Asked of the IR, where a call still says whom it calls. See [`tail::comes_back`].
    let alone = tail::comes_back(source, names, elsewhere);
    // The ends of lifetimes the front end wrote, which only the sharing of slots reads and which
    // are only worth anything where no value the optimizer made carries an address past one. After
    // [`tail::comes_back`] because that is what says whether anything will share. See
    // [`crate::lifetimes`].
    lifetimes::settle(source, flags.reuse && !alone);
    // Before selection, which would otherwise pick a register the command line said is not there.
    // Read after the lowerings above, since a value one of them makes is a value in the function.
    // A function that owes back every register owes back the vector registers and the x87 stack
    // too, and saving those is a thing gcc does not do either: it refuses such a function when its
    // target has them, which the front end does for this one. What is left is the target that has
    // none, or a function with nothing in it that needs one, and taking both away here is what
    // keeps a value from reaching for them all the same.
    let vectors = flags.vector && !saves_all;
    let x87 = flags.x87 && !saves_all;
    let stacked = machine.conv.word == 4;
    lower::off_registers(source, vectors, x87, flags.x87_return, stacked)?;
    let lowered =
        lower::func_for(source, names, machine.selector, machine.conv, elsewhere, flags.debug)?;
    recording.fired.merge(&lowered.fired);
    let lower::Lowered { mut func, mut stack, blocks, .. } = lowered;
    // Straight after selection, because this is the last moment the machine blocks and the IR
    // blocks still stand one for one, and the pass that reads the numbers is the very last one
    // there is. See `crate::weights`.
    if flags.reorder {
        weights::carry(source, &blocks, &mut func);
    }
    // At the same moment and for the same reason, and read by the same pass. See `crate::cold`.
    if flags.reorder && flags.partition {
        cold::mark(source, &blocks, &mut func, elsewhere);
    }
    // What the refusal before selection missed, which would be a rule that reaches for a vector
    // register on its own to do something that is not about a float at all. None does now, and
    // this is what says so if one ever starts.
    let vector =
        |number| func.class_of(rucc_mir::Reg::virtual_reg(number)) == Some(machine.conv.sse_class);
    if !vectors && (0..func.vregs()).filter_map(|n| u32::try_from(n).ok()).any(vector) {
        return Err(Unsupported::Registers { inst: None, ty: None, off: lower::Off::Vector });
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
    // A constant added to an index goes into the displacement first, so an address that took one
    // is handed on to its readers with it already inside.
    fold::offsets(&mut func, machine.insts, machine.shapes, names);
    let mut pending = fold::Pending {
        addresses: &mut stack.addresses,
        arguments: &mut stack.arguments,
        dynamic: &mut stack.dynamic,
    };
    fold::addresses(
        &mut func,
        machine.insts,
        machine.shapes,
        names,
        &mut pending,
        flags.code_model,
    );

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
    // Last of the three, so that a name read or written directly is still read from the
    // instruction pointer and only an address wanted as a value is written as a number.
    if flags.code_model == CodeModel::Kernel {
        fold::absolute(&mut func, machine.insts, names);
        fold::tables(&mut func, machine.insts, names);
    }

    // Whether this function carries a canary is the front end's answer, because what
    // `-fstack-protector` asks about is the kind of local a function has and the types are gone by
    // here. What the machine does about it is this crate's answer, and a target with nowhere to
    // keep the word a canary is copied from does nothing, which is what the driver refuses a
    // command line over before any of this runs.
    // Not in a naked function, whatever the command line asked of every function. The canary is a
    // word the prologue copies into the frame and the check at the end reads back, so a function
    // with neither has nowhere to put it and nowhere to read it from. gcc leaves one out too.
    // A local the front end said wants one counts only while its slot is still in the frame, so a
    // function whose arrays and escaping locals were all turned into values has none, as with gcc.
    let guarded = || {
        source.blocks().flat_map(|block| source.insts(block)).any(|inst| {
            source[inst].opcode == ir::Opcode::Alloca
                && source[inst].flags.contains(ir::Flags::GUARD)
        })
    };
    let protect = (source.attrs.set.contains(ir::AttrSet::STACK_PROTECT) || guarded()) && !naked;
    let guard = protect.then_some(machine.conv.guard.as_ref()).flatten();
    // Nothing at all on a target with no hook to call, which is the same answer the protector gives
    // on a target with nowhere to keep its word, and the driver refuses the command line over it
    // before any of this runs.
    // A function that said `no_instrument_function` gets none either, as with gcc, which is how the
    // kernel's `notrace` keeps the tracer out of itself.
    let profile = match machine.conv.trace {
        Some(_) if !source.attrs.set.contains(ir::AttrSet::NO_INSTRUMENT) => flags.profile,
        _ => Profile::No,
    };
    // A tail call is a call on a machine with no instruction to jump away with, and the frame is
    // worked out as though it stays one.
    stack.kept |= machine.insts.away.is_none() && !stack.tails.is_empty();
    let base = Layout { moves: machine.insts.classes, ..Layout::new(machine.conv, machine.file) };
    let base = stack.layout(base);
    let guarded = machine.guarded();
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
        //
        // Nor in an interrupt handler, whose convention says there is none for the reason
        // [`rucc_target::CallRegs::interrupted`] gives.
        red_zone: flags.red_zone && !naked && interrupt.is_none(),
        // Two registers to a push on a machine with an instruction for it, which is `stp` on
        // AArch64, and one at a time on x86-64, which has none.
        pairs: machine.insts.pair.is_some(),
        protect: guard.is_some(),
        guarded: &guarded,
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
        saves_all,
        vectors,
        // Not in a naked function, which has no prologue to do the aligning in.
        forced: !naked && source.attrs.set.contains(ir::AttrSet::FORCE_ALIGN),
        // What comes back comes back in the general purpose registers, since a function that
        // saves everything has had the other files taken away above, and one register to a value
        // in the order the convention hands them out.
        returned: {
            let returns = machine.conv.int_returns;
            &returns[..source.signature().returns.len().min(returns.len())]
        },
        interrupt,
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
    let reach = (flags.reuse && !alone).then(|| {
        let count = stack.locals.len();
        slots::reach(&func, &stack.addresses, &stack.ends, count, machine.insts, names)
    });

    // The instructions as they are now, for the locals the front end kept in values. The
    // allocator's liveness is counted along this order and the rewrite is about to put spills,
    // reloads and edge moves in among them, so the list has to be taken before it runs. Only in a
    // function that named something, since a function that named nothing has no use for it. See
    // [`crate::kept`].
    //
    // Or where a local the program declared may share its bytes, which is only where there is a
    // `reach`, since a local that shares is in the frame over part of the function and the part is
    // asked about the same way.
    //
    // Only for a build that writes debugging information, since the stretches are read by nothing
    // else.
    let line = (flags.debug
        && (!func.named.is_empty() || (reach.is_some() && !stack.declared.is_empty())))
    .then(|| kept::before(&func));

    // Which arithmetic reads its two sources either way round, so the allocator may write the
    // answer over whichever of the two is finished with. Marked here rather than at selection
    // because every pass in between that rewrites an instruction would have to carry the mark.
    commuting(&mut func, machine.shapes, names);

    // The single pass allocator in a function that can be come back into. A value live across the
    // call that comes back has to be read by the second arrival from memory the first arm did not
    // write, and the backtracking allocator is free to leave it in a register the first arm writes.
    let allocator = if alone { Allocator::Single } else { flags.allocator };
    let called = names.resolve(func.name).to_owned();
    // One more register in a function that will keep no frame pointer, which is asked of the parts
    // of the frame the allocator cannot change. Not in a naked function, which has no prologue to
    // save it in, nor in one that saves every register, which has a list of its own to save.
    let spare = machine
        .spare
        .as_ref()
        .filter(|_| !naked && !saves_all && !frame::keeps_frame_pointer(&layout));
    let env = spare.unwrap_or(&machine.env);
    // The scratch registers handed out too, where nothing after allocation writes into one while a
    // value may be in it. The protector's check writes into one at every return, and the walk over
    // the pages of a frame that grows writes into one in the middle of the body. The rest of what
    // this file puts in a scratch register goes in the prologue, before anything the allocator
    // placed is live.
    let wide = machine
        .wide
        .as_ref()
        .filter(|_| !naked && !saves_all && !layout.grows && guard.is_none())
        .map(|[plain, spared]| if spare.is_some() { spared } else { plain });
    let allocation = match wide {
        Some(wide) => {
            rucc_regalloc::run_either(&mut func, wide, env, &called, flags.verify, allocator)
        }
        None => rucc_regalloc::run_with(&mut func, env, &called, flags.verify, allocator),
    };
    recording.pressure.record(&called, Cost::of(&allocation));

    // After allocation, because the largest area in most frames is the spill slots and nothing
    // knows how many of those there are until the allocator has finished running out of registers,
    // and because a spill slot cannot be shared with a local until it is known there is one.
    let widths = frame::widths(&layout, &func, &allocation);
    // Nothing shares in a function that can be come back into, since the second arrival reads
    // bytes the liveness says nobody wants. See [`tail::comes_back`].
    let share = if alone {
        Slots::apart(&stack.locals, &widths)
    } else {
        Slots::share(&func, reach.as_ref(), &allocation, &stack.locals, &widths)
    };
    // The ends of lifetimes have said all they had to, and they are markers the target has no
    // encoding for, so they come out before anything is laid out or written.
    for &(inst, _) in &stack.ends {
        if func.block_of(inst).is_some() {
            func.remove_inst(inst);
        }
    }
    let mut layout = Layout { share: Some(&share), ..layout };
    let mut frame = Frame::of(&func, &allocation, &layout);
    // A frame bigger than a page is taken by calling the platform's routine for it, and on a
    // machine whose call leaves the return address in a register that call writes over it. So such
    // a function is not a leaf, whatever the program called, and has to save the register the way
    // any other caller does. AArch64 Windows is the one, and clang saves x29 and x30 there too.
    let page = machine.insts.probe.as_ref().map_or(u32::MAX, |probe| probe.interval);
    if layout.leaf
        && machine.conv.chkstk.is_some()
        && machine.conv.link.is_some()
        && frame.size() > page
    {
        layout = Layout { leaf: false, ..layout };
        frame = Frame::of(&func, &allocation, &layout);
    }
    // The register was handed out on the promise that it would not be one, and a frame pointer set
    // up over a value the allocator put there is a wrong program rather than a slow one.
    assert!(
        spare.is_none() || !frame.frame_pointer(),
        "a function given the frame pointer to allocate kept one"
    );
    // The one thing a naked function cannot be given. Everything else the attribute asks for is
    // something left out, and leaving something out always works; bytes are the one thing the body
    // may want that only a prologue provides. A local, a spilled value and the arguments of a call
    // are the three ways to want them, and the answer to all three is the same sentence.
    if naked && frame.size() > 0 {
        return Err(Unsupported::Naked { bytes: frame.size() });
    }
    // Here because the frame is settled and nothing after this changes how big it is. The name is
    // the one the source spelled, since that is what gcc's report says and what a person reading
    // it looks for, and a renamed function is the one place it differs from the symbol.
    recording.stack.record(Usage {
        name: names.resolve(source.spelled.unwrap_or(source.name)).to_owned(),
        named: source.named,
        declared: source.declared,
        bytes: frame.usage(),
        dynamic: frame.grows(),
    });

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
        .chain(stack.passed.iter().filter_map(|&(decl, up)| Some((decl, i32::try_from(up).ok()?))))
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
    let protect = guard.map(|guard| Protect { guard, branch: machine.branch, scratch: guarded });
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
    // The hook and the list are the function's own where `fentry_name` and `fentry_section` said,
    // and the command line's otherwise. A section the function named lists the call whether or not
    // `-mrecord-mcount` asked, which is what gcc does, while one only the command line named is
    // where the calls go when they are listed and nothing more.
    let trace = machine.conv.trace.and_then(|trace| {
        let (hook, early) = match profile {
            Profile::No => return None,
            Profile::Early => (trace.early, true),
            Profile::Late => (trace.late, false),
        };
        let name = match source.fentry_name.or(flags.fentry_name) {
            Some(name) => name,
            None => names.intern(hook),
        };
        let record = flags.mcount.record || source.fentry_section.is_some();
        let mcount = Mcount { record, ..flags.mcount };
        let section = source.fentry_section.or(flags.fentry_section);
        Some(Tracing { name, early, mcount, section })
    });
    // And once more for the room a patcher was promised, which is a run of the shortest
    // instruction that does nothing and so needs the target to have one. Nothing is written on a
    // target that does not, rather than a run of something longer: the flag counts bytes, and a
    // patcher writing over the room starts at its front and wants every byte in it to be a place
    // it could have started at.
    // What `patchable_function_entry` on the function said, in place of the command line.
    let room = source.patchable.map_or(flags.patch, |(total, before)| Room {
        after: total.saturating_sub(before),
        before,
    });
    let pad = room.any().then_some(machine.insts.pad).flatten().map(|name| Padding {
        name,
        before: room.before,
        after: room.after,
    });
    // The pad at the top of the function is left out of one whose type says `nocf_check`, and
    // under `-mmanual-endbr` out of every one that does not say `cf_check`. The pads at its
    // labels stay, since a computed `goto` is a different branch.
    let asked = !flags.manual_endbr || source.attrs.set.contains(ir::AttrSet::CF_CHECK);
    let entry = landing.filter(|_| asked && !source.attrs.set.contains(ir::AttrSet::NOCF));
    let convention = Convention {
        protect,
        probe,
        landing: entry,
        trace,
        pad,
        ..Convention::new(machine.conv, machine.insts)
    };
    let moves = finish(&mut func, &allocation, &frame, &stack, convention, names);

    // After the moves are written, because a spill and the reload of it are written by different
    // decisions of the allocator and what stands between the two is settled by the function they
    // both went into. Before the layout, because the layout is where the instruction sequence
    // stops being something a pass may edit.
    let pointer = frame.frame_pointer();
    copies::clean(&mut func, &moves, machine.shapes, machine.insts, machine.conv, pointer, names);

    // After the moves are cleaned up, since that pass follows what the scratch registers hold, and
    // before the schedule, which should see the extra `add` as the instruction it is.
    far(&mut func, machine.insts, machine.conv, scratch, names);

    // After the allocator's moves have been cleaned up, because a schedule chosen around a move
    // that is about to be taken out is a schedule built around an instruction that is not in the
    // output. Before the layout, because the layout is the freeze: it writes the jumps the block
    // order needs and it puts a comparison and the branch that reads it together, and neither
    // survives an instruction being moved in afterwards. That is section 38.6's placement, and the
    // reason it is after allocation rather than before is in [`crate::schedule`].
    if flags.schedule {
        schedule::insts(
            &mut func,
            (machine.conv.stack_pointer, machine.conv.int_class),
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
    //
    // The allocator's moves are the copies it may take out, and the record of which they are is the
    // one the cleanup above read. A function never hands out an id twice, so an instruction a pass
    // in between wrote is one the record does not know rather than one it knows as something else.
    let allocated = |inst| moves.at(inst).is_some();
    shorten::shorter(
        &mut func,
        machine.short,
        machine.flags,
        machine.shapes,
        names,
        flags.goal,
        &allocated,
    );

    // After every pass that writes or rewrites an instruction over the registers it was given, so
    // that the byte an instruction names is the one it ends up with. i386 only: see
    // [`crate::bytes`].
    if std::ptr::eq(machine.selector, &select::x86::SELECTOR) {
        bytes::reach(&mut func, machine.insts.prefix, machine.conv.int_class, names);
    }

    // Once the blocks will not move again, since a head is a block a jump runs backwards to and
    // which way a jump runs is the layout's answer. Nothing below adds or takes out a block.
    if flags.align_loops {
        func.heads = layout::heads(&func);
    }

    // After everything that edits instructions, because a call is the one instruction all of them
    // leave alone and a jump out of the function is one some of them would not know about. Nothing
    // before this sees anything but a call, a return and an epilogue, which is right on its own.
    //
    // A frame with no call left in it but these was laid out as a leaf, so each of them has to go:
    // one left a call would be made with the stack pointer wherever the leaf left it and with the
    // return address of a function that never saved its own.
    //
    // A frame laid out as anything but a leaf is fine with a call left in it, and a protected one
    // is one of those: its check comes between the call and the `ret`, so the call has to stay
    // where it is, and the frame already leaves the stack pointer where a call needs it.
    let jumped = tail::jumps(&mut func, &stack.tails, machine.insts, machine.branch, names);
    assert!(
        stack.kept || !layout.leaf || jumped == stack.tails.len(),
        "a leaf whose tail calls did not all become jumps"
    );

    // A landing pad after every call that can come back by a jump, which is one to a
    // `returns_twice` function or one of an `indirect_return` type, as in gcc. After the tail
    // calls, since one that became a jump comes back to this function's caller instead, and after
    // everything that moves or adds instructions, so nothing comes between the call and its pad.
    // `-mmanual-endbr` leaves these where they are, as it does the pads at labels.
    split::after_calls(&mut func, machine.insts, landing, names);

    // After the tail calls, because a `ret` a tail call replaced leaves through the callee's, and
    // before the mitigations, which may turn a `ret` into a jump.
    // What the function may have left in a register other than a general purpose one, which the
    // choices without `-gpr` clear too. A `long double` it gives back on the x87 stack is in a
    // register that is not cleared, the way one in `rax` is not.
    let types: Vec<ir::Type> = source.signature().returns.iter().map(|ret| ret.ty).collect();
    let files = zero::Files {
        vector: vectors,
        extension: flags.zero_extension,
        x87: flags.x87 && !saves_all,
        x87_returned: if abi::back_on_x87(&types) { types.len() } else { 0 },
    };
    zero::apply(&mut func, zeroing, files, machine.shapes, &machine.file, names);

    // After the tail jumps, because a `ret` that became a jump to a callee is a direct jump and no
    // longer a return, and after everything else for the reason in [`crate::thunks`]: the thunk a
    // branch goes to is named after the register the allocator gave it.
    thunks::harden(&mut func, machine.insts, speculation, names);

    // After every pass that could leave a call at the end of the function or take one away, the
    // thunks included, since a call through a thunk is still a call. See [`crate::trailing`].
    if flags.trailing {
        trailing::trap(&mut func, machine.selector, machine.shapes, names);
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

/// Which registers a function's returns clear: what its `zero_call_used_regs` said if it said
/// anything, and what `-fzero-call-used-regs=` said if not.
fn zeroing(command: Option<Zeroing>, set: ir::AttrSet) -> Option<Zeroing> {
    let arg = set.contains(ir::AttrSet::ZERO_ARG);
    let wide = set.contains(ir::AttrSet::ZERO_WIDE);
    if set.contains(ir::AttrSet::ZERO_SKIP) {
        None
    } else if set.contains(ir::AttrSet::ZERO_ALL) {
        Some(Zeroing { all: true, arg, wide })
    } else if set.contains(ir::AttrSet::ZERO_USED) {
        Some(Zeroing { all: false, arg, wide })
    } else {
        command
    }
}

/// Marks every instruction the machine says reads its two sources either way round.
fn commuting(func: &mut mir::Func, shapes: &MachineInsts, names: &Interner) {
    let insts: Vec<mir::Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    // Asked once per opcode rather than by name for every instruction. tamnd/rucc#2233.
    let mut known: Map<mir::Opcode, bool> = Map::default();
    for inst in insts {
        let opcode = func[inst].opcode;
        let commutes =
            *known.entry(opcode).or_insert_with(|| shapes.commutes(names.resolve(opcode.name())));
        if commutes {
            func[inst].flags = func[inst].flags.with(mir::Flags::COMMUTES);
        }
    }
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

    /// `int f(int a) { return g(a) + a; }` compiled for AArch64 and printed.
    fn aarch64_call() -> String {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32]);
        let sig = source.add_signature(Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let callee = names.intern("g");
        let call = Builder::new(&mut source, block).call(callee, sig, &[args[0]]);
        let got = source[call].first_result.expect("an integer comes back");
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::Add, got, args[0], IrFlags::default());
        build.ret(&[sum]);

        let machine = Machine::aarch64(&aarch64::AAPCS64);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");
        mir::print_func(&out, &names, &aarch64::REGS)
    }

    #[test]
    fn an_aarch64_function_that_calls_keeps_its_return_address_in_a_frame_record() {
        let text = aarch64_call();
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let first = |what: &str| lines.iter().position(|line| line.contains(what));
        // The call writes over x30, so it goes on the stack with x29 before anything else, and the
        // frame pointer is pointed at the pair.
        let record = first("a64.push_pair_64 $x29, $x30").unwrap_or_else(|| panic!("{text}"));
        let pointed = first("$x29 = a64.mov_rr_64 $sp").unwrap_or_else(|| panic!("{text}"));
        let call = first("a64.bl").unwrap_or_else(|| panic!("{text}"));
        let back = first("a64.pop_pair_64").unwrap_or_else(|| panic!("{text}"));
        let ret =
            lines.iter().position(|&line| line == "a64.ret").unwrap_or_else(|| panic!("{text}"));
        assert!(record < pointed && pointed < call && call < back && back < ret, "{text}");
        // Every push moves the stack pointer by sixteen, so whatever the frame takes on top of them
        // is a multiple of sixteen too, and nothing is taken for the word x86 would have owed.
        for line in &lines {
            if let Some(rest) = line.split("a64.sub_ri_64 $sp, ").nth(1) {
                let size: u32 = rest.parse().unwrap_or_else(|_| panic!("{text}"));
                assert_eq!(size % 16, 0, "{text}");
            }
        }
    }

    #[test]
    fn an_aarch64_leaf_keeps_no_frame_record() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut source, block);
        let sum = build.binary(Opcode::Add, args[0], args[1], IrFlags::default());
        build.ret(&[sum]);

        let machine = Machine::aarch64(&aarch64::AAPCS64);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");
        let text = mir::print_func(&out, &names, &aarch64::REGS);
        assert!(!text.contains("push"), "{text}");
        assert!(text.contains("a64.add_rr_32"), "{text}");
    }

    /// A byte swap and the two zero counts are `rev`, `clz`, and `rbit` then `clz` on any AArch64,
    /// at both widths, with nothing written out around them.
    #[test]
    fn an_aarch64_byte_swap_or_zero_count_is_the_instruction_for_it() {
        for (opcode, wanted) in [
            (Opcode::Bswap, &["a64.rev_r"][..]),
            (Opcode::Ctlz, &["a64.clz_r"][..]),
            (Opcode::Cttz, &["a64.rbit_r", "a64.clz_r"][..]),
        ] {
            for width in [32, 64] {
                let ty = Type::int(width);
                let (mut names, mut source, block, args) = blank(&[ty]);
                let mut build = Builder::new(&mut source, block);
                let done = build.unary(opcode, args[0], ty);
                build.ret(&[done]);
                let machine = Machine::aarch64(&aarch64::AAPCS64);
                let out = compile(
                    &mut source,
                    &mut names,
                    &machine,
                    &Elsewhere::default(),
                    Flags::default(),
                )
                .expect("every instruction has a rule");
                let text = mir::print_func(&out, &names, &aarch64::REGS);
                for inst in wanted {
                    assert!(text.contains(&format!("{inst}_{width}")), "{inst}_{width}: {text}");
                }
                assert!(!text.contains("lsr") && !text.contains("mul"), "written out: {text}");
            }
        }
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
        // addition needs, since the sum is written into the register one of the two operands was
        // read from and the return wants it in `rax`. The addition is marked as reading them
        // either way round, which is why the allocator was free to pick.
        assert_eq!(
            mir::print_func(&out, &names, &REGS),
            "mfunc @f {\n\
             block0:\n    \
             $rdi($rdi) = x64.arg_val_32\n    \
             $rsi($rsi) = x64.arg_val_32\n    \
             $rdi(reuse 1) = x64.add_rr_32 commutes $rdi, $rsi\n    \
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
        let flags = Flags { debug: true, ..Flags::default() };
        let out = compile(&mut source, &mut names, &machine, &Elsewhere::default(), flags)
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
        let ones = build.unary(Opcode::Ctpop, args[0], i32);
        build.ret(&[ones]);

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
                stack: &mut StackUsage::new(),
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
        // The bit count went through the group rather than reaching the selector, which has no
        // rule for one.
        assert!(listing.contains("counts"), "{listing}");
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
                stack: &mut StackUsage::new(),
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
                stack: &mut StackUsage::new(),
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

    /// `int f(int a, ...) { return g(a, ...); }` with that many arguments, compiled with sibling
    /// calls on or off, as the machine code that comes out.
    fn tail(count: usize, sibling: bool) -> String {
        let i32 = Type::int(32);
        let params = vec![i32; count];
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&params).with_returns(&[i32]);
        let mut source = Func::new(names.intern("f"), signature.clone());
        let block = source.create_block();
        let args: Vec<_> = params.iter().map(|&ty| source.append_param(block, ty)).collect();
        let sig = source.add_signature(signature);
        let callee = names.intern("g");
        let call = Builder::new(&mut source, block).call(callee, sig, &args);
        let got = source[call].first_result.expect("an integer comes back");
        Builder::new(&mut source, block).ret(&[got]);

        let machine = Machine::x86_64(&SYSV);
        let flags = Flags { sibling, ..Flags::default() };
        let out = compile(&mut source, &mut names, &machine, &Elsewhere::default(), flags)
            .expect("every instruction has a rule");
        mir::print_func(&out, &names, &REGS)
    }

    /// A call whose answer is the answer ends in a jump to it once the frame is given back, and
    /// only when the flag says so.
    #[test]
    fn a_call_in_tail_position_is_a_jump_when_asked_for() {
        let text = tail(2, true);
        assert!(text.contains("x64.jmp_away @g"), "{text}");
        assert!(!text.contains("x64.call"), "{text}");
        assert!(!text.contains("x64.ret"), "{text}");

        let text = tail(2, false);
        assert!(text.contains("x64.call"), "{text}");
        assert!(text.contains("x64.ret"), "{text}");
    }

    /// `int f(int (*g)(int), int a) { return g(a); }`, which gives its frame back and jumps through
    /// the register the address is in, and that register is one the epilogue leaves alone.
    #[test]
    fn a_call_through_a_pointer_in_tail_position_is_a_jump_through_a_register() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[Type::PTR, i32]);
        source.set_signature(Signature::new().with_params(&[Type::PTR, i32]).with_returns(&[i32]));
        let sig = source.add_signature(Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let varargs = source.push_abis(&[]);
        let info = source.add_call(ir::CallInfo { callee: None, signature: sig, varargs });
        let mut build = Builder::new(&mut source, block);
        let inst = ir::InstData {
            args: build.func().push_values(&[args[0], args[1]]),
            extra: ir::Extra::Call(info),
            ..ir::InstData::new(Opcode::CallIndirect)
        };
        let called = build.inst(inst, &[i32]);
        let got = source[called].first_result.expect("an integer comes back");
        Builder::new(&mut source, block).ret(&[got]);

        let machine = Machine::x86_64(&SYSV);
        let flags = Flags { sibling: true, ..Flags::default() };
        let out = compile(&mut source, &mut names, &machine, &Elsewhere::default(), flags)
            .expect("every instruction has a rule");
        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.jmp_reg $rax"), "{text}");
        assert!(!text.contains("x64.call"), "{text}");
        assert!(!text.contains("x64.ret"), "{text}");
    }

    /// Eight arguments are two more than there are registers for, so two go in the argument area
    /// at the bottom of this frame, and the call has to be made while the frame is still there.
    #[test]
    fn a_call_that_needs_the_argument_area_stays_a_call() {
        let text = tail(8, true);
        assert!(text.contains("x64.call"), "{text}");
        assert!(!text.contains("x64.jmp_away"), "{text}");
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

        // One of these comes back on the x87 stack and the other in a register, and the only pair
        // that stack holds is two `long double` halves of one complex value. So this is refused
        // rather than lowered, and it is the convention that refuses it rather than anything about
        // the instructions.
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

    /// A `_Complex long double` goes back on the x87 stack as two values, the real half on top.
    ///
    /// Each half arrives in memory like any `long double`, and the return loads the imaginary half
    /// first so that the real one is in `st(0)` above it, which is where the caller looks for each.
    /// A call to such a function takes both off again, the real half first, so the stack is empty
    /// by the time anything else touches it.
    #[test]
    fn a_complex_long_double_goes_back_on_the_x87_stack_as_a_pair() {
        let f80 = Type::float(rucc_ir::Float::F80);
        let (mut names, mut source, block, args) = blank(&[f80, f80]);
        Builder::new(&mut source, block).ret(&[args[1], args[0]]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("the pair is what the convention asks for");
        let text = mir::print_func(&out, &names, &REGS);
        // The second parameter is the real half here, so it is loaded last and ends up on top. There
        // is no frame, so the first parameter is right above the return address.
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let imaginary = lines.iter().position(|&line| line == "x64.fld_t [$rsp + 8]");
        let real = lines.iter().position(|&line| line == "x64.fld_t [$rsp + 24]");
        assert!(imaginary.is_some() && real == imaginary.map(|at| at + 1), "{text}");
        assert!(!text.contains("x64.ret_val"), "nothing comes back in a register: {text}");

        let (mut names, mut source, block, _) = blank(&[]);
        let sig = source.add_signature(Signature::new().with_returns(&[f80, f80]));
        let callee = names.intern("g");
        let call = Builder::new(&mut source, block).call(callee, sig, &[]);
        let halves: Vec<ir::Value> = source[call].results().collect();
        Builder::new(&mut source, block).ret(&[halves[1], halves[0]]);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("a call can take the pair off");
        let text = mir::print_func(&out, &names, &REGS);
        assert_eq!(text.matches("x64.fstp_t").count(), 2, "{text}");
        assert_eq!(text.matches("x64.fld_t").count(), 2, "{text}");
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

    /// `__builtin_popcount`, `__builtin_clz` and `__builtin_ctz` are one instruction each where the
    /// processor has it, at both widths, whether the command line or the function's own `target`
    /// attribute said so, and are never that instruction where it does not.
    #[test]
    fn a_bit_count_is_one_instruction_where_the_processor_has_it() {
        let built = |opcode: Opcode, width: u32, isa: Isa, target: Option<Isa>| {
            let ty = Type::int(width);
            let (mut names, mut source, block, args) = blank(&[ty]);
            source.target = target;
            let mut build = Builder::new(&mut source, block);
            let counted = build.unary(opcode, args[0], ty);
            build.ret(&[counted]);
            let machine = Machine::x86_64(&SYSV);
            let flags = Flags { isa, ..Flags::default() };
            let out = compile(&mut source, &mut names, &machine, &Elsewhere::default(), flags)
                .expect("a count is an instruction or arithmetic");
            mir::print_func(&out, &names, &REGS)
        };
        for (opcode, feature, inst) in [
            (Opcode::Ctpop, "popcnt", "x64.popcnt"),
            (Opcode::Ctlz, "lzcnt", "x64.lzcnt"),
            (Opcode::Cttz, "bmi", "x64.tzcnt"),
        ] {
            let has = Isa::of(&[feature]);
            for width in [32, 64] {
                let wanted = format!("{inst}_{width}");
                let text = built(opcode, width, has, None);
                assert!(text.contains(&wanted), "{wanted} from the command line: {text}");
                let text = built(opcode, width, Isa::NONE, Some(has));
                assert!(text.contains(&wanted), "{wanted} from the attribute: {text}");
                let text = built(opcode, width, Isa::NONE, None);
                assert!(!text.contains(inst), "no {inst} on a plain x86-64: {text}");
                let text = built(opcode, width, has, Some(Isa::NONE));
                assert!(!text.contains(inst), "nor in a function built for less: {text}");
            }
        }
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

    /// Two integer vectors read from memory, added lane by lane, mixed with a bitwise operation and
    /// written back, which is the whole of what an SSE2 loop body is made of.
    ///
    /// Each one is a single instruction on a whole vector register, and no lane is taken out into
    /// a general purpose register along the way, which is what would happen if the value had been
    /// put in the wrong file.
    #[test]
    fn an_integer_vector_is_added_and_mixed_a_whole_register_at_a_time() {
        let (mut names, mut source, block, args) = blank(&[Type::PTR, Type::PTR]);
        let mut build = Builder::new(&mut source, block);
        let info = rucc_ir::MemInfo {
            size: 16,
            align: 16,
            order: rucc_ir::MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let i32x4 = Type::vector(Type::int(32), 4);
        let i64x2 = Type::vector(Type::int(64), 2);
        let x = build.load(i32x4, args[0], info, ir::Flags::default());
        let y = build.load(i32x4, args[1], info, ir::Flags::default());
        let sum = build.binary(Opcode::Add, x, y, ir::Flags::default());
        let less = build.binary(Opcode::Sub, sum, y, ir::Flags::default());
        let mixed = build.binary(Opcode::Xor, less, x, ir::Flags::default());
        build.store(mixed, args[0], info, ir::Flags::default());
        let p = build.load(i64x2, args[1], info, ir::Flags::default());
        let q = build.binary(Opcode::Add, p, p, ir::Flags::default());
        let r = build.binary(Opcode::And, q, p, ir::Flags::default());
        let s = build.binary(Opcode::Or, r, q, ir::Flags::default());
        build.store(s, args[1], info, ir::Flags::default());
        build.ret(&[]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        for inst in [
            "x64.movdqu_rm",
            "x64.movdqu_mr",
            "x64.paddd_rr",
            "x64.psubd_rr",
            "x64.pxor_rr",
            "x64.paddq_rr",
            "x64.pand_rr",
            "x64.por_rr",
        ] {
            assert!(text.contains(inst), "{inst} in {text}");
        }
        assert!(text.contains("$xmm"), "{text}");
    }

    /// Lanes read, replaced and put in another order, which is a `pshufd` and the moves between the
    /// two files and nothing through memory but the loads and stores the program asked for.
    #[test]
    fn a_lane_is_moved_inside_the_vector_register() {
        let (mut names, mut source, block, args) = blank(&[Type::PTR, Type::PTR]);
        let mut build = Builder::new(&mut source, block);
        let info = rucc_ir::MemInfo {
            size: 16,
            align: 16,
            order: rucc_ir::MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let i32x4 = Type::vector(Type::int(32), 4);
        let i64x2 = Type::vector(Type::int(64), 2);
        let x = build.load(i32x4, args[0], info, ir::Flags::default());
        let s = build.shuffle(x, ir::Shuffle::new(&[1, 2, 3, 0]).expect("four lanes"));
        let e = build.extract_lane(s, 2);
        let t = build.insert_lane(s, e, 3);
        let u = build.insert_lane(t, e, 0);
        build.store(u, args[0], info, ir::Flags::default());
        let p = build.load(i64x2, args[1], info, ir::Flags::default());
        let q = build.extract_lane(p, 1);
        let r = build.insert_lane(p, q, 0);
        let w = build.insert_lane(r, q, 1);
        let b = build.unary(Opcode::Bitcast, w, i32x4);
        build.store(b, args[1], info, ir::Flags::default());
        build.ret(&[]);

        let machine = Machine::x86_64(&SYSV);
        let out =
            compile(&mut source, &mut names, &machine, &Elsewhere::default(), Flags::default())
                .expect("every lane has a lowering");

        let text = mir::print_func(&out, &names, &REGS);
        for inst in [
            "x64.pshufd_ri",
            "x64.movd_from_xmm",
            "x64.movd_to_xmm",
            "x64.movss_rr",
            "x64.movq_from_xmm",
            "x64.movq_to_xmm",
            "x64.movsd_rr",
            "x64.punpcklqdq_rr",
        ] {
            assert!(text.contains(inst), "{inst} in {text}");
        }
        // The merges write over their first source, and the allocator only keeps the two in one
        // register when it is told to.
        for line in text.lines() {
            if ["movss_rr", "movsd_rr", "punpcklqdq_rr"].iter().any(|it| line.contains(*it)) {
                assert!(line.contains("(reuse 1)"), "{line} in {text}");
            }
        }
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
        assert!(text.contains("x64.movups_rm"), "{text}");
        assert!(text.contains("x64.movups_mr"), "{text}");
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

    /// A function written in the other convention gets a machine of the same kind under that
    /// convention, and one the platform does not have is `None` rather than the native one.
    #[test]
    fn a_function_of_the_other_convention_gets_the_other_machine() {
        use rucc_target::Convention;
        let triple = |text: &str| text.parse::<rucc_target::Triple>().expect("a triple");
        let info = TargetInfo::new(triple("x86_64-unknown-linux-gnu"));
        let machine = Machine::for_target(&info).expect("x86-64 is the target this crate covers");
        let ms = machine.under(Convention::Ms).expect("Linux has the Windows convention");
        assert!(std::ptr::eq(ms.conv, &x86_64::MS_ON_SYSV));
        assert!(machine.under(Convention::Sysv).is_none());
        // The Windows convention owes `xmm6` to `xmm15` back, so the vector scratch registers are
        // two it does not owe.
        assert!(!ms.env.scratch(ms.conv.sse_class).iter().any(|&reg| ms.conv.preserves_sse(reg)));

        let info = TargetInfo::new(triple("x86_64-pc-windows-msvc"));
        let machine = Machine::for_target(&info).expect("x86-64 is the target this crate covers");
        let sysv = machine.under(Convention::Sysv).expect("Windows has the System V convention");
        assert!(std::ptr::eq(sysv.conv, &x86_64::SYSV_ON_WIN64));
        assert!(machine.under(Convention::Ms).is_none());

        let info = TargetInfo::new(triple("aarch64-unknown-linux-gnu"));
        let machine = Machine::for_target(&info).expect("aarch64 has a back end");
        assert!(machine.under(Convention::Ms).is_none());
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

        // The AArch64 machine, with its own selector, so nothing compiles x86-64 instructions for
        // an AArch64 program.
        let info = TargetInfo::new(triple("aarch64-unknown-linux-gnu"));
        let machine = Machine::for_target(&info).expect("aarch64 has a back end");
        assert!(std::ptr::eq(machine.conv, &aarch64::AAPCS64));
        assert!(std::ptr::eq(machine.selector, &select::aarch64::SELECTOR));

        // Not a target this crate has a back end for, and saying so is the whole point.
        let info = TargetInfo::new(triple("riscv64-unknown-linux-gnu"));
        assert!(Machine::for_target(&info).is_none());
    }
}
