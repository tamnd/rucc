//! The allocation checker: whether an assignment is one the machine can actually run.
//!
//! Design: `spec/10-backend.md` section 10.4, which asks for this in debug and CI builds.
//!
//! A register allocator is the pass whose bugs are hardest to find from the outside. It does not
//! change what a program means, so a wrong allocation compiles, links and runs, and then produces
//! the wrong number in one function of one program under one register pressure. The stack trace
//! points at the arithmetic, the arithmetic is right, and the value it read was overwritten four
//! instructions earlier by something unrelated. A checker turns all of that into an assertion at
//! the point the mistake was made, naming the two values and the register they were both put in.
//!
//! # What it asks
//!
//! Five questions, and they are the whole of what an assignment has to get right.
//!
//! Every value the function reads is written first, on every path that reaches the read. Every
//! value the function uses has somewhere to live. Two values that are both wanted at the same
//! point are not in the same register or the same slot. Nothing is sitting in a register that an
//! instruction insists on for itself, because that register belongs to the instruction for as long
//! as it runs. A value an instruction can only read from memory is in memory.
//!
//! The first of those is not about the allocation at all, since the value would be read before it
//! was written whatever register it went to. It is asked here because this is where the answer is
//! already computed: a value read before it is written is a value live on the way into the entry
//! block, and the liveness the allocator needs anyway says which those are. A function that gets
//! this wrong is one the allocator will happily place, and what comes out reads a stack slot
//! nothing ever stored to.
//!
//! # What it does not ask
//!
//! Whether the allocation is any good. A function with every value on the stack passes, and so it
//! should: it is slow and it is correct, and this is the thing that says which of the two a
//! problem is. Quality is what the numbers in `spec/14-target-ladder.md` are for.
//!
//! It also does not read the rewrite. It runs on the assignment, before [`crate::rewrite`] has
//! touched the function, because the assignment is the decision and the rewrite is a
//! transcription of it. A rewrite that transcribes a good decision badly is a different bug, and
//! [`crate::trace`] is the checker that catches it by following each value from the instruction
//! that wrote it to the instructions that read it.
//!
//! # Why it repeats work
//!
//! The two address instructions are worked out again here rather than borrowed from
//! [`crate::assign`], and that is deliberate. A checker that shares its reasoning with the thing
//! it checks agrees with it about everything, including the mistakes, and the one bug it can never
//! find is the one in the code they share. Fifteen lines is a cheap price for a second opinion.
//!
//! It is also allowed to be slow. Looking for a value in a register an instruction wants is the
//! plain product of the values and the constrained operands, with no index over either, because a
//! checker runs in debug builds and in CI and the thing it is checking is the thing that has to be
//! fast.

use std::fmt;

use rucc_mir::{Constraint, Func, Inst, Reg, Role};
use rucc_target::{PhysReg, RegClass};

use crate::assign::{Assignment, Place};
use crate::live::{Area, Live, Range};
use crate::order::{Order, Point};

/// One thing wrong with an allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// A value the function reads or writes was given no place at all.
    Nowhere {
        /// The value with nowhere to be.
        reg: Reg,
    },
    /// Two values that are both live somewhere were put in the same place, so whichever is written
    /// second destroys the other.
    Shared {
        /// The value that was there first.
        first: Reg,
        /// The value that was put on top of it.
        second: Reg,
        /// The place they were both given.
        place: Place,
    },
    /// A value was left in a register an instruction claims for itself, over the instruction that
    /// claims it, so the moves around that instruction overwrite the value.
    InTheWay {
        /// The value in the way.
        reg: Reg,
        /// The register the instruction insists on.
        at: PhysReg,
        /// The instruction that insists on it.
        inst: Inst,
    },
    /// A value an instruction can only read from memory was put in a register.
    NotOnTheStack {
        /// The value that has to be in memory.
        reg: Reg,
        /// The instruction that says so.
        inst: Inst,
    },
    /// A value is read on some path from the entry block without anything on that path having
    /// written it, so what the instruction reading it gets is whatever was left there.
    NeverWritten {
        /// The value nothing writes.
        reg: Reg,
    },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::Nowhere { reg } => write!(f, "{} has nowhere to live", name(*reg)),
            Problem::Shared { first, second, place } => {
                let (first, second) = (name(*first), name(*second));
                write!(f, "{first} and {second} are both live and both in {}", place_name(*place))
            }
            Problem::InTheWay { reg, at, inst } => {
                let reg = name(*reg);
                let inst = inst.index();
                write!(f, "{reg} is in register {}, which instruction {inst} wants", at.number())
            }
            Problem::NotOnTheStack { reg, inst } => {
                let reg = name(*reg);
                write!(f, "{reg} is not on the stack, and instruction {} needs it", inst.index())
            }
            Problem::NeverWritten { reg } => {
                write!(f, "{} is read before anything writes it", name(*reg))
            }
        }
    }
}

