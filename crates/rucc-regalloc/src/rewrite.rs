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
//! # How many scratch registers one instruction wants
//!
//! Two of a class, and a target holds two of each back for exactly this. The instruction that asks
//! for most is a two address one that reads two values and writes a third with nothing of the three
//! in a register, and the arithmetic works out because the two reads are what use the two scratch
//! registers and the answer is written into the one the operand it reuses was read into. Writing
//! over that destroys nothing, since it holds a copy of a value whose home is a stack slot, and the
//! answer is stored away from it afterwards. Giving the answer a scratch register of its own would
//! want a third, which a program with enough live values around a call reaches, and that was issue
//! #350.
//!
//! It is only a scratch register the answer may have that way. Where the operand it reuses is in a
//! register the assignment gave out, the value in it may be wanted after the instruction, and the
//! assignment only lets one be written over when it is not, which it says by giving the answer that
//! register in the first place. So the answer takes a scratch register there and the two address
//! copy fills it, and the count still comes to two, because an operand that is in a register is not
//! holding a scratch register.
//!
//! Deciding either way needs to know where the operand it reuses went, so an operand that reuses
//! another and has no register of its own is placed in a second pass over the operands.
//!
//! The count is per class. An instruction reading a spilled value out of each of two files wants
//! the first register of each, since a class holds its own back and nothing on the instruction is
//! in the other's.
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
    let mut taken = Taken::new();

    // Where the assignment put each operand's value, taken before anything is rewritten, since
    // rewriting an operand is what loses that. The second pass below reads it.
    let places: Vec<Place> =
        operands.iter().map(|operand| place(assignment, operand.reg)).collect();

    // A spilled operand that reuses another is left for the second pass, because where it goes
    // depends on where the operand it reuses went and that is not known until every operand ahead
    // of it has been placed.
    let mut reusing: Vec<usize> = Vec::new();

    for (index, operand) in operands.iter_mut().enumerate() {
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
            (Place::Slot(_), None) if matches!(operand.constraint, Constraint::Reuse(_)) => {
                reusing.push(index);
                continue;
            }
            (Place::Slot(slot), fixed) => {
                let at = fixed.unwrap_or_else(|| taken.next(env, operand.class));
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

    for index in reusing {
        let Constraint::Reuse(other) = operands[index].constraint else {
            unreachable!("only an operand that reuses another was left for this pass")
        };
        let Place::Slot(slot) = places[index] else {
            unreachable!("only a spilled operand was left for this pass")
        };
        // Where the operand it reuses was read into, if it was read into anywhere. A scratch
        // register holds a copy of a value that lives on the stack, so writing over it destroys
        // nothing and the instruction can have it. A register the assignment gave out is a
        // different matter: the value in it may be wanted after the instruction, and the
        // assignment only lets one be written over when it is not, which it says by giving the
        // answer that register. So a fresh scratch register there, and the copy below fills it.
        //
        // Either way the instruction wants two of the class and no more. If the operand it reuses
        // is on the stack then it is holding one of them already, and if it is not then it is not
        // holding one at all.
        let other = usize::from(other);
        let at = match places[other] {
            Place::Slot(_) => phys(operands[other].reg),
            Place::Reg(_) => taken.next(env, operands[index].class),
        };
        push(
            &mut before,
            &mut after,
            &operands[index],
            Move::new(Place::Reg(at), Place::Slot(slot)),
        );
        operands[index].reg = Reg::physical(at);
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

/// How many scratch registers of each class one instruction has been handed.
///
/// Counted per class rather than in one running number, because the classes hold their own back
/// and an instruction reading a spilled value out of each of two files would otherwise skip the
/// first register of the second file for no reason.
#[derive(Debug, Default)]
struct Taken(Vec<usize>);

impl Taken {
    /// Nothing handed out yet.
    fn new() -> Self {
        Self::default()
    }

    /// The next scratch register of a class.
    ///
    /// # Panics
    ///
    /// Panics if the class has none left, which is an instruction wanting more registers to read
    /// spilled values into than the target held back. Two is enough for every instruction a target
    /// here writes, since a two address instruction reads at most two values and writes into the
    /// register one of them arrived in.
    fn next(&mut self, env: &Env, class: RegClass) -> PhysReg {
        let index = usize::from(class.number());
        if self.0.len() <= index {
            self.0.resize(index + 1, 0);
        }
        let scratch = *env
            .scratch(class)
            .get(self.0[index])
            .expect("an instruction wanting more scratch registers than the class has");
        self.0[index] += 1;
        scratch
    }
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
    use rucc_target::x86_64::{GPR, RAX, RDX, REGS, SYSV, XMM};

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
