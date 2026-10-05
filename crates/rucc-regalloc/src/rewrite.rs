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
//! What each instruction needs around it for the machine to accept the places its operands were
//! given is worked out in [`crate::legalize`], and this files what that says as edits.
//!
//! # A value put away around an instruction
//!
//! A value [`crate::backtrack`] kept in a register an instruction destroys is stored to its slot
//! in front of everything else the instruction needs and loaded back behind everything else. In
//! front because the moves that hand the instruction its operands read what they move and never
//! write the value's register, which nothing at the instruction insists on, and behind because the
//! moves that take its answers away may read the register the call returned something in. Either
//! way round would be correct for the value. These are the only places it is not.
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

use rucc_base::hash::Map;
use rucc_mir::{Block, Func, Inst, Param, Reg};
use rucc_target::RegClass;

use crate::assign::{Assignment, Env, Place};
use crate::legalize::{self, Spare, place};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
/// an edge carrying the wrong number of arguments, and if a class has fewer than two registers on
/// an edge that moves a spilled value into a spilled parameter, all of which are the caller handing
/// it something it was told not to.
///
/// The assignment is taken by reference and may gain a slot, which is the one the register borrowed
/// at an instruction with more spilled operands than the class holds registers back for waits in.
/// The section above says what the borrowing is, and the slot is asked for here rather than planned
/// before allocation because most functions never want one.
#[must_use]
pub fn rewrite(func: &mut Func, assignment: &mut Assignment, env: &Env) -> Vec<Edit> {
    let blocks: Vec<Block> = func.blocks().collect();
    assert!(
        func.entry().is_none_or(|entry| func[entry].params.is_empty()),
        "what arrives in a function is not a block parameter"
    );

    let mut edits = Vec::new();
    let mut spare = Spare::default();
    // Collected once for the whole function rather than once a block, since rewriting an
    // instruction needs the function and the walk over the blocks would be borrowing it.
    let insts: Vec<Inst> = blocks.iter().flat_map(|&block| func.insts(block)).collect();
    let saves = saves(func, assignment);
    for inst in insts {
        let saved = saves.get(&inst).map_or(&[][..], Vec::as_slice);
        instruction(func, assignment, env, &mut spare, inst, saved, &mut edits);
    }

    let preds = preds(func, &blocks);
    for &block in &blocks {
        behind_the_end(func, block, &preds, &mut edits);
    }
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

/// The stores that put away the values kept in a register each instruction destroys, by the
/// instruction. Each load that brings one back is the same move the other way round.
fn saves(func: &Func, assignment: &Assignment) -> Map<Inst, Vec<(Move<Place>, RegClass)>> {
    let mut saves: Map<Inst, Vec<(Move<Place>, RegClass)>> = Map::default();
    for save in assignment.saves() {
        let (Some(Place::Reg(at)), Some(class)) =
            (assignment.place(save.reg), func.class_of(save.reg))
        else {
            continue;
        };
        let store = Move::new(Place::Slot(save.slot), Place::Reg(at));
        saves.entry(save.inst).or_default().push((store, class));
    }
    saves
}

/// Rewrites one instruction's operands, and files what has to happen either side of it.
fn instruction(
    func: &mut Func,
    assignment: &mut Assignment,
    env: &Env,
    spare: &mut Spare,
    inst: Inst,
    saved: &[(Move<Place>, RegClass)],
    edits: &mut Vec<Edit>,
) {
    let legal = legalize::instruction(func, assignment, env, spare, inst);
    let list = func[inst].operands;
    func[list].copy_from_slice(&legal.operands);
    let before = saved.iter().copied().chain(legal.before);
    edits.extend(before.map(|(mov, class)| Edit { at: At::Before(inst), mov, class }));
    let back = saved.iter().map(|&(store, class)| (Move::new(store.from, store.to), class));
    edits.extend(legal.after.into_iter().chain(back).map(|(mov, class)| Edit {
        at: At::After(inst),
        mov,
        class,
    }));
}

/// Moves what has to happen behind the last instruction of a block that leaves several ways to the
/// start of every block it goes to.
///
/// That instruction is an `asm goto`, the one thing that both writes values and ends a block with
/// more than one edge out of it. Behind it in its own block is the fall through only, since the
/// template has already jumped to a label by then when it was going to, so a value it wrote that
/// has to be taken somewhere else has to be taken there on every edge. Each of those goes to a
/// block with no other way in, which `rucc_codegen::split::critical` sees to, and these are filed
/// before the edge's own moves because those may read what these put in place.
///
/// A branch writes nothing and has nothing behind it, and neither does any other instruction a
/// block that leaves several ways ends in, so this is nothing for all of those.
fn behind_the_end(func: &Func, block: Block, preds: &[usize], edits: &mut Vec<Edit>) {
    let succs = &func[block].succs;
    if succs.len() < 2 {
        return;
    }
    let Some(last) = func.insts(block).last() else { return };
    if !func[func[last].operands].iter().any(|operand| operand.role.is_def()) {
        return;
    }
    let behind: Vec<Edit> =
        edits.iter().filter(|edit| edit.at == At::After(last)).copied().collect();
    if behind.is_empty() {
        return;
    }
    edits.retain(|edit| edit.at != At::After(last));
    for call in succs {
        assert!(
            preds[call.block.index()] == 1,
            "an edge out of an asm goto that writes something has to be split before allocation"
        );
        edits.extend(behind.iter().map(|&edit| Edit { at: At::StartOf(call.block), ..edit }));
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
        //
        // A class with no scratch register is one [`fits`] said goes round in no cycle, so the
        // ordering never reaches for the place it is given to break one with, and that place is
        // nothing rather than a register.
        let parallel: Vec<Move<Option<Place>>> = params
            .iter()
            .zip(args)
            .filter(|(param, _)| param.class == class)
            .map(|(param, &arg)| {
                Move::new(Some(place(assignment, param.reg)), Some(place(assignment, arg)))
            })
            .collect();
        let scratch = env.scratch(class);
        let cycle = scratch.first().map(|&reg| Place::Reg(reg));
        for mov in moves::sequence(&parallel, cycle) {
            let (Some(to), Some(from)) = (mov.to, mov.from) else {
                panic!(
                    "a class whose values go round in a cycle on an edge and which has no scratch \
                     register"
                )
            };
            let mov = Move::new(to, from);
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

/// Whether the rewrite can write an assignment down with no more than the scratch registers `env`
/// holds back.
///
/// Only a class that holds none back can fail. A value of that class on the stack has nowhere to
/// be read into at an instruction that wants it, and an edge whose moves of that class go round in
/// a cycle has nowhere to keep one value while the others move. Those are the only two things the
/// rewrite takes a scratch register for, so an assignment with neither is one it never asks for one
/// in. A slot a value waits in around a call is not either of them, since the value is in its
/// register on both sides and the moves are between that register and the slot.
#[must_use]
pub fn fits(func: &Func, assignment: &Assignment, env: &Env) -> bool {
    let bare = |class: RegClass| env.scratch(class).is_empty();
    let slots = assignment.slots();
    let stacked = assignment.placed().any(|(_, at)| match at {
        Place::Slot(slot) => usize::try_from(slot)
            .ok()
            .and_then(|slot| slots.get(slot))
            .is_some_and(|&class| bare(class)),
        Place::Reg(_) => false,
    });
    if stacked {
        return false;
    }
    for block in func.blocks() {
        for call in &func[block].succs {
            let params = &func[call.block].params;
            let mut classes: Vec<RegClass> =
                params.iter().map(|param| param.class).filter(|&class| bare(class)).collect();
            classes.sort_unstable();
            classes.dedup();
            for class in classes {
                // The ordering is given a place that is not one to break a cycle with, so a move
                // out of it in what comes back is where it would have taken a scratch register.
                let parallel: Vec<Move<Option<Place>>> = params
                    .iter()
                    .zip(&call.args)
                    .filter(|(param, _)| param.class == class)
                    .map(|(param, &arg)| {
                        Move::new(Some(place(assignment, param.reg)), Some(place(assignment, arg)))
                    })
                    .collect();
                if moves::sequence(&parallel, None).iter().any(|mov| mov.from.is_none()) {
                    return false;
                }
            }
        }
    }
    true
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

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Constraint, Opcode, Operand};
    use rucc_target::x86_64::{GPR, RAX, RCX, RDX, REGS, RSI, SYSV, XMM};

    use super::*;
    use crate::assign::assign;
    use crate::legalize::phys;
    use crate::live::Live;
    use crate::order::Order;

    /// The x86-64 environment, with the last three of the allocation order held back as scratch.
    fn env() -> Env {
        let (order, scratch) = SYSV.int_order.split_at(SYSV.int_order.len() - 3);
        Env::new().with(GPR, order, scratch)
    }

    /// An environment with that many general purpose registers and two scratch after them.
    fn narrow(count: usize) -> Env {
        Env::new().with(GPR, &SYSV.int_order[..count], &SYSV.int_order[count..count + 2])
    }

    /// What a place is called, which is what an assertion reads.
    ///
    /// The class comes in because a register is a number within its class and the two files here
    /// number from zero, so nothing but the class tells `rcx` from `xmm1`.
    fn named(class: RegClass, place: Place) -> String {
        match place {
            Place::Reg(reg) => REGS.name(class, reg).expect("a register").to_string(),
            Place::Slot(slot) => format!("slot{slot}"),
        }
    }

    /// Runs both halves and reports the edits as lines an assertion can read.
    fn run(func: &mut Func, env: &Env) -> Vec<String> {
        let order = Order::of(func);
        let live = Live::of(func, &order);
        let mut assignment = assign(func, &order, &live, env);
        rewrite(func, &mut assignment, env)
            .into_iter()
            .map(|edit| {
                let at = match edit.at {
                    At::Before(inst) => format!("before {}", inst.index()),
                    At::After(inst) => format!("after {}", inst.index()),
                    At::StartOf(block) => format!("start of {}", block.index()),
                    At::EndOf(block) => format!("end of {}", block.index()),
                };
                format!(
                    "{at}: {} = {}",
                    named(edit.class, edit.mov.to),
                    named(edit.class, edit.mov.from)
                )
            })
            .collect()
    }

    /// The registers an instruction's operands ended up naming.
    fn operands(func: &Func, inst: Inst) -> Vec<String> {
        func[func[inst].operands]
            .iter()
            .map(|operand| named(operand.class, Place::Reg(phys(operand.reg))))
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
    fn a_register_an_instruction_insists_on_costs_nothing_when_the_values_can_have_it() {
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

        // Nothing either side of the division. The dividend is read out of `rax` for the last
        // time and the quotient is written into it afterwards, so both of them live there and the
        // moves that used to carry the value in and the answer out are not written.
        assert_eq!(run(&mut func, &env()), Vec::<String>::new());
        assert_eq!(operands(&func, divide), ["rax", "rax"]);
    }

    #[test]
    fn a_register_an_instruction_insists_on_is_moved_into_when_the_value_cannot_have_it() {
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
        func.build(block, opcode).uses(dividend, GPR).finish();

        // This time the dividend is wanted after the division, so it cannot be in the register the
        // division writes and the value is moved in. The answer still comes out of `rax` without
        // a move, which is the half of it the hint bought.
        assert_eq!(run(&mut func, &env()), ["before 1: rax = rcx"]);
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

    /// A two address instruction with nothing in a register is two scratch registers and not three.
    ///
    /// The answer has no register of its own to be in, so what it is written into is whichever one
    /// the operand it reuses was read into, and it is stored away from there afterwards. Handing it
    /// a scratch register of its own would want a third, and a class holds two back, which is issue
    /// #350: a program with enough live values around a call reached it and the compiler aborted.
    #[test]
    fn a_two_address_instruction_whose_answer_and_operands_are_all_spilled_wants_two_registers() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let keeper = func.new_vreg(GPR);
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, opcode).def(keeper, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(left, GPR).with(Constraint::Stack))
            .finish();
        func.build(block, opcode)
            .operand(Operand::write(right, GPR).with(Constraint::Stack))
            .finish();
        let add = func
            .build(block, opcode)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, opcode).uses(keeper, GPR).finish();
        func.build(block, opcode).uses(sum, GPR).finish();

        // Both operands are read in, the answer is written into the register the operand it
        // reuses arrived in, and it is stored away from there. Two scratch registers, which is
        // what the class holds back. Asking for one of its own would be a third and would abort.
        assert_eq!(
            run(&mut func, &narrow(1)),
            [
                "after 1: slot0 = rcx",
                "after 2: slot1 = rcx",
                "before 3: rcx = slot0",
                "before 3: rdx = slot1",
                "after 3: slot2 = rcx",
                "before 5: rcx = slot2",
            ]
        );
        assert_eq!(operands(&func, add), ["rcx", "rcx", "rdx"]);
    }

    /// A three address instruction with nothing in a register is two scratch registers, not three.
    ///
    /// The case #726 aborted on. `x64.lea_64` and the `x64.cmp_set_*` family read two values and
    /// write a third that is neither of them, and when all three ends are on the stack there are
    /// three operands wanting a register at one instruction. Counting them in one running number
    /// asks for a third scratch register and the class holds two back.
    ///
    /// Two is enough because the answer's register is not wanted until the instruction writes it,
    /// by which time the registers the operands were read into have been read. So the answer goes
    /// back into the first of them and is stored away from there.
    #[test]
    fn a_three_address_instruction_whose_answer_and_operands_are_all_spilled_wants_two_registers() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let keeper = func.new_vreg(GPR);
        let base = func.new_vreg(GPR);
        let index = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        func.build(block, opcode).def(keeper, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(base, GPR).with(Constraint::Stack))
            .finish();
        func.build(block, opcode)
            .operand(Operand::write(index, GPR).with(Constraint::Stack))
            .finish();
        let lea =
            func.build(block, opcode).def(address, GPR).uses(base, GPR).uses(index, GPR).finish();
        func.build(block, opcode).uses(keeper, GPR).finish();
        func.build(block, opcode).uses(address, GPR).finish();

        // Both operands are read in, the answer is written into the first of the two registers
        // they arrived in, and it is stored away from there. Two, which is what the class holds.
        assert_eq!(
            run(&mut func, &narrow(1)),
            [
                "after 1: slot0 = rcx",
                "after 2: slot1 = rcx",
                "before 3: rcx = slot0",
                "before 3: rdx = slot1",
                "after 3: slot2 = rcx",
                "before 5: rcx = slot2",
            ]
        );
        assert_eq!(operands(&func, lea), ["rcx", "rcx", "rdx"]);
    }

    /// A spilled answer takes a scratch register where the operand it reuses is in a real one.
    ///
    /// The value in that register may be wanted after the instruction, and the assignment is the
    /// only thing that knows whether it is. It says so by giving the answer that register, and here
    /// it did not, so writing over it would destroy a value. The count still comes to two, because
    /// an operand that is in a register is not holding a scratch register.
    #[test]
    fn a_spilled_answer_does_not_write_over_a_register_the_assignment_gave_to_something_else() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, opcode).def(left, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(right, GPR).with(Constraint::Stack))
            .finish();
        let add = func
            .build(block, opcode)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, opcode).uses(left, GPR).finish();
        func.build(block, opcode).uses(sum, GPR).finish();

        // The left value is in `rax` and is read again afterwards, so the answer is copied into a
        // scratch register and written there instead.
        assert_eq!(
            run(&mut func, &narrow(1)),
            [
                "after 1: slot0 = rcx",
                "before 2: rcx = slot0",
                "before 2: rdx = rax",
                "after 2: slot1 = rdx",
                "before 4: rcx = slot1",
            ]
        );
        assert_eq!(operands(&func, add), ["rdx", "rax", "rcx"]);
    }

    /// The count of scratch registers handed out is per class and not one number for all of them.
    ///
    /// An instruction reading a spilled value out of each of two files wants the first register of
    /// each, since the files hold their own back and nothing on the instruction is in the other's.
    #[test]
    fn an_instruction_reading_out_of_two_files_takes_the_first_scratch_register_of_each() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let integer = func.new_vreg(GPR);
        let number = func.new_vreg(XMM);
        let spare = func.new_vreg(GPR);
        let other = func.new_vreg(XMM);
        func.build(block, opcode).def(integer, GPR).finish();
        func.build(block, opcode).def(number, XMM).finish();
        func.build(block, opcode).def(spare, GPR).finish();
        func.build(block, opcode).def(other, XMM).finish();
        func.build(block, opcode).uses(integer, GPR).uses(number, XMM).finish();
        let read = func.build(block, opcode).uses(spare, GPR).uses(other, XMM).finish();

        // One register in each file, so the value of each that is wanted later goes to the stack
        // and is read back at the instruction that wants it.
        let env = Env::new().with(GPR, &SYSV.int_order[..1], &SYSV.int_order[1..3]).with(
            XMM,
            &SYSV.sse_order[..1],
            &SYSV.sse_order[1..3],
        );
        assert_eq!(
            run(&mut func, &env),
            [
                "after 2: slot0 = rcx",
                "after 3: slot1 = xmm1",
                "before 5: rcx = slot0",
                "before 5: xmm1 = slot1",
            ]
        );
        assert_eq!(operands(&func, read), ["rcx", "xmm1"]);
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

    /// Three values read and none written wants a third register, which is tamnd/rucc#913.
    ///
    /// There is no answer here to fold back into the register an operand arrived in, so the trick
    /// that keeps a two address instruction down to two has nothing to work on and each of the
    /// three wants a register of its own. The instruction is the indexed store: `a[i] = v` reads a
    /// base, an index and a value, and at `-O0`, where nothing is coalesced, all three of them are
    /// stack slots. The rewriter aborted on it, which stopped brotli and cmocka on the first file
    /// that held one and sqlite3 on `fts5Init`.
    ///
    /// The third register is borrowed rather than held back, and the borrowing is what this is
    /// really about: it takes a register the allocator gave to a value that is live right across
    /// the instruction, which is safe because that value is put in a slot in front of the
    /// instruction and brought back behind it.
    #[test]
    fn an_instruction_reading_three_spilled_values_borrows_a_register_for_the_third() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let keeper = func.new_vreg(GPR);
        let base = func.new_vreg(GPR);
        let index = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        func.build(block, opcode).def(keeper, GPR).finish();
        for reg in [base, index, value] {
            func.build(block, opcode)
                .operand(Operand::write(reg, GPR).with(Constraint::Stack))
                .finish();
        }
        let store =
            func.build(block, opcode).uses(base, GPR).uses(index, GPR).uses(value, GPR).finish();
        func.build(block, opcode).uses(keeper, GPR).finish();

        assert_eq!(
            run(&mut func, &narrow(2)),
            [
                "after 1: slot0 = rdx",
                "after 2: slot1 = rdx",
                "after 3: slot2 = rdx",
                "before 4: slot3 = rax",
                "before 4: rdx = slot0",
                "before 4: rsi = slot1",
                "before 4: rax = slot2",
                "after 4: rax = slot3",
            ]
        );
        assert_eq!(operands(&func, store), ["rdx", "rsi", "rax"]);
    }

    /// A register the instruction only writes still carries a value in.
    ///
    /// A call names every caller saved register as one it writes, and on x86-64 the two held back for
    /// scratch are both caller saved, so an indirect call through a pointer on the stack has nowhere
    /// to read the pointer into unless a register named only on the way out is still free on the way
    /// in. Reading them as spoken for stopped cmocka on its first file.
    #[test]
    fn a_register_the_instruction_only_writes_still_carries_a_value_in() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let target = func.new_vreg(GPR);
        func.build(block, opcode)
            .operand(Operand::write(target, GPR).with(Constraint::Stack))
            .finish();
        let call = func
            .build(block, opcode)
            .def(Reg::physical(RDX), GPR)
            .def(Reg::physical(RSI), GPR)
            .uses(target, GPR)
            .finish();

        assert_eq!(run(&mut func, &narrow(2)), ["after 0: slot0 = rdx", "before 1: rdx = slot0"]);
        assert_eq!(operands(&func, call), ["rdx", "rsi", "rdx"]);
    }

    /// A scratch register the instruction has already named for itself is passed over.
    ///
    /// The move that carries a value into a register a fixed constraint asks for and the move that
    /// fills a scratch register both go in front of the instruction, so handing the same register
    /// out twice would lose one of the two values without anything saying so. On x86-64 the way
    /// into this is inline assembly naming `r10` or `r11`, which are the two the file holds back.
    #[test]
    fn a_register_the_instruction_already_named_is_not_handed_out_as_scratch() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let wanted = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        for reg in [wanted, other] {
            func.build(block, opcode)
                .operand(Operand::write(reg, GPR).with(Constraint::Stack))
                .finish();
        }
        let read = func
            .build(block, opcode)
            .operand(Operand::read(wanted, GPR).with(Constraint::Fixed(RCX)))
            .uses(other, GPR)
            .finish();

        // `rcx` is both the first scratch register here and the one the instruction insists on, so
        // the value it did not ask for by name starts at the second one instead.
        assert_eq!(
            run(&mut func, &narrow(1)),
            [
                "after 0: slot0 = rcx",
                "after 1: slot1 = rcx",
                "before 2: rcx = slot0",
                "before 2: rdx = slot1"
            ]
        );
        assert_eq!(operands(&func, read), ["rcx", "rdx"]);
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
