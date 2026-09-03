//! Making an assignment true in the function it was worked out for.
//!
//! Design: `spec/10-backend.md` section 10.4.
//!
//! [`crate::assign`] says where every value goes and touches nothing. This is the other half: every
//! operand is rewritten to the place its value was given, and the moves that the places do not
//! already say are collected. After it the function names no virtual register and no block asks
//! for anything, which is the point at which machine IR stops being in SSA form and starts being
//! something an encoder could read.
//!
//! # Why the moves are handed back rather than written
//!
//! A move is an instruction, and an instruction has an opcode, and an opcode belongs to a target.
//! `spec/10-backend.md` section 10.8 says no pipeline crate holds target specific code, so this
//! crate is not the one that can write `x64.mov`. What it hands back is an [`Edit`]: a move
//! between two places, the class it is in, and where in the function it goes. `rucc-codegen` turns
//! each one into whatever its target moves a register with, which for a value on the stack is a
//! load or a store rather than a move at all.
//!
//! The edits at any one place are in the order they have to be made in. That matters in two
//! places: a spilled operand is read into a scratch register before the instruction that wants it,
//! and a two address instruction's copy has to come after that read, because what it is copying
//! may be the thing that was just read in.
//!
//! # What a fixed register turns into
//!
//! A move each way. The assignment deliberately gave the value some other register, so a division
//! whose dividend has to be in `rax` gets a move into `rax` in front of it and a move out of `rax`
//! behind it. That is the cost of the rule the assignment follows, and it is the rule that keeps
//! the `-O0` allocator one pass.
//!
//! # What an edge turns into
//!
//! The moves that write the block's parameters, in an order they can be made in one at a time,
//! which is what [`crate::moves`] is for. Where they go depends on the shape of the edge. A block
//! with one successor puts them at its own end, in front of the branch it finishes with, and a
//! block with several puts them at the start of the block the edge goes to, which is safe exactly
//! because that block has no other predecessor. An edge that is critical has neither place to put
//! them and has to have been split before allocation ran, which this checks rather than assumes.
//!
//! An edge is also the one place a value can be asked to go from one stack slot to another, which
//! happens when a spilled value is passed to a parameter that was itself spilled. No machine here
//! has that instruction, so the move goes through a register, and the register is a second scratch
//! rather than the one the ordering may be holding a value in for the length of a cycle. Expanding
//! it here rather than leaving it to the target is the same decision as everything else in this
//! file: a move through a temporary is a fact about places, and which register is free to be the
//! temporary is a fact only this crate has.

use rucc_mir::{Block, Constraint, Func, Inst, Operand, Param, Reg};
use rucc_target::{PhysReg, RegClass};

use crate::assign::{Assignment, Env, Place};
use crate::moves::{self, Move};

/// One move the places did not already make true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
    /// Where in the function it goes.
    pub at: At,
    /// What it moves, and where to.
    pub mov: Move<Place>,
    /// The class both places are in, which is what says how wide the move is.
    pub class: RegClass,
}

/// Where an edit goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// In front of an instruction, which is where a value it reads is put where it wants it.
    Before(Inst),
    /// Behind an instruction, which is where a value it wrote somewhere it insisted on is taken
    /// away to where it lives.
    After(Inst),
    /// At the start of a block, in front of everything in it.
    StartOf(Block),
    /// At the end of a block, behind everything in it. Only ever a block with one edge out of
    /// it, since a block with two puts an edge's moves at the start of the block it goes to.
    EndOf(Block),
}

