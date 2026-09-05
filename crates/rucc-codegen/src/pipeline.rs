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
//! than from block frequency. No scheduling and no peepholes, so the redundant moves a coalescer
//! would take out are still in the output.

use rucc_base::Interner;
use rucc_ir as ir;
use rucc_mir as mir;
use rucc_regalloc::assign::Env;
use rucc_target::{Arch, BranchInsts, CallRegs, FrameInsts, PhysReg, RegFile, TargetInfo, x86_64};

use crate::coverage::Fired;
use crate::expand;
use crate::finish::finish;
use crate::frame::{Frame, Layout};
use crate::layout;
use crate::lower::{self, Unsupported};
use crate::split;
use crate::varargs;

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
/// into something, and those can want a register at the same instruction. Which two does not
/// matter. These are the last two the convention would reach for, which is what makes holding
/// them back cost the least.
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
        match target.triple.arch {
            Arch::X86_64 => Some(Self::x86_64(conv)),
            Arch::Aarch64 | Arch::Riscv64 => None,
        }
    }
}

/// What the command line says about a frame, as opposed to what the machine says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags {
    /// Whether every function keeps a frame pointer, which `-fno-omit-frame-pointer` asks for.
    pub frame_pointer: bool,
    /// Whether the red zone may be used, which `-mno-red-zone` and every kernel turns off.
    pub red_zone: bool,
}

