//! Both register allocators and the allocation checker.
//!
//! Design: `spec/10-backend.md`. Layer rank 11, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! Liveness is here, which is the question both allocators ask first: [`order`] lays a function out
//! in the line the encoder will emit it in, and [`live`] says where in that line each value is
//! wanted. So is [`moves`], which puts the moves an edge turns into in an order they can be made in
//! one at a time. The single pass allocator's decision is in [`assign`]: where every value of a
//! function goes, in one linear scan, which is what `-O0` asks for. The rewrite that makes that
//! decision true in the function is in [`rewrite`], with what each instruction needs around it for
//! the machine to accept the places worked out in [`legalize`], and [`run`] is the two of them
//! together, which is the whole of the `-O0` allocator. [`check`] reads an assignment back and says
//! whether it is one the machine can run, which [`run`] asserts on in debug and CI builds and which
//! the backtracking allocator is held to the same way. [`trace`] asks the other half of the
//! question, which is whether the rewrite wrote that decision down without losing a value on the
//! way: it follows every value from the instruction that wrote it to the instructions that read it,
//! through the moves, and [`run`] asserts on it in the same builds.
//!
//! The allocator the optimizer uses is in [`backtrack`]. [`pressure`] counts, at every point, the
//! values that want a register against the registers there are, and [`spill`] reads that to pick
//! which values go to memory before [`backtrack`] places the rest.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-regalloc/0.24.4")]

pub mod assign;
pub mod backtrack;
pub mod check;
pub mod legalize;
pub mod live;
pub mod moves;
pub mod order;
pub mod pressure;
pub mod rewrite;
pub mod spill;
pub mod trace;

/// What allocating a function produced.
///
/// The moves are handed back rather than written into the function because a move is an
/// instruction and an instruction belongs to a target, which `spec/10-backend.md` section 10.8
/// says this crate holds nothing of. The consumer turns each one into whatever its target moves a
/// register with.
#[derive(Debug, Clone)]
pub struct Allocation {
    /// Where every value of the function went, which is what the frame layout reads.
    pub assignment: assign::Assignment,
    /// The moves the places do not already make true, in the order they have to be made in.
    pub edits: Vec<rewrite::Edit>,
    /// The line the function was laid out in while it was allocated, which the liveness below is
    /// counted along.
    pub order: order::Order,
    /// Where every value was live.
    ///
    /// Handed back rather than dropped because the stack slot allocator shares one run of bytes
    /// between two things that are never both wanted, and the only liveness that knows where a
    /// spilled value is wanted is the one the spilling was decided from. Working a second one out
    /// afterwards would cost a pass and would be free to disagree with this one.
    /// `spec/optimizer/36-lowering-and-isel.md` section 36.7 asks for the one answer.
    pub live: live::Live,
}

/// Allocates registers for a function the way `-O0` asks for, rewriting it as it goes.
///
/// This is the shape `spec/10-backend.md` section 10.4 gives an allocator: a function and the
/// registers it may use in, an assignment and the moves that make it true out. The backtracking
/// allocator will answer the same question the same way.
///
/// # Panics
///
/// Panics on a function the caller was told not to hand it, which is one with a critical edge or
/// one whose entry block has parameters. See [`rewrite::rewrite`].
///
/// It also panics on an assignment [`check`] finds a problem with, which is a bug in this crate or
/// in whatever produced the function rather than anything the caller did. `spec/10-backend.md`
/// section 10.4 asks for that check in debug and CI builds, and it runs before the rewrite because
/// the assignment is the decision and the rewrite only writes it down.
///
/// It panics on a rewrite [`trace`] finds a value missing from as well. That one runs afterwards,
/// since a transcription can only be read once it has been made, and it is the check
/// `spec/optimizer/39-register-allocation.md` section 39.6 asks for.
///
/// `verify` is what turns both of those on in a build that has assertions compiled out. A debug
/// build runs them whatever it says, since that is where a broken pass should be caught, and a
/// release build runs them when the caller asks, which is what `-Zverify-each` is for and what
/// section 10.4 means by a CI build. It is a parameter rather than a `cfg!` because the thing
/// worth catching is a pass that writes a function nothing defines a register in, the gate
/// compiles release, and a check the gate never runs is a check that finds the bug after the merge
/// rather than on the pull request. tamnd/rucc#1411.
///
/// `called` is what to call the function in that message. It is passed in rather than read off the
/// function because the name there is a symbol and resolving one wants the interner, which this
/// crate has no reason to be handed otherwise. Without it the message is a pair of register numbers
/// and nothing that says where, and finding the function it was about in a file the size of the
/// SQLite amalgamation means bisecting by hand.
pub fn run(func: &mut rucc_mir::Func, env: &assign::Env, called: &str, verify: bool) -> Allocation {
    run_with(func, env, called, verify, Allocator::Single)
}