/// Rewrites a function to the places it was given, and says what moves are still wanted.
///
/// # Panics
///
/// Panics if the entry block has parameters, since there is no edge into it for their moves to go
/// on and what arrives in a function is the ABI lowering's to say. Panics on a critical edge, on
/// an edge carrying the wrong number of arguments, and if a class runs out of scratch registers
/// for one instruction or has fewer than two on an edge that moves a spilled value into a spilled
/// parameter, all of which are the caller handing it something it was told not to.
#[must_use]
pub fn rewrite(func: &mut Func, assignment: &Assignment, env: &Env) -> Vec<Edit> {
    let blocks: Vec<Block> = func.blocks().collect();
    assert!(
        func.entry().is_none_or(|entry| func[entry].params.is_empty()),
        "what arrives in a function is not a block parameter"
    );

    let mut edits = Vec::new();
    for &block in &blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            instruction(func, assignment, env, inst, &mut edits);
        }
    }

    let preds = preds(func, &blocks);
    for &block in &blocks {
        edges(func, assignment, env, block, &preds, &mut edits);
    }
    for &block in &blocks {
        func.params_mut(block).clear();
        for call in func.succs_mut(block) {
            call.args.clear();
        }
    }
    edits
}

/// Rewrites one instruction's operands, and says what has to happen either side of it.
fn instruction(
    func: &mut Func,
    assignment: &Assignment,
    env: &Env,
    inst: Inst,
    edits: &mut Vec<Edit>,
) {
    let list = func[inst].operands;
    let mut operands: Vec<Operand> = func[list].to_vec();
    let mut before: Vec<(Move<Place>, RegClass)> = Vec::new();
    let mut after: Vec<(Move<Place>, RegClass)> = Vec::new();
    let mut taken = 0;

    for operand in &mut operands {
        let fixed = match operand.constraint {
            Constraint::Fixed(at) => Some(at),
            _ => None,
        };
        let at = match (place(assignment, operand.reg), fixed) {
            (Place::Reg(at), None) => at,
            (Place::Reg(at), Some(fixed)) => {
                if at != fixed {
                    let (there, here) = (Place::Reg(fixed), Place::Reg(at));
                    push(&mut before, &mut after, operand, Move::new(there, here));
                }
                fixed
            }
            (Place::Slot(slot), fixed) => {
                let at = fixed.unwrap_or_else(|| {
                    let scratch = *env
                        .scratch(operand.class)
                        .get(taken)
                        .expect("an instruction wanting more scratch registers than the class has");
                    taken += 1;
                    scratch
                });
                push(
                    &mut before,
                    &mut after,
                    operand,
                    Move::new(Place::Reg(at), Place::Slot(slot)),
                );
                at
            }
        };
        operand.reg = Reg::physical(at);
    }

    // A two address instruction writes one of the registers it reads, and the copy that makes that
    // true goes after everything else in front of the instruction, since what it reads may be a
    // value that was itself only just read in from the stack.
    for index in 0..operands.len() {
        let Constraint::Reuse(other) = operands[index].constraint else { continue };
        let (to, from) = (operands[index], operands[usize::from(other)]);
        if to.reg != from.reg {
            let mov = Move::new(Place::Reg(phys(to.reg)), Place::Reg(phys(from.reg)));
            before.push((mov, to.class));
        }
    }

    func[list].copy_from_slice(&operands);
    edits.extend(before.into_iter().map(|(mov, class)| Edit { at: At::Before(inst), mov, class }));
    edits.extend(after.into_iter().map(|(mov, class)| Edit { at: At::After(inst), mov, class }));
}

/// Files a move in front of the instruction or behind it, and turns it round for a value the
/// instruction writes, since that one travels the other way.
fn push(
    before: &mut Vec<(Move<Place>, RegClass)>,
    after: &mut Vec<(Move<Place>, RegClass)>,
    operand: &Operand,
    mov: Move<Place>,
) {
    if operand.role.is_def() {
        after.push((Move::new(mov.from, mov.to), operand.class));
    } else {
        before.push((mov, operand.class));
    }
}

/// The moves the edges out of a block turn into.
fn edges(
    func: &mut Func,
    assignment: &Assignment,
    env: &Env,
    block: Block,
    preds: &[usize],
    edits: &mut Vec<Edit>,
) {
    let succs = func[block].succs.clone();
    let single = succs.len() == 1;
    for call in &succs {
        let params = func[call.block].params.clone();
        assert_eq!(
            params.len(),
            call.args.len(),
            "an edge carries what the block it goes to asks for"
        );
        if params.is_empty() {
            continue;
        }
        assert!(
            single || preds[call.block.index()] == 1,
            "a critical edge has nowhere to put its moves and has to be split before allocation"
        );
        let at = if single { At::EndOf(block) } else { At::StartOf(call.block) };
        edits.extend(edge(assignment, env, &params, &call.args, at));
    }
}

