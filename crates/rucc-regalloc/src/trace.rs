//! Following every value from the instruction that wrote it to the instructions that read it.
//!
//! Design: `spec/optimizer/39-register-allocation.md` section 39.6, which asks for an independent
//! verifier that every use reads the value its definition produced, and `spec/10-backend.md`
//! section 10.4, which says a check like it runs in debug and CI builds.
//!
//! # Why there are two checkers
//!
//! [`crate::check`] reads the assignment. It asks whether the decision is one the machine can run:
//! whether two values that are live at once were given the same place, whether anything is sitting
//! in a register an instruction claims for itself, and so on. Its own module doc says what it
//! leaves alone, which is the rewrite, on the grounds that the assignment is the decision and the
//! rewrite is a transcription of it.
//!
//! This is the other half. A transcription can lose a value while the decision behind it was
//! right, and the places it can lose one are exactly the places [`crate::rewrite`] does something:
//! handing out scratch registers for a value that lives on the stack, moving a value into the
//! register an instruction insists on, copying one operand into another for a two address form,
//! and putting an edge's moves into an order that can be performed one at a time. Both #350 and
//! #726 were bugs in that code and neither is a shape the assignment checker can see.
//!
//! # What it asks
//!
//! One question. At every instruction, does each register the instruction reads hold the value the
//! operand said it wanted.
//!
//! That is asked by walking the function with a note of which value is in each place, starting
//! from nothing at the entry block. An instruction writing an operand puts that value in the place
//! the operand ended up naming. A move puts what is in one place into another, or takes the note
//! away when the place it reads from held nothing known. An edge carries the block's arguments
//! into the block's parameters, and what arrives is checked and then goes on under the parameter's
//! name, since from there on that is what the value is called.
//!
//! A block with two edges into it keeps only what both of them agree about, because a value that
//! is in one place on one path and another place on the other is not in either. That is a
//! fixed point and it is worked out first, before anything is reported, so that a loop is not
//! complained about on the first time round before the back edge has been seen.
//!
//! # Why it is told the function twice
//!
//! [`shape`] is taken before the rewrite and holds what each operand's value was called and what
//! each edge carried. The rewrite is what loses both: afterwards an operand names a physical
//! register and an edge carries nothing. Checking a transcription means holding on to the thing
//! that was transcribed, and a snapshot of the operand lists is a cheaper way to do that than a
//! copy of the function.
//!
//! # Why it repeats work
//!
//! It works out where a value lives from the assignment again rather than reading it off the
//! rewritten function, and it sequences nothing. A checker that shares its reasoning with the
//! thing it checks agrees with it about the mistakes as well, and the bug it can never find is the
//! one in the code they share.
//!
//! It is also allowed to be slow. The state is a map from places to values and it is copied at
//! every edge, because a checker runs in debug builds and the thing it is checking is the thing
//! that has to be fast.

use std::collections::HashMap;
use std::fmt;

use rucc_mir::{Block, Func, Inst, Operand, Param, Reg, Role};
use rucc_target::RegClass;

use crate::assign::{Assignment, Place};
use crate::rewrite::{At, Edit};

/// One value read out of a place that was not holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// An instruction reads a place holding something other than the value its operand names.
    Read {
        /// The instruction doing the reading.
        inst: Inst,
        /// The place it reads.
        place: Place,
        /// Which file that place is in, since a register is a number inside its class.
        class: RegClass,
        /// The value the operand says is there.
        wanted: Reg,
        /// What is really there, if anything is known to be.
        found: Option<Reg>,
    },
    /// An edge did not leave a block's parameter holding the argument the edge carried for it.
    Arrived {
        /// The block the edge leaves.
        from: Block,
        /// The block it goes to.
        to: Block,
        /// Where the parameter lives.
        place: Place,
        /// Which file that place is in.
        class: RegClass,
        /// The argument that was supposed to arrive there.
        wanted: Reg,
        /// What is really there, if anything is known to be.
        found: Option<Reg>,
    },
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::Read { inst, place, class, wanted, found } => write!(
                f,
                "instruction {} reads {} out of {}, which {}",
                inst.index(),
                name(*wanted),
                spelled(*class, *place),
                holding(*found)
            ),
            Fault::Arrived { from, to, place, class, wanted, found } => write!(
                f,
                "the edge from block {} to block {} was to leave {} in {}, which {}",
                from.index(),
                to.index(),
                name(*wanted),
                spelled(*class, *place),
                holding(*found)
            ),
        }
    }
}