/// Everything wrong with an allocation, in an order a person can read.
///
/// An empty answer is the one every allocation is supposed to give. Anything else is a compiler
/// bug rather than a program the compiler cannot handle, which is why [`crate::run`] asserts on it
/// instead of reporting it as a diagnostic.
///
/// # Panics
///
/// Panics on a function with two billion virtual registers in it, which is a function no machine
/// has the memory to hold.
#[must_use]
pub fn check(func: &Func, order: &Order, live: &Live, assignment: &Assignment) -> Vec<Problem> {
    let mut problems = Vec::new();
    // What arrives live in the entry block is what the function reads without writing, since
    // nothing runs in front of the entry block to have written it.
    if let Some(entry) = func.entry() {
        for reg in live.live_in(entry) {
            problems.push(Problem::NeverWritten { reg });
        }
    }
    let reuses = reuses(func, order);
    let mut values = Vec::new();
    for (number, reuse) in reuses.iter().enumerate() {
        let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
        let (Some(mut area), Some(class)) = (live.area(reg), func.class_of(reg)) else {
            continue;
        };
        let Some(place) = assignment.place(reg) else {
            problems.push(Problem::Nowhere { reg });
            continue;
        };
        // A two address instruction writes its answer into a register it read, so the answer is
        // really in that register from the moment the instruction starts and not from the moment
        // it ends. Reading its area any other way lets it share the register with something the
        // same instruction is still reading.
        if let Some(reuse) = reuse {
            area = area.with(reuse.at);
        }
        values.push(Value { reg, class, range: area.hull(), area, place });
    }
    overlaps(&values, &reuses, live, &mut problems);
    instructions(func, order, assignment, &values, &reuses, &mut problems);
    problems
}

/// Everything wrong with an allocation, as an assertion message.
#[must_use]
pub fn report(problems: &[Problem]) -> String {
    let places = if problems.len() == 1 { "place" } else { "places" };
    let mut report = format!("the allocation is wrong in {} {places}", problems.len());
    for problem in problems {
        report.push_str("\n  ");
        report.push_str(&problem.to_string());
    }
    report
}

/// One value, where it is wanted and where it was put.
#[derive(Debug, Clone, Copy)]
struct Value<'a> {
    reg: Reg,
    class: RegClass,
    /// The interval around the area, which is what the sweep below reads.
    range: Range,
    /// Everywhere the value is really live, which is what says whether sharing a place with
    /// another value is a mistake.
    area: Area<'a>,
    place: Place,
}

/// A value written into the register another operand of the same instruction was read from.
#[derive(Debug, Clone, Copy)]
struct Reuse {
    source: Reg,
    at: Point,
}

