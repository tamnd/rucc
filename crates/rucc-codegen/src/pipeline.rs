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
use rucc_ir as ir;
use rucc_mir as mir;
use rucc_regalloc::assign::Env;
use rucc_target::{BranchInsts, CallRegs, FrameInsts, PhysReg, RegFile, TargetInfo, x86_64};
use rucc_tuple::Arch;

use crate::coverage::Fired;
use crate::elsewhere::Elsewhere;
use crate::expand;
use crate::finish::{Convention, Padding, Probing, Protect, Tracing, finish};
use crate::fold;
use crate::frame::{Frame, Layout};
use crate::layout;
use crate::lower::{self, Unsupported};
use crate::pressure::{Cost, Pressure};
use crate::retry;
use crate::split;
use crate::switch;
use crate::varargs;
use crate::wide;
use crate::widths;

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
    /// What the allocator may hand out, and what it holds back.
    pub env: Env,
}

/// The scratch registers held back from the allocator on x86-64.
///
/// Two, because a move on an edge may have to break a cycle and a spilled value has to be read
/// into something, and those can want a register at the same instruction. Two is also what the
/// instruction wanting most wants, which is one that reads two spilled values and writes a third,
/// and `rewrite` says why the answer goes back into a register an operand arrived in rather than
/// asking for a third.
///
/// It is not two because two was enough to start with and nobody looked again. There is no third
/// to hold back. A scratch register has to be one the convention passes nothing in, since the
/// rewriter puts moves in wherever it likes, and one the callee does not owe back, since the
/// rewriter runs after the prologue has been decided and cannot ask for a register to be saved.
/// On SysV that is `r10` and `r11` and nothing else, so if the rewriter ever does want a third the
/// answer is not to take one here.
const SCRATCH: [PhysReg; 2] = [x86_64::R10, x86_64::R11];