/// What the rewrite is about to lose: what each operand's value was called, and what each edge
/// carries.
///
/// Taken with [`shape`], before [`crate::rewrite`] runs, and read by [`trace`] afterwards.
#[derive(Debug, Clone, Default)]
pub struct Shape {
    /// Every instruction's operands as they were, by the instruction's index.
    operands: Vec<Vec<Operand>>,
    /// Every block's parameters, by the block's index.
    params: Vec<Vec<Param>>,
    /// Every block's edges out and what each one carries, by the block's index.
    succs: Vec<Vec<Call>>,
}

/// One edge out of a block, as it was before the rewrite emptied it.
#[derive(Debug, Clone)]
struct Call {
    block: Block,
    args: Vec<Reg>,
}

/// What a function looked like before the rewrite.
///
/// # Panics
///
/// Panics on a function with more instructions than a `usize` counts, which is one no machine has
/// the memory to hold.
#[must_use]
pub fn shape(func: &Func) -> Shape {
    let mut shape = Shape {
        operands: vec![Vec::new(); func.inst_count()],
        params: vec![Vec::new(); func.block_count()],
        succs: vec![Vec::new(); func.block_count()],
    };
    for block in func.blocks() {
        shape.params[block.index()] = func[block].params.clone();
        shape.succs[block.index()] = func[block]
            .succs
            .iter()
            .map(|call| Call { block: call.block, args: call.args.clone() })
            .collect();
        for inst in func.insts(block) {
            shape.operands[inst.index()] = func[func[inst].operands].to_vec();
        }
    }
    shape
}

/// Everywhere the rewritten function reads a place that is not holding the value it wants.
///
/// An empty answer is the one every allocation is supposed to give. Anything else is a compiler
/// bug rather than a program the compiler cannot handle, which is why [`crate::run`] asserts on it
/// instead of reporting it as a diagnostic.
///
/// The function is the rewritten one, the shape is what [`shape`] took of it beforehand, and the
/// edits are the ones the rewrite handed back. The three together are the whole of what the
/// machine will be asked to run.
#[must_use]
pub fn trace(func: &Func, shape: &Shape, assignment: &Assignment, edits: &[Edit]) -> Vec<Fault> {
    let filed = File::of(func, edits);
    let mut entry: Vec<Option<State>> = vec![None; func.block_count()];
    let Some(start) = func.entry() else { return Vec::new() };

    entry[start.index()] = Some(arrived(shape));
    let mut queue = vec![start];
    let mut ignored = Vec::new();
    while let Some(block) = queue.pop() {
        let Some(state) = entry[block.index()].clone() else { continue };
        ignored.clear();
        let out = body(func, shape, &filed, edits, block, state, &mut ignored);
        let single = shape.succs[block.index()].len() == 1;
        for call in &shape.succs[block.index()] {
            let over =
                cross(shape, &filed, edits, assignment, block, call, single, &out, &mut ignored);
            if narrow(&mut entry[call.block.index()], &over) {
                queue.push(call.block);
            }
        }
    }

    // Now that every block's state has settled, one pass to say what is wrong with it. Doing this
    // inside the loop above would complain about the first time round a loop, before the back edge
    // has had a chance to take anything away.
    let mut faults = Vec::new();
    for block in func.blocks() {
        let Some(state) = entry[block.index()].clone() else { continue };
        let out = body(func, shape, &filed, edits, block, state, &mut faults);
        let single = shape.succs[block.index()].len() == 1;
        for call in &shape.succs[block.index()] {
            cross(shape, &filed, edits, assignment, block, call, single, &out, &mut faults);
        }
    }
    faults
}

/// Everything wrong with a rewritten function, as an assertion message.
#[must_use]
pub fn report(faults: &[Fault]) -> String {
    let places = if faults.len() == 1 { "place" } else { "places" };
    let mut report = format!("the rewrite loses a value in {} {places}", faults.len());
    for fault in faults {
        report.push_str("\n  ");
        report.push_str(&fault.to_string());
    }
    report
}