/// Looks for two values that are both live somewhere and were put in the same place.
///
/// A sweep in the order the values start, holding the ones whose interval still reaches this one,
/// so the pairs it compares are the pairs that can be wrong rather than all of them. The interval
/// is generous, so a pair that survives the sweep is then asked whether the areas inside those
/// intervals really meet.
fn overlaps(
    values: &[Value<'_>],
    reuses: &[Option<Reuse>],
    live: &Live,
    problems: &mut Vec<Problem>,
) {
    let mut sorted = values.to_vec();
    sorted.sort_by_key(|value| (value.range.start, value.reg));
    let mut active: Vec<Value<'_>> = Vec::new();
    for value in sorted {
        active.retain(|held| held.range.end >= value.range.start);
        for held in &active {
            if !together(*held, value)
                || !held.area.overlaps(value.area)
                || coalesced(*held, value, reuses, live)
            {
                continue;
            }
            problems.push(Problem::Shared {
                first: held.reg,
                second: value.reg,
                place: value.place,
            });
        }
        active.push(value);
    }
}

/// Whether two values were put in the same place.
///
/// Two registers of different classes are different registers even when they are the same number,
/// which is what a class is. Two slots are the same slot whatever is in them, because a frame is
/// one piece of memory.
fn together(first: Value<'_>, second: Value<'_>) -> bool {
    match (first.place, second.place) {
        (Place::Reg(first_at), Place::Reg(second_at)) => {
            first_at == second_at && first.class == second.class
        }
        (Place::Slot(first_slot), Place::Slot(second_slot)) => first_slot == second_slot,
        _ => false,
    }
}

/// Whether one of the two is the answer a two address instruction wrote into the register it read
/// the other from, which is the one overlap that is not a mistake.
///
/// It only holds when the value being read is finished with at that instruction. A value read
/// again afterwards needs its register afterwards, so writing over it is the plain bug this whole
/// file exists to find.
///
/// It also only holds when the value being written is not already live where the instruction
/// reads. The area read here is the one liveness worked out, without the extra point the reuse
/// adds, so a value that covers the reuse point on its own is one that was already live on the way
/// in. That is what a loop carrying its own answer round looks like: written at the bottom and read
/// by the next turn. Such a value is wanted where the instruction reads as well as after it, so it
/// is genuinely on top of the one it reuses and no excuse at the one instruction they share makes
/// them fit in a single register.
fn coalesced(first: Value<'_>, second: Value<'_>, reuses: &[Option<Reuse>], live: &Live) -> bool {
    let pair = |source: Value<'_>, dest: Value<'_>| {
        let Some(reuse) = reuses[index(dest.reg)] else { return false };
        reuse.source == source.reg
            && live.area(dest.reg).is_some_and(|area| !area.covers(reuse.at))
            && live.range(source.reg).is_some_and(|r| r.end == reuse.at)
    };
    pair(first, second) || pair(second, first)
}

/// Looks for a value in a register an instruction wants, and for a value that had to be in memory
/// and is not.
fn instructions(
    func: &Func,
    order: &Order,
    assignment: &Assignment,
    values: &[Value<'_>],
    reuses: &[Option<Reuse>],
    problems: &mut Vec<Problem>,
) {
    for block in func.blocks() {
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                if operand.constraint == Constraint::Stack
                    && matches!(assignment.place(operand.reg), Some(Place::Reg(_)))
                {
                    problems.push(Problem::NotOnTheStack { reg: operand.reg, inst });
                }
                // A physical register an operand names outright is claimed exactly as firmly as
                // one a constraint asks for, since nothing before allocation writes one except an
                // instruction that has no choice.
                let at = match operand.constraint {
                    Constraint::Fixed(at) => Some(at),
                    _ => operand.reg.phys(),
                };
                let Some(at) = at else { continue };
                let early = order.early(inst);
                let point = if operand.role == Role::Def { order.late(inst) } else { early };
                for value in values {
                    let mine = value.reg == operand.reg
                        || reuses[index(value.reg)].is_some_and(|reuse| {
                            reuse.source == operand.reg
                                && reuse.at == early
                                && value.place == Place::Reg(at)
                        });
                    if mine || value.class != operand.class {
                        continue;
                    }
                    // The interval around a value covers blocks the value never reaches, so what
                    // decides this is the area inside it, which says whether the value is live at
                    // this point rather than whether the point is between its ends.
                    // tamnd/rucc#982.
                    if value.place == Place::Reg(at) && value.area.covers(point) {
                        problems.push(Problem::InTheWay { reg: value.reg, at, inst });
                    }
                }
            }
        }
    }
}

/// The value each two address instruction reuses, by the virtual register it writes.
fn reuses(func: &Func, order: &Order) -> Vec<Option<Reuse>> {
    let mut reuses = vec![None; func.vregs()];
    for block in func.blocks() {
        for inst in func.insts(block) {
            let operands = &func[func[inst].operands];
            for operand in operands {
                let Constraint::Reuse(other) = operand.constraint else { continue };
                let number = operand.reg.number().and_then(|number| usize::try_from(number).ok());
                let Some(number) = number else { continue };
                let source = operands[usize::from(other)].reg;
                reuses[number] = Some(Reuse { source, at: order.early(inst) });
            }
        }
    }
    reuses
}

/// A virtual register's number as a table index, and zero for a physical one, which never has an
/// entry of its own and is never what a reuse writes.
fn index(reg: Reg) -> usize {
    reg.number().and_then(|number| usize::try_from(number).ok()).unwrap_or(0)
}