/// Which of the two allocators decides where the values go.
///
/// The rewrite, the checks and the moves are the same for both, since each hands back an
/// [`assign::Assignment`] and nothing after the decision asks which one made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Allocator {
    /// The single pass of [`assign`], which is what `-O0` asks for.
    #[default]
    Single,
    /// The allocator of [`backtrack`], which weighs what each value costs to keep in memory.
    Backtracking,
}

/// Allocates registers for a function with the allocator asked for, rewriting it as it goes.
///
/// # Panics
///
/// As [`run`].
pub fn run_with(
    func: &mut rucc_mir::Func,
    env: &assign::Env,
    called: &str,
    verify: bool,
    allocator: Allocator,
) -> Allocation {
    let order = order::Order::of(func);
    let live = live::Live::of(func, &order);
    let assignment = decide(func, &order, &live, env, allocator);
    write(func, assignment, env, order, live, called, verify)
}

/// Allocates registers with `wide` where that needs none of the scratch registers it leaves out,
/// and with `env` where it does.
///
/// `wide` is `env` with some of what `env` holds back handed out instead. Holding a register back
/// costs every function that never reads a value off the stack, which is most of them, and on
/// x86-64 the two held back are two of the nine a call may destroy. A function short of those
/// reaches for one the callee has to save, which is a push and a pop for a register the scratch
/// pair could have been. So the allocator is asked with the registers handed out first, and what it
/// decides is kept if [`rewrite::fits`] says the rewrite will never reach for a scratch register
/// the class does not have. When it would, the function is allocated again the way it always was.
///
/// The answer is the same shape either way, and nothing after it has to ask which one it was. A
/// caller that writes code of its own into a scratch register after allocation has to do it where
/// no value is live, which a prologue and a return are.
///
/// # Panics
///
/// As [`run`].
pub fn run_either(
    func: &mut rucc_mir::Func,
    wide: &assign::Env,
    env: &assign::Env,
    called: &str,
    verify: bool,
    allocator: Allocator,
) -> Allocation {
    let order = order::Order::of(func);
    let live = live::Live::of(func, &order);
    let tried = decide(func, &order, &live, wide, allocator);
    if rewrite::fits(func, &tried, wide) {
        return write(func, tried, wide, order, live, called, verify);
    }
    let assignment = decide(func, &order, &live, env, allocator);
    write(func, assignment, env, order, live, called, verify)
}

/// Where every value goes, with the allocator asked for.
fn decide(
    func: &rucc_mir::Func,
    order: &order::Order,
    live: &live::Live,
    env: &assign::Env,
    allocator: Allocator,
) -> assign::Assignment {
    match allocator {
        Allocator::Single => assign::assign(func, order, live, env),
        Allocator::Backtracking => backtrack::assign(func, order, live, env),
    }
}