impl Default for Flags {
    /// No frame pointer and the red zone allowed, which is what a convention that has one says
    /// when nobody on the command line has said otherwise.
    fn default() -> Self {
        Self { frame_pointer: false, red_zone: true }
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
/// # Errors
///
/// The first thing in it this cannot lower, which is what [`lower::func`] reports and is the only
/// pass here that can refuse a function. Everything after lowering works on machine instructions
/// that exist, so it either runs or it is a bug in this crate.
pub fn compile(
    source: &mut ir::Func,
    names: &mut Interner,
    machine: &Machine,
    flags: Flags,
) -> Result<mir::Func, Unsupported> {
    compile_recording(source, names, machine, flags, &mut Fired::new())
}

/// The same compilation, with the lowering rules it fired recorded into `fired`.
///
/// Two functions rather than one that takes an option, because a caller that does not want the
/// number should not have to say so. What `fired` is for is `-Zrule-coverage`, which is how the
/// harness in `tamnd/rucc-compat` turns coverage of the rule set into a number over a corpus.
///
/// It is merged into rather than replaced, so a caller can pass the same one for every function of
/// a module and every module of a command line and get the answer for all of them.
///
/// # Errors
///
/// The same as [`compile`]. A function that was refused contributes nothing, since a function that
/// did not compile is not evidence that anything covered it.
pub fn compile_recording(
    source: &mut ir::Func,
    names: &mut Interner,
    machine: &Machine,
    flags: Flags,
    fired: &mut Fired,
) -> Result<mir::Func, Unsupported> {
    expand::switches(source);
    expand::floats(source);
    expand::bulk(source, names, machine.conv.word);
    varargs::lists(source, machine.conv);
    let lowered = lower::func(source, names, machine.conv)?;
    fired.merge(&lowered.fired);
    let lower::Lowered { mut func, stack, .. } = lowered;
    let layout = Layout {
        frame_pointer: flags.frame_pointer,
        red_zone: flags.red_zone,
        ..stack.layout(Layout::new(machine.conv, machine.file))
    };

    // Before allocation, because an edge that carries values into a block arrived at more than
    // one way, out of a block that leaves more than one way, has nowhere to put the moves those
    // values turn into, and the allocator asserts rather than guessing.
    split::critical(&mut func);
    let allocation = rucc_regalloc::run(&mut func, &machine.env);

    // After allocation, because the largest area in most frames is the spill slots and nothing
    // knows how many of those there are until the allocator has finished running out of registers.
    let frame = Frame::of(&func, &allocation, &layout);
    finish(&mut func, &allocation, &frame, &stack, machine.conv, machine.insts, names);

    // Last, because everything before this finds the blocks a function returns from by looking
    // for the ones that go nowhere, and after this a block that falls through goes nowhere too.
    layout::blocks(&mut func, machine.branch, names);
    Ok(func)
}

#[cfg(test)]
mod tests {
    use rucc_ir::{Builder, Flags as IrFlags, Func, Opcode, Signature, Type};
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
            .expect("every instruction has a rule");

        // `int f(int a, int b) { return a + b; }` end to end. A leaf that spills nothing needs no
        // frame at all, so there is no prologue to see. The moves in the middle are all copies
        // between registers that could have been the same register, which is what a coalescer
        // would take out and there is not one yet, see issue 255.
        assert_eq!(
            mir::print_func(&out, &names, &REGS),
            "mfunc @f {\n\
             block0:\n    \
             $rdi($rdi) = x64.arg_val_32\n    \
             $rax = x64.mov_rr_64 $rdi\n    \
             $rsi($rsi) = x64.arg_val_32\n    \
             $rcx = x64.mov_rr_64 $rsi\n    \
             $rdx = x64.mov_rr_64 $rax\n    \
             $rdx(reuse 1) = x64.add_rr_32 $rax, $rcx\n    \
             $rax = x64.mov_rr_64 $rdx\n    \
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
        compile_recording(&mut source, &mut names, &machine, Flags::default(), &mut fired)
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
        compile_recording(&mut source, &mut names, &machine, Flags::default(), &mut fired)
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
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
        // into the join as well. What is left is one jump in the whole function.
        let text = mir::print_func(&out, &names, &REGS);
        assert_eq!(
            text,
            "mfunc @f {\n\
             block0:\n    \
             $rdi($rdi) = x64.arg_val_32\n    \
             $rax = x64.mov_rr_64 $rdi\n    \
             $rsi($rsi) = x64.arg_val_32\n    \
             $rcx = x64.mov_rr_64 $rsi\n    \
             $rdx = x64.cmp_set_l_32 $rax, $rcx\n    \
             x64.test_rr_8 $rdx\n    \
             x64.jcc_e block2, block1\n\
             \nblock1:\n    \
             $rdx = x64.mov_rr_64 $rax\n    \
             x64.jmp block3\n\
             \nblock2:\n    \
             $rdx = x64.mov_rr_64 $rcx, block3\n\
             \nblock3:\n    \
             $rax = x64.mov_rr_64 $rdx\n    \
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
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
             $rax = x64.mov_rr_64 $rdi\n    \
             $rsi($rsi) = x64.arg_val_32\n    \
             $rcx = x64.mov_rr_64 $rsi\n    \
             $rsi = x64.mov_rr_64 $rcx\n    \
             $rcx = x64.mov_rr_64 $rax, block1\n\
             \nblock1:\n    \
             $rax = x64.mov_ri_32 0\n    \
             $rax = x64.cmp_set_ne_32 $rsi, $rax\n    \
             x64.test_rr_8 $rax\n    \
             x64.jcc_e block3, block2\n\
             \nblock2:\n    \
             $rax = x64.mov_rr_64 $rcx\n    \
             $rdx($rdx), early $rax($rax) = x64.idiv_rem_32 $rax($rax), $rsi\n    \
             $rcx = x64.mov_rr_64 $rdx\n    \
             $rdi = x64.mov_rr_64 $rax\n    \
             $r10 = x64.mov_rr_64 $rsi\n    \
             $rsi = x64.mov_rr_64 $rcx\n    \
             $rcx = x64.mov_rr_64 $r10\n    \
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
            .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        let read = rucc_mir::parse(&text, &mut names, &REGS).expect("what the printer wrote");
        assert_eq!(mir::print(&read, &names, &REGS), text);
    }

    #[test]
    fn a_function_this_cannot_lower_is_reported_rather_than_compiled() {
        let f80 = Type::float(rucc_ir::Float::F80);
        let (mut names, mut source, block, args) = blank(&[f80]);
        Builder::new(&mut source, block).ret(&[args[0]]);

        let machine = Machine::x86_64(&SYSV);
        let failed = compile(&mut source, &mut names, &machine, Flags::default())
            .expect_err("a long double arrives on the x87 stack");
        assert_eq!(failed.to_string(), "parameter 0 is on the x87 stack");
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
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
        let info =
            rucc_ir::MemInfo { size: 8, align: 8, order: rucc_ir::MemOrder::NotAtomic, tbaa: None };
        let read = build.load(f64, args[0], info, ir::Flags::default());
        let sum = build.binary(Opcode::FAdd, read, args[1], ir::Flags::default());
        build.store(sum, args[0], info, ir::Flags::default());
        build.ret(&[sum]);

        let machine = Machine::x86_64(&SYSV);
        let out = compile(&mut source, &mut names, &machine, Flags::default())
            .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("x64.movsd_rm"), "{text}");
        assert!(text.contains("x64.movsd_mr"), "{text}");
        // Not the aligned whole register move, which is what a spill uses and is the one
        // instruction here that would read and write more than the program asked for.
        assert!(!text.contains("x64.movaps_rm"), "{text}");
        assert!(!text.contains("x64.movaps_mr"), "{text}");
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
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
        let out = compile(&mut source, &mut names, &machine, Flags::default())
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
        let flags = Flags { frame_pointer: true, red_zone: true };
        let out = compile(&mut source, &mut names, &machine, flags)
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