/// How many of each class are held back.
const SCRATCH_COUNT: usize = SCRATCH.len();

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
        let free: Vec<PhysReg> =
            conv.sse_order.iter().copied().filter(|&reg| !conv.preserves_sse(reg)).collect();
        let at = free.len().saturating_sub(SCRATCH_COUNT);
        let sse_scratch: Vec<PhysReg> = free[at..].to_vec();
        let sse_order: Vec<PhysReg> =
            conv.sse_order.iter().copied().filter(|reg| !sse_scratch.contains(reg)).collect();
        Self {
            conv,
            file: x86_64::REGS,
            insts: &x86_64::FRAME,
            branch: &x86_64::BRANCH,
            env: Env::new().with(x86_64::GPR, &order, &SCRATCH).with(
                x86_64::XMM,
                &sse_order,
                &sse_scratch,
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

/// What the command line says about a frame, as opposed to what the machine says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    /// Whether every function keeps a frame pointer, which `-fno-omit-frame-pointer` asks for.
    pub frame_pointer: bool,
    /// Whether the red zone may be used, which `-mno-red-zone` and every kernel turns off.
    pub red_zone: bool,
    /// Whether a frame is taken a page at a time, which `-fstack-clash-protection` asks for.
    pub stack_clash: bool,
    /// Whether every function opens with a landing pad, which `-fcf-protection=branch` asks for.
    pub landing: bool,
    /// Whether every function calls a profiler on the way in, which `-pg` asks for.
    pub profile: Profile,
    /// How much room every function opens with for a patcher, which
    /// `-fpatchable-function-entry=` asks for. See [`Room`].
    pub patch: Room,
}

impl Default for Flags {
    /// No frame pointer, the red zone allowed, the frame taken in one subtraction, no landing pad,
    /// no profiling and no room for a patcher, which is what a convention that has a red zone says
    /// when nobody on the command line has said otherwise.
    fn default() -> Self {
        Self {
            frame_pointer: false,
            red_zone: true,
            stack_clash: false,
            landing: false,
            profile: Profile::No,
            patch: Room::default(),
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
/// The first thing in it this cannot lower, which is what [`lower::func`] reports and is the only
/// pass here that can refuse a function. Everything after lowering works on machine instructions
/// that exist, so it either runs or it is a bug in this crate.
pub fn compile(
    source: &mut ir::Func,
    names: &mut Interner,
    machine: &Machine,
    elsewhere: &Elsewhere,
    flags: Flags,
) -> Result<mir::Func, Unsupported> {
    compile_recording(
        source,
        names,
        machine,
        elsewhere,
        flags,
        &mut Fired::new(),
        &mut Pressure::new(),
    )
}

/// The same compilation, with what it did along the way recorded.
///
/// Two functions rather than one that takes options, because a caller that does not want the
/// numbers should not have to say so. What `fired` is for is `-Zrule-coverage`, which is how the
/// harness in `tamnd/rucc-compat` turns coverage of the rule set into a number over a corpus. What
/// `pressure` is for is `-Zregister-pressure`, which is how much of the frame the allocator had to
/// use and is the metric `spec/safe-memory/13-performance.md` section 13.1 asks for.
///
/// Both are added to rather than replaced, so a caller can pass the same pair for every function of
/// a module and every module of a command line and get the answer for all of them.
///
/// # Errors
///
/// The same as [`compile`]. A function that was refused contributes nothing to either, since a
/// function that did not compile is not evidence about what a rule set or a frame would have done.
pub fn compile_recording(
    source: &mut ir::Func,
    names: &mut Interner,
    machine: &Machine,
    elsewhere: &Elsewhere,
    flags: Flags,
    fired: &mut Fired,
    pressure: &mut Pressure,
) -> Result<mir::Func, Unsupported> {
    switch::switches(source);
    // Beside the switches rather than down with the rest of the rewriting, because both of them
    // make blocks and nothing in `expand` may. Before the orderings as well, since the head of the
    // loop it builds reads with an `atomic_load` and the pass below is what turns that into the
    // plain load this machine does anyway.
    retry::loops(source);
    // Before the width legalisation and everything after it, because what an ordered access
    // becomes here is a plain one and every pass below is written about a plain one by name.
    expand::orderings(source, machine.conv.word);
    // Ahead of the width legalisation and not part of it, because the two go in opposite
    // directions: an integer of forty bits becomes one of sixty four down there, and one of a
    // hundred and twenty eight becomes two of sixty four here. Doing this first means a function
    // holding both is one the pass below still works on, since by the time it runs the only widths
    // left are ones it has an answer for.
    wide::halves(source, machine.conv);
    // Before everything, because every pass after it is written about widths the machine has and
    // an integer of forty bits is not one of them.
    widths::integers(source);
    expand::bytes(source);
    expand::counts(source);
    expand::overflows(source);
    expand::floats(source);
    expand::bulk(source, names, machine.conv.word);
    expand::rounds(source, machine.conv.stack_align);
    varargs::lists(source, machine.conv);
    let lowered = lower::func(source, names, machine.conv, elsewhere)?;
    fired.merge(&lowered.fired);
    let lower::Lowered { mut func, mut stack, .. } = lowered;
    // Two things a frame that grows while it runs cannot be asked for at the same time, both of
    // them refusals rather than wrong code.
    if let Some(inst) = stack.grown_at {
        // What `-fstack-clash-protection` buys is that no frame ever steps over a guard page
        // without touching it, and a frame that grows while it runs steps by however much the
        // declaration asked for. The prologue's own pages are touched below, and the ones a
        // variable length array takes are not, so a function with both is refused rather than
        // compiled to something that keeps the flag's name and not its promise.
        if flags.stack_clash {
            return Err(Unsupported::Dynamic { inst, growing: lower::Growing::Probed });
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
    fold::addresses(&mut func, machine.insts, names, &mut pending);

    }

    // After selection, because the address instruction and the one that reads it are both machine
    // instructions only once selection has written them, and before allocation, because what makes
    // the pair safe to put together is that a virtual register is written once. The addresses into
    // the frame and into the caller's argument area go through it like anything else, and the two
    // lists `finish` reads are rewritten as they do, so an address that ends up inside its reader
    // is still an address the frame layout knows to write an offset into.
    let mut pending =
        fold::Pending {
        addresses: &mut stack.addresses,
        arguments: &mut stack.arguments,
        dynamic: &mut stack.dynamic,
    };
    fold::addresses(&mut func, machine.insts, names, &mut pending);

    // Whether this function carries a canary is the front end's answer, because what
    // `-fstack-protector` asks about is the kind of local a function has and the types are gone by
    // here. What the machine does about it is this crate's answer, and a target with nowhere to
    // keep the word a canary is copied from does nothing, which is what the driver refuses a
    // command line over before any of this runs.
    let protect = source.attrs.set.contains(ir::AttrSet::STACK_PROTECT);
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
        // function that calls it is given one whether or not anything else asked.
        frame_pointer: flags.frame_pointer || profile == Profile::Late,
        red_zone: flags.red_zone,
        protect: guard.is_some(),
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

    // Before allocation, because an edge that carries values into a block arrived at more than
    // one way, out of a block that leaves more than one way, has nowhere to put the moves those
    // values turn into, and the allocator asserts rather than guessing.
    split::critical(&mut func);
    let called = names.resolve(func.name).to_owned();
    let allocation = rucc_regalloc::run(&mut func, &machine.env, &called);
    pressure.record(&called, Cost::of(&allocation));

    // After allocation, because the largest area in most frames is the spill slots and nothing
    // knows how many of those there are until the allocator has finished running out of registers.
    let frame = Frame::of(&func, &allocation, &layout);
    let scratch = machine.env.scratch(machine.conv.int_class);
    let protect = guard.map(|guard| Protect {
        guard,
        branch: machine.branch,
        scratch: [scratch[0], scratch[1]],
    });
    // A target with no instruction that touches a page without changing it does nothing about the
    // flag, which is the same answer the protector gives on a target with nowhere to keep its word.
    // Every target this crate has a back end for has one.
    let probe = flags
        .stack_clash
        .then_some(machine.insts.probe.as_ref())
        .flatten()
        .map(|probe| Probing { probe, branch: machine.branch, scratch: [scratch[0], scratch[1]] });
    // The same answer for a target with nothing that marks an address as one an indirect branch
    // may arrive at, and the driver refuses the command line for the same reason it refuses the
    // other two before any of this runs.
    let landing = flags.landing.then_some(machine.insts.landing).flatten();
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
    finish(&mut func, &allocation, &frame, &stack, convention, names);

    // Last, because everything before this finds the blocks a function returns from by looking
    // for the ones that go nowhere, and after this a block that falls through goes nowhere too.
    layout::blocks(&mut func, machine.branch, names, &fusable);
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
            &mut fired,
            &mut Pressure::new(),
        )
        .expect("every instruction has a rule");
        let one = fired.count();
        assert!(one > 0, "an add and a return went through the table and nothing was recorded");

        let listing = fired.listing(&crate::select::x86_64::TABLE);
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
            &mut fired,
            &mut Pressure::new(),
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
        // one point and that point is in front of where the remainder is written. The copy that
        // takes the quotient nowhere then landed on top of the remainder.
        assert_eq!(
            mir::print_func(&out, &names, &REGS),
            "mfunc @f {\n\
             block0:\n    \
             $rdi($rdi) = x64.arg_val_32\n    \
             $rsi($rsi) = x64.arg_val_32\n    \
             $rcx = x64.mov_rr_64 $rdi, block1\n\
             \nblock1:\n    \
             x64.cmp_ri_32 $rsi, 0\n    \
             x64.jcc_e block3, block2\n\
             \nblock2:\n    \
             $rax = x64.mov_rr_64 $rcx\n    \
             $rdx($rdx), early $rax($rax) = x64.idiv_rem_32 $rax($rax), $rsi\n    \
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
