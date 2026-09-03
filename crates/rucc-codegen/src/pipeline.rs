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

use crate::finish::finish;
use crate::frame::{Frame, Layout};
use crate::layout;
use crate::lower::{self, Unsupported};
use crate::split;

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

impl Machine {
    /// The x86-64 machine under that convention.
    ///
    /// Only the general purpose registers are offered, because every rule in the set is about an
    /// integer and no value the selector produces is in any other class. A call still destroys the
    /// vector registers and still says so, and that costs nothing while nothing is in one.
    #[must_use]
    pub fn x86_64(conv: &'static CallRegs) -> Self {
        let order: Vec<PhysReg> =
            conv.int_order.iter().copied().filter(|reg| !SCRATCH.contains(reg)).collect();
        Self {
            conv,
            file: x86_64::REGS,
            insts: &x86_64::FRAME,
            branch: &x86_64::BRANCH,
            env: Env::new().with(x86_64::GPR, &order, &SCRATCH),
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
/// # Errors
///
/// The first thing in it this cannot lower, which is what [`lower::func`] reports and is the only
/// pass here that can refuse a function. Everything after lowering works on machine instructions
/// that exist, so it either runs or it is a bug in this crate.
pub fn compile(
    source: &ir::Func,
    names: &mut Interner,
    machine: &Machine,
    flags: Flags,
) -> Result<mir::Func, Unsupported> {
    let lowered = lower::func(source, names, machine.conv)?;
    let stack = Layout {
        frame_pointer: flags.frame_pointer,
        red_zone: flags.red_zone,
        ..lowered.layout(Layout::new(machine.conv, machine.file))
    };
    let mut func = lowered.func;

    // Before allocation, because an edge that carries values into a block arrived at more than
    // one way, out of a block that leaves more than one way, has nowhere to put the moves those
    // values turn into, and the allocator asserts rather than guessing.
    split::critical(&mut func);
    let allocation = rucc_regalloc::run(&mut func, &machine.env);

    // After allocation, because the largest area in most frames is the spill slots and nothing
    // knows how many of those there are until the allocator has finished running out of registers.
    let frame = Frame::of(&func, &allocation, &stack);
    finish(&mut func, &allocation, &frame, machine.conv, machine.insts, names);

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
        let out = compile(&source, &mut names, &machine, Flags::default())
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
        let out = compile(&source, &mut names, &machine, Flags::default())
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
        let out = compile(&source, &mut names, &machine, Flags::default())
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
        let out = compile(&source, &mut names, &machine, Flags::default())
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
        let out = compile(&source, &mut names, &machine, Flags::default())
            .expect("every instruction has a rule");

        let text = mir::print_func(&out, &names, &REGS);
        let read = rucc_mir::parse(&text, &mut names, &REGS).expect("what the printer wrote");
        assert_eq!(mir::print(&read, &names, &REGS), text);
    }

    #[test]
    fn a_function_this_cannot_lower_is_reported_rather_than_compiled() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (mut names, mut source, block, args) = blank(&[f64]);
        Builder::new(&mut source, block).ret(&[args[0]]);

        let machine = Machine::x86_64(&SYSV);
        let failed = compile(&source, &mut names, &machine, Flags::default())
            .expect_err("a double arrives in a vector register");
        assert_eq!(failed.to_string(), "parameter 0 is in a vector register");
    }

    #[test]
    fn the_flags_reach_the_frame() {
        let i32 = Type::int(32);
        let (mut names, mut source, block, args) = blank(&[i32]);
        Builder::new(&mut source, block).ret(&[args[0]]);

        let machine = Machine::x86_64(&SYSV);
        let flags = Flags { frame_pointer: true, red_zone: true };
        let out =
            compile(&source, &mut names, &machine, flags).expect("every instruction has a rule");

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