/// What is already in place on the way into the function.
///
/// A value the allocator placed arrives nowhere, since nothing has run to write it, and a value
/// read before it is written is the assignment checker's question rather than this one's. What
/// does arrive is every physical register the function names without the allocator having handed
/// it out: the stack pointer, the frame pointer, and whatever else the calling convention leaves
/// standing. Nothing defines those, so without this every instruction reaching the frame through
/// the frame pointer would be read as reading a register nothing had written.
///
/// Each of them starts out holding itself, which is the only name it has. That still catches an
/// instruction or a move that writes over one of them, because writing over it puts a different
/// value there and the next read of it says so.
fn arrived(shape: &Shape) -> State {
    let mut state = State::new();
    for operands in &shape.operands {
        for operand in operands {
            if operand.role == Role::Use && operand.reg.phys().is_some() {
                state
                    .entry(spot(operand.class, Place::Reg(operand.reg.phys().expect("physical"))))
                    .or_insert(operand.reg);
            }
        }
    }
    state
}

/// Which value is in which place, as far as anything is known.
///
/// A place with no entry is one nothing is known about, which is either a place nothing has
/// written yet or one the paths into a block disagree about. Reading one is a fault, since the
/// machine will read whatever is there.
type State = HashMap<Spot, Reg>;

/// A place and the file it is in.
///
/// The class is in here because a physical register is a number inside its class, so the first
/// general purpose register and the first floating point register are both register zero and are
/// not the same place at all.
type Spot = (u8, Place);

/// The place an operand of that class ended up naming.
fn spot(class: RegClass, place: Place) -> Spot {
    (class.number(), place)
}

/// Which edits go where, as indexes into the list the rewrite handed back.
///
/// The rewrite hands its edits back in the order it made them, which is every instruction's and
/// then every edge's, so a walk in program order has to be able to ask for the ones at a point
/// rather than read them in the order they arrived.
#[derive(Debug, Default)]
struct File {
    before: Vec<Vec<usize>>,
    after: Vec<Vec<usize>>,
    start_of: Vec<Vec<usize>>,
    end_of: Vec<Vec<usize>>,
}

impl File {
    /// The edits of a function, filed by where in it they go.
    fn of(func: &Func, edits: &[Edit]) -> Self {
        let mut filed = File {
            before: vec![Vec::new(); func.inst_count()],
            after: vec![Vec::new(); func.inst_count()],
            start_of: vec![Vec::new(); func.block_count()],
            end_of: vec![Vec::new(); func.block_count()],
        };
        for (index, edit) in edits.iter().enumerate() {
            match edit.at {
                At::Before(inst) => filed.before[inst.index()].push(index),
                At::After(inst) => filed.after[inst.index()].push(index),
                At::StartOf(block) => filed.start_of[block.index()].push(index),
                At::EndOf(block) => filed.end_of[block.index()].push(index),
            }
        }
        filed
    }
}

/// Walks one block, saying what is where at the end of it and what went wrong on the way.
///
/// The edits an edge into this block turned into are not applied here. They belong to the edge and
/// [`cross`] has already made them true in the state this is handed.
fn body(
    func: &Func,
    shape: &Shape,
    filed: &File,
    edits: &[Edit],
    block: Block,
    mut state: State,
    faults: &mut Vec<Fault>,
) -> State {
    for inst in func.insts(block) {
        for &edit in &filed.before[inst.index()] {
            moved(&mut state, &edits[edit]);
        }
        let was = &shape.operands[inst.index()];
        let now = &func[func[inst].operands];

        // Every read first and every write afterwards, because an instruction reads its operands
        // before it writes its answer. A write that lands on something the same instruction still
        // wants to read is the assignment checker's question and it has already asked it.
        for (operand, place) in was.iter().zip(now.iter()) {
            let Some(at) = landed(place) else { continue };
            if operand.role != Role::Use {
                continue;
            }
            let found = state.get(&spot(operand.class, at)).copied();
            if found != Some(operand.reg) {
                let (class, wanted) = (operand.class, operand.reg);
                faults.push(Fault::Read { inst, place: at, class, wanted, found });
            }
        }
        for (operand, place) in was.iter().zip(now.iter()) {
            let Some(at) = landed(place) else { continue };
            if !operand.role.is_def() {
                continue;
            }
            state.insert(spot(operand.class, at), operand.reg);
        }
        for &edit in &filed.after[inst.index()] {
            moved(&mut state, &edits[edit]);
        }
    }
    state
}