/// The moves one edge turns into, in the order they can be made in.
fn edge(assignment: &Assignment, env: &Env, params: &[Param], args: &[Reg], at: At) -> Vec<Edit> {
    let mut classes: Vec<RegClass> = params.iter().map(|param| param.class).collect();
    classes.sort_unstable();
    classes.dedup();

    let mut edits = Vec::new();
    for class in classes {
        // One class at a time, because a scratch register is per class and a value never crosses
        // from one to another on an edge.
        let parallel: Vec<Move<Place>> = params
            .iter()
            .zip(args)
            .filter(|(param, _)| param.class == class)
            .map(|(param, &arg)| Move::new(place(assignment, param.reg), place(assignment, arg)))
            .collect();
        let scratch = env.scratch(class);
        let cycle = *scratch
            .first()
            .expect("a class whose values are passed on an edge and which has no scratch register");
        for mov in moves::sequence(&parallel, Place::Reg(cycle)) {
            match (mov.to, mov.from) {
                // No machine here moves one piece of memory into another, so the value goes
                // through a register, and it is a second scratch rather than the one the ordering
                // above may be holding a value in for the length of a cycle.
                (Place::Slot(_), Place::Slot(_)) => {
                    let through = Place::Reg(*scratch.get(1).expect(
                        "a class passing a spilled value to a spilled parameter and having only \
                         one scratch register",
                    ));
                    edits.push(Edit { at, mov: Move::new(through, mov.from), class });
                    edits.push(Edit { at, mov: Move::new(mov.to, through), class });
                }
                _ => edits.push(Edit { at, mov, class }),
            }
        }
    }
    edits
}

/// How many edges arrive in each block.
fn preds(func: &Func, blocks: &[Block]) -> Vec<usize> {
    let mut preds = vec![0; func.block_count()];
    for &block in blocks {
        for call in &func[block].succs {
            preds[call.block.index()] += 1;
        }
    }
    preds
}

/// Where a register is, whether the allocator put it there or it was already somewhere.
fn place(assignment: &Assignment, reg: Reg) -> Place {
    assignment.place(reg).unwrap_or_else(|| Place::Reg(phys(reg)))
}