/// What a value is called in a report.
fn name(reg: Reg) -> String {
    match reg.number() {
        Some(number) => format!("%{number}"),
        None => format!("register {}", reg.phys().expect("a physical register").number()),
    }
}

/// What a place is called in a report, without the target's name for it, since this crate holds
/// nothing of any target.
fn place_name(place: Place) -> String {
    match place {
        Place::Reg(at) => format!("register {}", at.number()),
        Place::Slot(slot) => format!("slot {slot}"),
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Opcode, Operand};
    use rucc_target::x86_64::{GPR, RAX, RCX, RDX, SYSV};

    use super::*;
    use crate::assign::{Env, assign};

    /// The x86-64 environment, with the last three of the allocation order held back as scratch.
    fn env() -> Env {
        let (order, scratch) = SYSV.int_order.split_at(SYSV.int_order.len() - 3);
        Env::new().with(GPR, order, scratch)
    }

    /// What the checker says about the allocation the single pass allocator works out, which is
    /// supposed to be nothing at all.
    fn allocated(func: &Func) -> Vec<String> {
        let order = Order::of(func);
        let live = Live::of(func, &order);
        let assignment = assign(func, &order, &live, &env());
        said(func, &order, &live, &assignment)
    }

    /// What the checker says about an allocation somebody wrote by hand.
    fn said(func: &Func, order: &Order, live: &Live, assignment: &Assignment) -> Vec<String> {
        check(func, order, live, assignment).iter().map(ToString::to_string).collect()
    }

    /// The order and the liveness of a function, which every hand written case needs both of.
    fn read(func: &Func) -> (Order, Live) {
        let order = Order::of(func);
        let live = Live::of(func, &order);
        (order, live)
    }

    #[test]
    fn an_allocation_the_allocator_worked_out_has_nothing_wrong_with_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish();

        assert_eq!(allocated(&func), Vec::<String>::new());
    }

    #[test]
    fn a_value_with_nowhere_to_live_is_found() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let only = func.new_vreg(GPR);
        func.build(block, opcode).def(only, GPR).finish();
        func.build(block, opcode).uses(only, GPR).finish();

        let (order, live) = read(&func);
        let assignment = Assignment::empty(func.vregs());

        assert_eq!(said(&func, &order, &live, &assignment), ["%0 has nowhere to live"]);
    }

    #[test]
    fn a_value_read_before_anything_writes_it_is_found() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let never = func.new_vreg(GPR);
        func.build(block, opcode).uses(never, GPR).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(never, Place::Reg(RAX));

        assert_eq!(
            said(&func, &order, &live, &assignment),
            ["%0 is read before anything writes it"]
        );
    }

    #[test]
    fn two_values_that_are_both_wanted_and_share_a_register_are_found() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(first, Place::Reg(RAX));
        assignment.put(second, Place::Reg(RAX));

        let said = said(&func, &order, &live, &assignment);
        assert_eq!(said, ["%0 and %1 are both live and both in register 0"]);
    }

    #[test]
    fn a_value_that_lives_in_a_hole_of_another_may_share_its_register() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let arm = func.create_block();
        let tail = func.create_block();
        let across = func.new_vreg(GPR);
        let inside = func.new_vreg(GPR);
        func.build(entry, opcode).def(across, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(arm), BlockCall::to(tail)];
        func.build(arm, opcode).def(inside, GPR).finish();
        func.build(arm, opcode).uses(inside, GPR).finish();
        func.build(tail, opcode).uses(across, GPR).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(across, Place::Reg(RAX));
        assignment.put(inside, Place::Reg(RAX));

        // The arm is written between the two blocks the first value is live in and is a block that
        // value's own path never goes through, so the second is not sitting on top of it and the
        // interval around the first saying so is not what decides this. A checker that read the
        // intervals would call every register the allocator has learned to share a value written
        // over another. tamnd/rucc#982.
        assert_eq!(said(&func, &order, &live, &assignment), Vec::<String>::new());
    }

    #[test]
    fn two_values_that_are_both_wanted_and_share_a_slot_are_found() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        let slot = assignment.take_slot(GPR);
        assignment.put(first, Place::Slot(slot));
        assignment.put(second, Place::Slot(slot));

        let said = said(&func, &order, &live, &assignment);
        assert_eq!(said, ["%0 and %1 are both live and both in slot 0"]);
    }

    #[test]
    fn two_values_that_are_never_both_wanted_may_share_anything() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).uses(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(second, GPR).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(first, Place::Reg(RAX));
        assignment.put(second, Place::Reg(RAX));

        assert_eq!(said(&func, &order, &live, &assignment), Vec::<String>::new());
    }

    #[test]
    fn a_value_left_in_a_register_an_instruction_wants_is_found() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let nop = Opcode::new(names.intern("x64.nop"));
        let divide = Opcode::new(names.intern("x64.idiv"));
        let block = func.create_block();
        let held = func.new_vreg(GPR);
        let dividend = func.new_vreg(GPR);
        func.build(block, nop).def(held, GPR).finish();
        func.build(block, nop).def(dividend, GPR).finish();
        // The division reads its dividend out of one register and no other, so anything still
        // wanted afterwards has to be somewhere else while it runs.
        func.build(block, divide)
            .operand(Operand::read(dividend, GPR).with(Constraint::Fixed(RAX)))
            .finish();
        func.build(block, nop).uses(held, GPR).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(held, Place::Reg(RAX));
        assignment.put(dividend, Place::Reg(RCX));

        let said = said(&func, &order, &live, &assignment);
        assert_eq!(said, ["%0 is in register 0, which instruction 2 wants"]);
    }

    #[test]
    fn the_value_an_instruction_wants_a_register_for_may_be_in_it_already() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let nop = Opcode::new(names.intern("x64.nop"));
        let divide = Opcode::new(names.intern("x64.idiv"));
        let block = func.create_block();
        let dividend = func.new_vreg(GPR);
        func.build(block, nop).def(dividend, GPR).finish();
        func.build(block, divide)
            .operand(Operand::read(dividend, GPR).with(Constraint::Fixed(RAX)))
            .finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(dividend, Place::Reg(RAX));

        // Being in the register the instruction wanted is the best answer, not a problem, and the
        // rewrite writes no move at all for it.
        assert_eq!(said(&func, &order, &live, &assignment), Vec::<String>::new());
    }

    #[test]
    fn a_value_that_can_only_be_read_from_memory_and_is_in_a_register_is_found() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let nop = Opcode::new(names.intern("x64.nop"));
        let wide = Opcode::new(names.intern("x64.wide"));
        let block = func.create_block();
        let only = func.new_vreg(GPR);
        func.build(block, nop).def(only, GPR).finish();
        func.build(block, wide).operand(Operand::read(only, GPR).with(Constraint::Stack)).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(only, Place::Reg(RAX));

        let said = said(&func, &order, &live, &assignment);
        assert_eq!(said, ["%0 is not on the stack, and instruction 1 needs it"]);
    }

    #[test]
    fn a_two_address_instruction_may_write_the_register_it_read_a_finished_value_from() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let nop = Opcode::new(names.intern("x64.nop"));
        let add = Opcode::new(names.intern("x64.add"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, nop).def(left, GPR).finish();
        func.build(block, nop).def(right, GPR).finish();
        func.build(block, add)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, nop).uses(sum, GPR).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(left, Place::Reg(RAX));
        assignment.put(right, Place::Reg(RCX));
        assignment.put(sum, Place::Reg(RAX));

        // The left operand is finished with at the addition, so the sum takes its register and
        // the addition is the one instruction rather than a move and an instruction.
        assert_eq!(said(&func, &order, &live, &assignment), Vec::<String>::new());
    }

    #[test]
    fn a_two_address_instruction_may_not_write_over_a_value_wanted_afterwards() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let nop = Opcode::new(names.intern("x64.nop"));
        let add = Opcode::new(names.intern("x64.add"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, nop).def(left, GPR).finish();
        func.build(block, nop).def(right, GPR).finish();
        func.build(block, add)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, nop).uses(sum, GPR).uses(left, GPR).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(left, Place::Reg(RAX));
        assignment.put(right, Place::Reg(RCX));
        assignment.put(sum, Place::Reg(RAX));

        // The left operand is read again after the addition, so the addition may not have its
        // register even though it is the one the addition reads.
        let said = said(&func, &order, &live, &assignment);
        assert_eq!(said, ["%0 and %2 are both live and both in register 0"]);
    }

    #[test]
    fn a_two_address_instruction_may_not_write_the_register_it_reads_its_other_operand_from() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let nop = Opcode::new(names.intern("x64.nop"));
        let add = Opcode::new(names.intern("x64.add"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, nop).def(left, GPR).finish();
        func.build(block, nop).def(right, GPR).finish();
        func.build(block, add)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, nop).uses(sum, GPR).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(left, Place::Reg(RAX));
        assignment.put(right, Place::Reg(RCX));
        assignment.put(sum, Place::Reg(RCX));

        // Copying the left operand into the sum's register would destroy the right operand before
        // the addition has read it, even though both are finished with at the addition.
        let said = said(&func, &order, &live, &assignment);
        assert_eq!(said, ["%1 and %2 are both live and both in register 1"]);
    }

    #[test]
    fn a_two_address_instruction_may_not_write_the_register_it_read_over_its_own_last_answer() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let nop = Opcode::new(names.intern("x64.nop"));
        let add = Opcode::new(names.intern("x64.add"));
        let head = func.create_block();
        let latch = func.create_block();
        let out = func.create_block();
        let source = func.new_vreg(GPR);
        let carried = func.new_vreg(GPR);
        func.build(head, nop).def(source, GPR).finish();
        func.build(head, nop).def(carried, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(latch)];
        func.build(latch, add)
            .operand(Operand::write(carried, GPR).with(Constraint::Reuse(1)))
            .uses(source, GPR)
            .uses(carried, GPR)
            .finish();
        *func.succs_mut(latch) = vec![BlockCall::to(head), BlockCall::to(out)];
        func.build(out, nop).uses(carried, GPR).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(source, Place::Reg(RAX));
        assignment.put(carried, Place::Reg(RAX));

        // The source is finished with at the addition, which is what would normally let the answer
        // have its register. It does not here, because the answer is the one the last turn round
        // the loop wrote and the addition reads it too, so both are wanted where it reads.
        let said = said(&func, &order, &live, &assignment);
        assert_eq!(said, ["%0 and %1 are both live and both in register 0"]);
    }

    #[test]
    fn a_two_address_answer_with_a_hole_in_front_of_it_still_may_not_take_the_other_operand() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let nop = Opcode::new(names.intern("x64.nop"));
        let add = Opcode::new(names.intern("x64.add"));
        let entry = func.create_block();
        let head = func.create_block();
        let arm = func.create_block();
        let latch = func.create_block();
        let out = func.create_block();
        let seed = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        let inside = func.new_vreg(GPR);
        let loaded = func.new_vreg(GPR);
        func.build(entry, nop).def(seed, GPR).finish();
        func.build(entry, nop).def(sum, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(head)];
        func.build(head, nop).uses(sum, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(arm), BlockCall::to(latch)];
        func.build(arm, nop).def(inside, GPR).finish();
        func.build(arm, nop).uses(inside, GPR).finish();
        *func.succs_mut(arm) = vec![BlockCall::to(out)];
        func.build(latch, nop).def(loaded, GPR).finish();
        func.build(latch, add)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(seed, GPR)
            .uses(loaded, GPR)
            .finish();
        *func.succs_mut(latch) = vec![BlockCall::to(head), BlockCall::to(out)];

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(seed, Place::Reg(RCX));
        assignment.put(sum, Place::Reg(RAX));
        assignment.put(inside, Place::Reg(RDX));
        assignment.put(loaded, Place::Reg(RAX));

        // The answer is live in the entry and the head too, so the piece the addition writes is not
        // the first one and the arm in between is a hole. Copying the left operand into the answer's
        // register still destroys the right operand before the addition reads it, and a checker that
        // added the extra point to the first piece rather than the piece the addition writes saw
        // nothing wrong with any of it. tamnd/rucc#982.
        let said = said(&func, &order, &live, &assignment);
        assert_eq!(said, ["%1 and %3 are both live and both in register 0"]);
    }

    #[test]
    fn a_report_names_every_problem() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish();

        let (order, live) = read(&func);
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(first, Place::Reg(RAX));
        assignment.put(second, Place::Reg(RAX));

        let problems = check(&func, &order, &live, &assignment);
        assert_eq!(
            report(&problems),
            "the allocation is wrong in 1 place\n  %0 and %1 are both live and both in register 0"
        );
        assert_eq!(report(&[]), "the allocation is wrong in 0 places");
    }
}