/// Carries one edge's values into the block it goes to, and says what is where when they arrive.
///
/// The moves go at the end of the block the edge leaves when that is its only edge out, and at the
/// start of the block it goes to otherwise, which is safe exactly because a block reached by an
/// edge from a block with two of them has no other edge into it.
#[allow(clippy::too_many_arguments, reason = "an edge is the two blocks and everything between")]
fn cross(
    shape: &Shape,
    filed: &File,
    edits: &[Edit],
    assignment: &Assignment,
    from: Block,
    call: &Call,
    single: bool,
    out: &State,
    faults: &mut Vec<Fault>,
) -> State {
    let mut state = out.clone();
    let params = &shape.params[call.block.index()];
    let list =
        if single { &filed.end_of[from.index()] } else { &filed.start_of[call.block.index()] };
    for &edit in list {
        moved(&mut state, &edits[edit]);
    }

    // What each parameter is called from here on. Worked out from the state the moves left and
    // then written back all at once, because two parameters can be in each other's places and
    // renaming one before the other has been read would lose the second.
    let mut arrived = Vec::new();
    for (param, &arg) in params.iter().zip(&call.args) {
        let Some(at) = home(assignment, param.reg) else { continue };
        let found = state.get(&spot(param.class, at)).copied();
        if found != Some(arg) {
            let (to, class) = (call.block, param.class);
            faults.push(Fault::Arrived { from, to, place: at, class, wanted: arg, found });
        }
        arrived.push((spot(param.class, at), param.reg));
    }
    for (spot, reg) in arrived {
        state.insert(spot, reg);
    }
    state
}

/// Performs one move on the state, which is putting what is in one place into another.
///
/// A move out of a place nothing is known about takes the note away rather than leaving a stale
/// one, since what it copied is whatever was there.
fn moved(state: &mut State, edit: &Edit) {
    let to = spot(edit.class, edit.mov.to);
    let from = spot(edit.class, edit.mov.from);
    match state.get(&from).copied() {
        Some(reg) => state.insert(to, reg),
        None => state.remove(&to),
    };
}

/// Keeps only what a block's state and an edge into it agree about, and says whether that changed
/// anything.
///
/// A block nothing has reached yet takes the edge's state whole. After that every edge can only
/// take something away, which is what makes the walk finish.
fn narrow(entry: &mut Option<State>, over: &State) -> bool {
    match entry {
        None => {
            *entry = Some(over.clone());
            true
        }
        Some(state) => {
            let before = state.len();
            state.retain(|spot, reg| over.get(spot) == Some(&*reg));
            state.len() != before
        }
    }
}

/// Where an operand ended up, which after the rewrite is always a physical register.
fn landed(operand: &Operand) -> Option<Place> {
    operand.reg.phys().map(Place::Reg)
}

/// Where a value lives, whether the assignment put it there or it was a physical register already.
///
/// Nothing for a value that is neither, which is a value the assignment was never asked about. The
/// assignment checker reports that as a value with nowhere to live, so this leaves it alone rather
/// than saying the same thing twice.
fn home(assignment: &Assignment, reg: Reg) -> Option<Place> {
    assignment.place(reg).or_else(|| reg.phys().map(Place::Reg))
}

/// What a value is called in a report.
fn name(reg: Reg) -> String {
    match reg.number() {
        Some(number) => format!("%{number}"),
        None => match reg.phys() {
            Some(at) => format!("register {}", at.number()),
            None => "nothing".to_owned(),
        },
    }
}

/// What a place is called in a report, without the target's name for it, since this crate holds
/// nothing of any target.
fn spelled(class: RegClass, place: Place) -> String {
    match place {
        Place::Reg(at) => format!("register {} of class {}", at.number(), class.number()),
        Place::Slot(slot) => format!("slot {slot}"),
    }
}