/// The physical register a register is, once it has to be one.
fn phys(reg: Reg) -> PhysReg {
    reg.phys().expect("a register the assignment says nothing about and that is not a register")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Opcode};
    use rucc_target::x86_64::{GPR, RAX, RDX, REGS, SYSV};

    use super::*;
    use crate::assign::assign;
    use crate::live::Live;
    use crate::order::Order;

    /// The x86-64 environment, with the last three of the allocation order held back as scratch.
    fn env() -> Env {
        let (order, scratch) = SYSV.int_order.split_at(SYSV.int_order.len() - 3);
        Env::new().with(GPR, order, scratch)
    }

    /// An environment with that many general purpose registers and one scratch after them.
    fn narrow(count: usize) -> Env {
        Env::new().with(GPR, &SYSV.int_order[..count], &SYSV.int_order[count..count + 2])
    }

    /// What a place is called, which is what an assertion reads.
    fn named(place: Place) -> String {
        match place {
            Place::Reg(reg) => REGS.name(GPR, reg).expect("a register").to_string(),
            Place::Slot(slot) => format!("slot{slot}"),
        }
    }

    /// Runs both halves and reports the edits as lines an assertion can read.
    fn run(func: &mut Func, env: &Env) -> Vec<String> {
        let order = Order::of(func);
        let live = Live::of(func, &order);
        let assignment = assign(func, &order, &live, env);
        rewrite(func, &assignment, env)
            .into_iter()
            .map(|edit| {
                let at = match edit.at {
                    At::Before(inst) => format!("before {}", inst.index()),
                    At::After(inst) => format!("after {}", inst.index()),
                    At::StartOf(block) => format!("start of {}", block.index()),
                    At::EndOf(block) => format!("end of {}", block.index()),
                };
                format!("{at}: {} = {}", named(edit.mov.to), named(edit.mov.from))
            })
            .collect()
    }

    /// The registers an instruction's operands ended up naming.
    fn operands(func: &Func, inst: Inst) -> Vec<String> {
        func[func[inst].operands]
            .iter()
            .map(|operand| named(Place::Reg(phys(operand.reg))))
            .collect()
    }

    #[test]
    fn every_operand_ends_up_naming_the_register_its_value_was_given() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        let read = func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish();

        assert_eq!(run(&mut func, &env()), Vec::<String>::new());
        assert_eq!(operands(&func, read), ["rax", "rcx"]);
    }

    #[test]
    fn a_register_an_instruction_insists_on_is_moved_into_and_out_of() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let dividend = func.new_vreg(GPR);
        let quotient = func.new_vreg(GPR);
        func.build(block, opcode).def(dividend, GPR).finish();
        let divide = func
            .build(block, opcode)
            .operand(Operand::write(quotient, GPR).with(Constraint::Fixed(RAX)))
            .operand(Operand::read(dividend, GPR).with(Constraint::Fixed(RAX)))
            .finish();
        func.build(block, opcode).uses(quotient, GPR).finish();

        // The value goes into `rax` in front of the division and the answer comes out of it
        // behind, which is the move the assignment chose to pay for rather than tie a value to a
        // register for the whole of its life.
        assert_eq!(run(&mut func, &env()), ["before 1: rax = rcx", "after 1: rcx = rax"]);
        assert_eq!(operands(&func, divide), ["rax", "rax"]);
    }

    #[test]
    fn a_two_address_instruction_that_did_not_get_its_register_copies_first() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, opcode).def(left, GPR).finish();
        func.build(block, opcode).def(right, GPR).finish();
        let add = func
            .build(block, opcode)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, opcode).uses(left, GPR).finish();

        // The left value is wanted afterwards, so the answer could not have its register and the
        // copy in front of the addition is what makes the instruction two address.
        assert_eq!(run(&mut func, &env()), ["before 2: rdx = rax"]);
        assert_eq!(operands(&func, add), ["rdx", "rax", "rcx"]);
    }

    #[test]
    fn a_two_address_instruction_that_did_get_its_register_copies_nothing() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, opcode).def(left, GPR).finish();
        func.build(block, opcode).def(right, GPR).finish();
        let add = func
            .build(block, opcode)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, opcode).uses(right, GPR).finish();

        assert_eq!(run(&mut func, &env()), Vec::<String>::new());
        assert_eq!(operands(&func, add), ["rax", "rax", "rcx"]);
    }

    #[test]
    fn a_spilled_value_is_read_into_a_scratch_register_at_each_instruction_that_wants_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        let read = func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish();

        // One register between two values, so one of them goes to the stack. It is written there
        // where it is computed and read back where it is wanted, and both ends of that go through
        // the scratch register that is held out of the allocation order for exactly this.
        assert_eq!(run(&mut func, &narrow(1)), ["after 1: slot0 = rcx", "before 2: rcx = slot0"]);
        assert_eq!(operands(&func, read), ["rax", "rcx"]);
    }

    #[test]
    fn an_edge_out_of_a_block_with_one_way_to_go_moves_at_the_end_of_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let tail = func.create_block();
        let held = func.new_vreg(GPR);
        let carried = func.new_vreg(GPR);
        func.build(head, opcode).def(held, GPR).finish();
        func.build(head, opcode).def(carried, GPR).finish();
        func.build(head, opcode).uses(held, GPR).finish();
        let param = func.append_param(tail, GPR);
        *func.succs_mut(head) = vec![BlockCall::with(tail, vec![carried])];
        let read = func.build(tail, opcode).uses(param, GPR).finish();

        // The value the edge carries is in the second register, because the first was busy where
        // the value was written, and the parameter it arrives as is in the first, because by then
        // it is not. So the edge is a move, and it goes at the end of the block it leaves.
        assert_eq!(run(&mut func, &env()), ["end of 0: rax = rcx"]);
        assert_eq!(operands(&func, read), ["rax"]);
        // Nothing arrives in a block any more and no edge carries anything, which is where SSA
        // form stops.
        assert!(func[tail].params.is_empty());
        assert!(func[head].succs[0].args.is_empty());
    }

    #[test]
    fn an_edge_out_of_a_block_with_a_choice_moves_at_the_start_of_where_it_goes() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let left = func.create_block();
        let right = func.create_block();
        let held = func.new_vreg(GPR);
        let carried = func.new_vreg(GPR);
        func.build(head, opcode).def(held, GPR).finish();
        func.build(head, opcode).def(carried, GPR).finish();
        func.build(head, opcode).uses(held, GPR).finish();
        let taken = func.append_param(left, GPR);
        *func.succs_mut(head) = vec![BlockCall::with(left, vec![carried]), BlockCall::to(right)];
        func.build(left, opcode).uses(taken, GPR).finish();

        // The move cannot go at the end of the block it leaves, because the other way out of that
        // block does not want it. It goes at the start of the block it arrives in, which is safe
        // because nothing else arrives there.
        assert_eq!(run(&mut func, &env()), ["start of 1: rax = rcx"]);
    }

    #[test]
    fn two_values_that_swap_on_an_edge_get_an_order_and_a_scratch_register() {
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

        // The loop hands each value back the other way round, which is the case no order of two
        // moves answers, so one of them goes through the scratch register. The edge into the loop
        // moves nothing, because each value is already where the parameter it feeds lives.
        assert_eq!(
            run(&mut func, &env()),
            ["end of 1: r13 = rcx", "end of 1: rcx = rax", "end of 1: rax = r13"]
        );
    }

    #[test]
    fn a_spilled_value_handed_to_a_spilled_parameter_goes_through_a_register() {
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

        // One register between the values and the parameters, so a value on the stack is handed to
        // a parameter on the stack, and no machine here has that instruction. It goes through the
        // second scratch register rather than the first, which is the one the ordering above is
        // entitled to be holding a value in.
        assert_eq!(
            run(&mut func, &narrow(1)),
            [
                "after 1: slot0 = rcx",
                "before 2: rcx = slot1",
                "end of 0: rdx = slot0",
                "end of 0: slot1 = rdx",
            ]
        );
    }

    #[test]
    #[should_panic(expected = "a critical edge has nowhere to put its moves")]
    fn a_critical_edge_is_refused() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let other = func.create_block();
        let join = func.create_block();
        let value = func.new_vreg(GPR);
        func.build(head, opcode).def(value, GPR).finish();
        let param = func.append_param(join, GPR);
        *func.succs_mut(head) = vec![BlockCall::with(join, vec![value]), BlockCall::to(other)];
        *func.succs_mut(other) = vec![BlockCall::with(join, vec![value])];
        func.build(join, opcode).uses(param, GPR).finish();

        let _ = run(&mut func, &env());
    }

    #[test]
    #[should_panic(expected = "what arrives in a function is not a block parameter")]
    fn a_parameter_on_the_entry_block_is_refused() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let param = func.append_param(block, GPR);
        let opcode = Opcode::new(names.intern("x64.nop"));
        func.build(block, opcode).uses(param, GPR).finish();

        let _ = run(&mut func, &env());
    }

    #[test]
    fn a_value_already_in_a_register_is_left_where_it_is() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let inst = func.build(block, opcode).uses(Reg::physical(RDX), GPR).finish();

        assert_eq!(run(&mut func, &env()), Vec::<String>::new());
        assert_eq!(operands(&func, inst), ["rdx"]);
    }
}