/// Makes a decision true in the function, checking it on the way in and the rewrite on the way out
/// in a build that asks for that.
fn write(
    func: &mut rucc_mir::Func,
    mut assignment: assign::Assignment,
    env: &assign::Env,
    order: order::Order,
    live: live::Live,
    called: &str,
    verify: bool,
) -> Allocation {
    let checking = verify || cfg!(debug_assertions);
    // An answer that went over the second source of an instruction that reads its sources either
    // way round. Swapping them makes it an ordinary reuse of the first, so the checker, the trace
    // and the rewrite read it as one. Liveness does not care which way round two uses are.
    for &inst in assignment.commuted() {
        let list = func[inst].operands;
        func[list].swap(1, 2);
    }
    if checking {
        let problems = check::check(func, &order, &live, &assignment);
        assert!(problems.is_empty(), "in '{called}': {}", check::report(&problems));
    }
    // What the rewrite is about to lose, taken while it is still there. Only in a build that is
    // going to read it, since the snapshot is a copy of every operand list in the function.
    let shape = checking.then(|| trace::shape(func));
    let edits = rewrite::rewrite(func, &mut assignment, env);
    if let Some(shape) = shape {
        let faults = trace::trace(func, &shape, &assignment, &edits);
        assert!(faults.is_empty(), "in '{called}': {}", trace::report(&faults));
    }
    Allocation { assignment, edits, order, live }
}

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Func, Opcode, Operand, Reg, Weight};
    use rucc_target::x86_64::{GPR, RAX, RCX, RDX, SYSV};

    use super::*;

    #[test]
    fn milestone_is_recorded() {
        assert!(MILESTONE.starts_with('M'));
    }

    #[test]
    fn allocating_a_function_places_every_value_and_hands_back_the_moves_it_needs() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        let third = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).def(third, GPR).finish();
        func.build(block, opcode).uses(first, GPR).uses(second, GPR).uses(third, GPR).finish();

        // Two registers to hand out and three values that are all wanted at once, so one of them
        // goes to the stack and the instruction that reads it gets a reload. This is also where
        // the checker runs, since a debug build asserts on what it says.
        let env = assign::Env::new().with(GPR, &SYSV.int_order[..2], &SYSV.int_order[2..5]);
        let allocation = run(&mut func, &env, "test", true);

        assert_eq!(allocation.assignment.spilled(), 1);
        assert_eq!(allocation.edits.len(), 2);
    }

    /// Three values wanted at once, which is the function the test above spills one of.
    fn three_at_once(names: &mut Interner) -> (Func, [Reg; 3]) {
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let values = [(); 3].map(|()| func.new_vreg(GPR));
        for value in values {
            func.build(block, opcode).def(value, GPR).finish();
        }
        func.build(block, opcode)
            .uses(values[0], GPR)
            .uses(values[1], GPR)
            .uses(values[2], GPR)
            .finish();
        (func, values)
    }

    #[test]
    fn a_function_that_needs_no_scratch_register_is_given_the_scratch_registers() {
        let mut names = Interner::new();
        let (mut func, values) = three_at_once(&mut names);

        // Two to hand out and one held back, or three to hand out and none held back. Nothing
        // spills with three, so the third is the one held back and no move is wanted anywhere.
        let wide = assign::Env::new().with(GPR, &SYSV.int_order[..3], &[]);
        let env = assign::Env::new().with(GPR, &SYSV.int_order[..2], &SYSV.int_order[2..4]);
        let allocation = run_either(&mut func, &wide, &env, "test", true, Allocator::Single);

        assert_eq!(allocation.assignment.spilled(), 0);
        assert!(allocation.edits.is_empty());
        let mut places: Vec<_> =
            values.iter().filter_map(|&value| allocation.assignment.place(value)).collect();
        places.sort_by_key(|place| match place {
            assign::Place::Reg(reg) => reg.number(),
            assign::Place::Slot(_) => u8::MAX,
        });
        let regs = [RAX, RCX, RDX].map(assign::Place::Reg);
        assert_eq!(places, regs);
    }

    #[test]
    fn a_function_that_spills_is_allocated_again_with_the_scratch_registers_held_back() {
        let mut names = Interner::new();
        let (mut func, _) = three_at_once(&mut names);

        // Two to hand out either way. The wide answer spills a value it has nothing to read back
        // into, so the function is allocated again with the scratch registers held back.
        let wide = assign::Env::new().with(GPR, &SYSV.int_order[..2], &[]);
        let env = assign::Env::new().with(GPR, &SYSV.int_order[..2], &SYSV.int_order[2..4]);
        let allocation = run_either(&mut func, &wide, &env, "test", true, Allocator::Single);

        assert_eq!(allocation.assignment.spilled(), 1);
        assert_eq!(allocation.edits.len(), 2);
    }

    #[test]
    fn two_values_swapping_on_an_edge_are_allocated_with_the_scratch_registers_held_back() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let body = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(head, opcode).def(first, GPR).finish();
        func.build(head, opcode).def(second, GPR).finish();
        let left = func.append_param(body, GPR);
        let right = func.append_param(body, GPR);
        *func.succs_mut(head) = vec![BlockCall::with(body, vec![first, second])];
        func.build(body, opcode).uses(left, GPR).uses(right, GPR).finish();
        *func.succs_mut(body) = vec![BlockCall::with(body, vec![right, left])];

        // Nothing spills, but the loop hands the two values back the other way round, which takes a
        // third register for one of them while the other moves.
        let wide = assign::Env::new().with(GPR, &SYSV.int_order[..2], &[]);
        let env = assign::Env::new().with(GPR, &SYSV.int_order[..2], &SYSV.int_order[2..4]);
        let allocation = run_either(&mut func, &wide, &env, "test", true, Allocator::Single);

        assert_eq!(allocation.assignment.spilled(), 0);
        let through = assign::Place::Reg(RDX);
        assert!(allocation.edits.iter().any(|edit| edit.mov.to == through), "{allocation:?}");
    }

    #[test]
    fn a_value_carried_round_a_loop_is_allocated_with_the_scratch_registers_given_out() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let body = func.create_block();
        let first = func.new_vreg(GPR);
        func.build(head, opcode).def(first, GPR).finish();
        let carried = func.append_param(body, GPR);
        *func.succs_mut(head) = vec![BlockCall::with(body, vec![first])];
        let next = func.new_vreg(GPR);
        func.build(body, opcode).def(next, GPR).uses(carried, GPR).finish();
        *func.succs_mut(body) = vec![BlockCall::with(body, vec![next])];

        // One value on each edge goes round in no cycle, so the answer with nothing held back is
        // the one written, and writing the edge has no scratch register to ask for.
        let wide = assign::Env::new().with(GPR, &SYSV.int_order[..2], &[]);
        let env = assign::Env::new().with(GPR, &SYSV.int_order[..1], &SYSV.int_order[1..3]);
        let allocation = run_either(&mut func, &wide, &env, "test", true, Allocator::Single);

        assert_eq!(allocation.assignment.spilled(), 0);
        for value in [first, carried, next] {
            let place = allocation.assignment.place(value);
            assert!(matches!(place, Some(assign::Place::Reg(_))), "{allocation:?}");
        }
    }

    #[test]
    fn a_value_a_loop_reads_is_put_away_around_the_call_the_loop_seldom_makes() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let [entry, head, cold, skip, latch, back, out] = [(); 7].map(|()| func.create_block());
        let step = func.new_vreg(GPR);
        func.build(entry, opcode).def(step, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(head)];
        func.build(head, opcode).uses(step, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(cold), BlockCall::to(skip)];
        // A call, as far as the allocator can tell: both registers it hands out are destroyed.
        let call = func
            .build(cold, opcode)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .operand(Operand::write(Reg::physical(RCX), GPR))
            .finish();
        *func.succs_mut(cold) = vec![BlockCall::to(latch)];
        *func.succs_mut(skip) = vec![BlockCall::to(latch)];
        func.build(latch, opcode).uses(step, GPR).finish();
        *func.succs_mut(latch) = vec![BlockCall::to(back), BlockCall::to(out)];
        *func.succs_mut(back) = vec![BlockCall::to(head)];
        func.build(out, opcode).uses(step, GPR).finish();
        for (block, often) in [(head, 100), (skip, 99), (latch, 100), (back, 99)] {
            func.set_weight(block, Weight::parts(often * Weight::SCALE));
        }

        // Two registers and the call destroys both, so the only other answer is the stack and a
        // load in every turn. The trace runs here as well, and it is what says the value read back
        // behind the call is the one put away in front of it.
        let env = assign::Env::new().with(GPR, &SYSV.int_order[..2], &SYSV.int_order[2..5]);
        let allocation = run_with(&mut func, &env, "test", true, Allocator::Backtracking);

        let assignment = &allocation.assignment;
        assert_eq!(assignment.spilled(), 0);
        let Some(assign::Place::Reg(at)) = assignment.place(step) else {
            panic!("the value went to the stack");
        };
        let saves = assignment.saves();
        assert_eq!(saves.len(), 1);
        assert_eq!((saves[0].reg, saves[0].inst), (step, call));
        let slot = assign::Place::Slot(saves[0].slot);
        let here = |edit: &&rewrite::Edit| match edit.at {
            rewrite::At::Before(inst) | rewrite::At::After(inst) => inst == call,
            _ => false,
        };
        let around: Vec<_> = allocation
            .edits
            .iter()
            .filter(here)
            .map(|edit| (edit.at, edit.mov.to, edit.mov.from))
            .collect();
        assert_eq!(
            around,
            [
                (rewrite::At::Before(call), slot, assign::Place::Reg(at)),
                (rewrite::At::After(call), assign::Place::Reg(at), slot),
            ]
        );
    }
}