/// What a place is holding, as the end of a sentence.
fn holding(found: Option<Reg>) -> String {
    match found {
        Some(reg) => format!("holds {}", name(reg)),
        None => "holds nothing anything has put there".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Constraint, Opcode, Operand};
    use rucc_target::x86_64::{GPR, RAX, RSP, SYSV};

    use super::*;
    use crate::assign::{Env, assign};
    use crate::live::Live;
    use crate::moves::Move;
    use crate::order::Order;
    use crate::rewrite::rewrite;

    /// The x86-64 environment, with the last three of the allocation order held back as scratch.
    fn env() -> Env {
        let (order, scratch) = SYSV.int_order.split_at(SYSV.int_order.len() - 3);
        Env::new().with(GPR, order, scratch)
    }

    /// An environment with that many general purpose registers and two scratch after them.
    fn narrow(count: usize) -> Env {
        Env::new().with(GPR, &SYSV.int_order[..count], &SYSV.int_order[count..count + 2])
    }

    /// Allocates a function and hands back everything the checker is told about it.
    fn allocate(func: &mut Func, env: &Env) -> (Shape, Assignment, Vec<Edit>) {
        let order = Order::of(func);
        let live = Live::of(func, &order);
        let assignment = assign(func, &order, &live, env);
        let taken = shape(func);
        let edits = rewrite(func, &assignment, env);
        (taken, assignment, edits)
    }

    /// What the checker says about a rewritten function, as lines an assertion can read.
    fn said(func: &Func, taken: &Shape, assignment: &Assignment, edits: &[Edit]) -> Vec<String> {
        trace(func, taken, assignment, edits).iter().map(ToString::to_string).collect()
    }

    #[test]
    fn every_value_an_instruction_reads_is_the_one_that_was_written_where_it_reads_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish();

        let (taken, assignment, edits) = allocate(&mut func, &env());
        assert_eq!(said(&func, &taken, &assignment, &edits), Vec::<String>::new());
    }

    #[test]
    fn a_value_that_lives_on_the_stack_is_followed_through_the_slot_it_lives_in() {
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

        // Two registers and three values wanted at once, so one of them is stored to a slot and
        // read back out of it, and the checker has to follow it both ways to say nothing is wrong.
        let (taken, assignment, edits) = allocate(&mut func, &narrow(2));
        assert_eq!(assignment.spilled(), 1);
        assert_eq!(said(&func, &taken, &assignment, &edits), Vec::<String>::new());
    }

    #[test]
    fn an_operand_the_rewrite_pointed_at_the_wrong_register_is_reported() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        let read = {
            func.build(block, opcode).def(first, GPR).finish();
            func.build(block, opcode).def(second, GPR).finish();
            func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish()
        };

        let (taken, assignment, edits) = allocate(&mut func, &env());
        assert_eq!(said(&func, &taken, &assignment, &edits), Vec::<String>::new());

        // The checker has to be checking, so point one operand at the register next door, which is
        // what a rewrite that handed out the wrong register would leave, and see it named. The
        // assignment behind it is untouched and is still the right one.
        let list = func[read].operands;
        func[list][0].reg = func[list][1].reg;

        assert_eq!(
            said(&func, &taken, &assignment, &edits),
            ["instruction 2 reads %0 out of register 1 of class 0, which holds %1"]
        );
    }

    #[test]
    fn a_move_that_writes_the_wrong_register_is_an_instruction_reading_the_wrong_value() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let dividend = func.new_vreg(GPR);
        let quotient = func.new_vreg(GPR);
        func.build(block, opcode).def(dividend, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(quotient, GPR).with(Constraint::Fixed(RAX)))
            .operand(Operand::read(dividend, GPR).with(Constraint::Fixed(RAX)))
            .finish();
        func.build(block, opcode).uses(quotient, GPR).finish();
        func.build(block, opcode).uses(dividend, GPR).finish();

        let (taken, assignment, mut edits) = allocate(&mut func, &env());
        assert_eq!(said(&func, &taken, &assignment, &edits), Vec::<String>::new());

        // The move that carries the dividend into the register the instruction insists on, sent
        // to the register next door instead. That is the shape of a rewrite that hands out the
        // wrong scratch register, and it has to be caught at the instruction that reads it.
        edits[0].mov.to = Place::Reg(SYSV.int_order[2]);
        assert_eq!(
            said(&func, &taken, &assignment, &edits),
            ["instruction 1 reads %0 out of register 0 of class 0, which holds nothing anything \
              has put there"]
        );
    }

    #[test]
    fn a_value_carried_over_an_edge_goes_on_under_the_name_the_block_it_arrives_in_gives_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let tail = func.create_block();
        let value = func.new_vreg(GPR);
        func.build(head, opcode).def(value, GPR).finish();
        let arrived = func.append_param(tail, GPR);
        *func.succs_mut(head) = vec![BlockCall::with(tail, vec![value])];
        func.build(tail, opcode).uses(arrived, GPR).finish();

        let (taken, assignment, edits) = allocate(&mut func, &env());
        assert_eq!(said(&func, &taken, &assignment, &edits), Vec::<String>::new());
    }

    #[test]
    fn two_values_that_swap_on_an_edge_arrive_the_right_way_round_only_in_the_order_they_were_put_in()
     {
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

        let (taken, assignment, mut edits) = allocate(&mut func, &env());
        assert_eq!(said(&func, &taken, &assignment, &edits), Vec::<String>::new());

        // The same swap written as the two moves it looks like, which is what a sequencer that
        // did not notice the cycle would leave. The second move reads a register the first has
        // already written, so both parameters end up holding the same value.
        let (to, from) = (Place::Reg(SYSV.int_order[0]), Place::Reg(SYSV.int_order[1]));
        edits.truncate(edits.len() - 3);
        edits.push(Edit { at: At::EndOf(body), mov: Move::new(to, from), class: GPR });
        edits.push(Edit { at: At::EndOf(body), mov: Move::new(from, to), class: GPR });

        assert_eq!(
            said(&func, &taken, &assignment, &edits),
            ["the edge from block 1 to block 1 was to leave %2 in register 1 of class 0, which \
                 holds %3"]
        );
    }

    #[test]
    fn a_loop_is_walked_until_it_settles_rather_than_reported_the_first_time_round() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let body = func.create_block();
        let latch = func.create_block();
        let out = func.create_block();
        let start = func.new_vreg(GPR);
        func.build(head, opcode).def(start, GPR).finish();
        let counter = func.append_param(body, GPR);
        *func.succs_mut(head) = vec![BlockCall::with(body, vec![start])];
        let next = func.new_vreg(GPR);
        func.build(body, opcode).def(next, GPR).uses(counter, GPR).finish();
        *func.succs_mut(body) = vec![BlockCall::to(latch), BlockCall::to(out)];
        func.build(latch, opcode).finish();
        *func.succs_mut(latch) = vec![BlockCall::with(body, vec![next])];
        func.build(out, opcode).finish();

        // The back edge is what the counter arrives on the second time round, and the checker only
        // reports anything once every edge into the block has been taken into account.
        let (taken, assignment, edits) = allocate(&mut func, &env());
        assert_eq!(said(&func, &taken, &assignment, &edits), Vec::<String>::new());
    }

    #[test]
    fn a_value_the_two_ways_into_a_block_leave_in_different_places_is_not_one_it_may_read() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let arm = func.create_block();
        let tail = func.create_block();
        let value = func.new_vreg(GPR);
        func.build(entry, opcode).def(value, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(arm), BlockCall::to(tail)];
        func.build(arm, opcode).finish();
        *func.succs_mut(arm) = vec![BlockCall::to(tail)];
        func.build(tail, opcode).uses(value, GPR).finish();

        let (taken, assignment, mut edits) = allocate(&mut func, &env());
        assert_eq!(said(&func, &taken, &assignment, &edits), Vec::<String>::new());

        // One arm of the diamond writes over the register the value is in. The block the two arms
        // meet in reads it, and what it gets depends on which way the program came, which is
        // exactly the case a walk that only followed one path would miss.
        let at = Place::Reg(SYSV.int_order[0]);
        let elsewhere = Place::Reg(SYSV.int_order[1]);
        edits.push(Edit { at: At::EndOf(arm), mov: Move::new(at, elsewhere), class: GPR });

        assert_eq!(
            said(&func, &taken, &assignment, &edits),
            ["instruction 2 reads %0 out of register 0 of class 0, which holds nothing anything \
              has put there"]
        );
    }

    #[test]
    fn a_register_the_function_arrives_holding_is_one_it_may_read_without_writing_it_first() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        func.build(block, opcode).operand(Operand::read(Reg::physical(RSP), GPR)).finish();

        // The stack pointer is not the allocator's to hand out and nothing in the function writes
        // it, so a walk that started from nothing at all would read every frame reference as a
        // read of a register nobody had written.
        let (taken, assignment, edits) = allocate(&mut func, &env());
        assert_eq!(said(&func, &taken, &assignment, &edits), Vec::<String>::new());
    }

    #[test]
    fn what_is_wrong_is_reported_in_a_sentence_that_says_how_many_things_are_wrong() {
        let fault = Fault::Read {
            inst: Inst::new(3),
            place: Place::Slot(1),
            class: GPR,
            wanted: Reg::virtual_reg(2),
            found: None,
        };
        assert_eq!(
            report(&[fault]),
            "the rewrite loses a value in 1 place\n  instruction 3 reads %2 out of slot 1, which \
             holds nothing anything has put there"
        );
    }
}
