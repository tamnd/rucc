//! Which register each value lives in, and which values live on the stack instead.
//!
//! Design: `spec/10-backend.md` section 10.4.
//!
//! This is the `-O0` allocator's decision and nothing else. It is linear scan over the line
//! [`crate::order`] lays the function out in: the values are taken in the order they are written,
//! each is given a register that nothing else live at the same time is in, and when there is no
//! such register one of the values in flight goes to the stack instead. There is no splitting and
//! no coalescing, so a value gets one place for the whole of its range and keeps it. That produces
//! mediocre code quickly, which is what `-O0` is for, and the allocator that produces good code
//! slowly is a separate one, in M4.
//!
//! Which value is sent to the stack is the one whose range ends last, counting the value being
//! placed among the candidates. A value wanted for a long time is the cheapest to spill per
//! instruction it frees a register over, and it is the only heuristic here.
//!
//! # What it does with a register an instruction insists on
//!
//! Two things. It stays out of that register for everybody else, and it tries that register first
//! for the value the operand names. A division wants its dividend in `rax`, so `rax` is
//! unavailable to every other value that is live where the division reads, and it is the first
//! register offered to the dividend itself. When the dividend gets it there is no move on the way
//! in, and when it does not the rewrite writes one and nothing else changes.
//!
//! That second half is the hint, and without it the register an instruction insists on is the one
//! register the value in it can never have, since the value's own operand is what makes the
//! register look busy. The effect is largest on returns, because a function that gives a value
//! back has an operand fixed to `rax` at the end of it and most functions give a value back.
//!
//! What makes the hint safe is asking about the register at each of the instruction's two points
//! rather than across the whole of it. An instruction reads at the first and writes at the second,
//! so a register it insists on is one value's at the first, another value's at the second, and
//! nobody else's at either. A division reads its dividend from `rax` and writes its quotient to
//! `rax`, and those are different values that can both live there. A value passed to a call in
//! `rdi` and wanted again afterwards cannot, because nothing writes `rdi` at the second point and
//! a register the call does not write is a register the call is assumed to destroy.
//!
//! An operand that has to be in memory is the other way round. The value it names goes on the
//! stack whatever else is true of it, because that is the only place the instruction could read it
//! from.
//!
//! # What it does with a two address instruction
//!
//! An `add` on x86-64 writes one of the registers it reads, which the operand says as a reuse of
//! another operand. The rewrite can always make that true by copying the source into the
//! destination first, but only if the destination is a register the instruction does not otherwise
//! read, so a value written by a reuse is treated here as live from where the instruction reads
//! rather than from where it writes. Then the copy is always safe.
//!
//! The copy is also usually unnecessary, and the one place this looks past the interval it is
//! placing is to see that: if the value being reused is read here for the last time, the value
//! being written may have its register, and the instruction is already two address without
//! anything being moved anywhere. That is the whole of the coalescing this allocator does, and it
//! is worth the dozen lines, because otherwise every piece of arithmetic in the output carries a
//! move in front of it.
//!
//! # What it does not do
//!
//! It does not touch the function. What comes out is a table saying where each value went, and the
//! pass that rewrites the operands and writes the moves reads it. Keeping the decision and the
//! rewrite apart is what lets the decision be checked by looking at it, and it is the shape
//! `spec/10-backend.md` section 10.4 asks for: an allocator is a function from a program to an
//! assignment and the moves that make it true.

use rucc_mir::{Constraint, Func, Operand, Reg, Role};
use rucc_target::{PhysReg, RegClass};

use crate::live::{Live, Range};
use crate::order::{Order, Point};

/// Where a value lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// In a register, for the whole of its range.
    Reg(PhysReg),
    /// In a slot of the frame, which is what a value the allocator ran out of registers for gets,
    /// and what a value an instruction can only read from memory gets.
    Slot(u32),
}

/// What the allocator is allowed to use.
///
/// The order is the calling convention's, because which register to hand out first follows from
/// which ones a call destroys, and `rucc-target` is where a convention says so. The scratch
/// registers are held back out of the order and are what a spilled value is read into at each
/// instruction that wants it, so a class needs as many of them as one of its instructions has
/// register operands. Nothing here uses them, since a spilled value is only read once the rewrite
/// is writing the instruction that reads it, but they are held back here because this is what
/// decides what everything else may have.
#[derive(Debug, Clone, Default)]
pub struct Env {
    classes: Vec<Class>,
}

/// What one class of registers offers.
#[derive(Debug, Clone, Default)]
struct Class {
    order: Vec<PhysReg>,
    scratch: Vec<PhysReg>,
}

impl Env {
    /// An environment offering nothing, which is what a target that has said nothing offers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same environment, with that class described.
    #[must_use]
    pub fn with(mut self, class: RegClass, order: &[PhysReg], scratch: &[PhysReg]) -> Self {
        let index = usize::from(class.number());
        if self.classes.len() <= index {
            self.classes.resize(index + 1, Class::default());
        }
        self.classes[index] = Class { order: order.to_vec(), scratch: scratch.to_vec() };
        self
    }

    /// The registers it may hand out in a class, in the order it prefers them.
    #[must_use]
    pub fn order(&self, class: RegClass) -> &[PhysReg] {
        self.classes.get(usize::from(class.number())).map_or(&[], |class| &class.order)
    }

    /// The registers held back in a class for reading a spilled value into.
    #[must_use]
    pub fn scratch(&self, class: RegClass) -> &[PhysReg] {
        self.classes.get(usize::from(class.number())).map_or(&[], |class| &class.scratch)
    }
}

/// Where every value in a function went.
#[derive(Debug, Clone)]
pub struct Assignment {
    places: Vec<Option<Place>>,
    slots: Vec<RegClass>,
}

impl Assignment {
    /// An assignment that says nothing yet about a function with that many values.
    ///
    /// This and [`Assignment::put`] and [`Assignment::take_slot`] are how an allocator says what
    /// it decided. There will be a second one in M4 and it will not reach its answer this way, so
    /// what an assignment is has to be separable from how this file arrives at one, and the
    /// checker in [`crate::check`] reads an assignment without caring which allocator wrote it.
    #[must_use]
    pub fn empty(vregs: usize) -> Self {
        Self { places: vec![None; vregs], slots: Vec::new() }
    }

    /// Records where a value went.
    ///
    /// # Panics
    ///
    /// Panics on a physical register, which is somewhere already, and on a virtual one the
    /// function never handed out.
    pub fn put(&mut self, reg: Reg, place: Place) {
        self.places[index(reg)] = Some(place);
    }

    /// Takes a slot of the frame, of that class, and gives back which one it is.
    ///
    /// # Panics
    ///
    /// Panics past four billion slots, which is a frame no machine has room for.
    pub fn take_slot(&mut self, class: RegClass) -> u32 {
        let slot = u32::try_from(self.slots.len()).expect("too many spilled values");
        self.slots.push(class);
        slot
    }

    /// Where a value lives, or `None` for a virtual register this function never mentions and for
    /// a physical one, which is already where it is.
    #[must_use]
    pub fn place(&self, reg: Reg) -> Option<Place> {
        self.places.get(usize::try_from(reg.number()?).ok()?).copied().flatten()
    }

    /// The class of each slot of the frame, which is what says how wide it has to be.
    #[must_use]
    pub fn slots(&self) -> &[RegClass] {
        &self.slots
    }

    /// How many values went to the stack.
    #[must_use]
    pub fn spilled(&self) -> usize {
        self.slots.len()
    }

    /// Puts a value on the stack, in a slot of its own.
    fn spill(&mut self, reg: Reg, class: RegClass) {
        let slot = self.take_slot(class);
        self.put(reg, Place::Slot(slot));
    }
}

/// One value waiting for a place.
#[derive(Debug, Clone, Copy)]
struct Interval {
    reg: Reg,
    class: RegClass,
    range: Range,
}

/// One value that has a register, for as long as it still wants it.
#[derive(Debug, Clone, Copy)]
struct Held {
    reg: Reg,
    class: RegClass,
    range: Range,
    at: PhysReg,
}

/// A register an instruction insists on, and where it insists on it.
#[derive(Debug, Clone, Copy)]
struct Blocked {
    class: RegClass,
    at: PhysReg,
    /// One of the instruction's two points. Every register an instruction insists on has an entry
    /// at each of them, because a register held at one of the two is a register nothing else may
    /// be in across the instruction.
    point: Point,
    /// The one value that may be in it there, which is the value of an operand the instruction
    /// reads at that point or writes at it. `None` means nothing may: an operand naming a physical
    /// register outright claims it against everything, and a point no operand covers is a point
    /// the instruction has the register to itself at.
    by: Option<Reg>,
}

/// A value written into the register another operand of the same instruction was read from.
#[derive(Debug, Clone, Copy)]
struct Reuse {
    /// The value being read, which is the one whose register would do.
    source: Reg,
    /// Where the instruction reads it.
    at: Point,
}

/// Decides where every value in a function lives.
///
/// # Panics
///
/// Panics if a class has no registers to hand out and something in the function is in that class,
/// since that is a target description that does not describe the target the function is for.
#[must_use]
pub fn assign(func: &Func, order: &Order, live: &Live, env: &Env) -> Assignment {
    let blocked = blocked(func, order);
    let forced = forced(func);
    let reuses = reuses(func, order);
    let hints = hints(func);

    let mut intervals = Vec::with_capacity(func.vregs());
    for (number, reuse) in reuses.iter().enumerate() {
        let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
        let (Some(mut range), Some(class)) = (live.range(reg), func.class_of(reg)) else {
            continue;
        };
        if let Some(reuse) = reuse {
            range.start = range.start.min(reuse.at);
        }
        intervals.push(Interval { reg, class, range });
    }
    intervals.sort_by_key(|interval| (interval.range.start, interval.reg));

    let mut assignment = Assignment::empty(func.vregs());
    let mut active: Vec<Held> = Vec::new();
    for interval in intervals {
        active.retain(|held| held.range.end >= interval.range.start);
        if forced.contains(&interval.reg) {
            assignment.spill(interval.reg, interval.class);
            continue;
        }
        // A class with no order is one the target says nothing allocates from, which on x86-64 is
        // the x87 stack. A value of such a class is a mistake at the point it was made rather than
        // a value with nowhere to go: what the target means is that the value lives in memory and
        // that whatever operates on it takes an address. See `ClassInfo::allocatable`.
        assert!(
            !env.order(interval.class).is_empty(),
            "a value in class {}, which the target hands out no registers from",
            interval.class.number()
        );
        let two_address = reuses[index(interval.reg)]
            .and_then(|reuse| coalesce(&assignment, &active, &blocked, interval, reuse));
        // The reuse comes first, because a two address instruction that has to copy its left
        // operand in pays for the copy whatever the hint says, and taking the hint here would buy
        // one move at the cost of another.
        let hinted = hints[index(interval.reg)].filter(|&at| {
            env.order(interval.class).contains(&at)
                && available(&active, &blocked, interval, at, None)
        });
        let chosen = two_address.or(hinted).or_else(|| {
            env.order(interval.class)
                .iter()
                .copied()
                .find(|&at| available(&active, &blocked, interval, at, None))
        });
        match chosen {
            Some(at) => {
                assignment.places[index(interval.reg)] = Some(Place::Reg(at));
                let reg = interval.reg;
                active.push(Held { reg, class: interval.class, range: interval.range, at });
            }
            None => spill_one(&mut assignment, &mut active, &blocked, interval),
        }
    }
    assignment
}

/// Whether a register is one this interval could have.
///
/// The exception is the value a reuse is coalescing with, which holds the register right up to the
/// point the new value takes it over and is the one thing that may overlap.
fn available(
    active: &[Held],
    blocked: &[Blocked],
    interval: Interval,
    at: PhysReg,
    except: Option<Reg>,
) -> bool {
    let taken = active
        .iter()
        .any(|held| held.at == at && held.class == interval.class && Some(held.reg) != except);
    let insisted = blocked.iter().any(|one| {
        one.at == at
            && one.class == interval.class
            && one.by != Some(interval.reg)
            && interval.range.covers(one.point)
    });
    !taken && !insisted
}

/// The register the value being reused is in, when this instruction is the last thing that reads
/// it and the register is otherwise free.
fn coalesce(
    assignment: &Assignment,
    active: &[Held],
    blocked: &[Blocked],
    interval: Interval,
    reuse: Reuse,
) -> Option<PhysReg> {
    let Some(Place::Reg(at)) = assignment.place(reuse.source) else { return None };
    let source = active.iter().find(|held| held.reg == reuse.source)?;
    // A value read again later needs its register after this instruction would have overwritten
    // it, so the two really do have to be different and the rewrite really does have to copy.
    let dies = source.range.end == reuse.at;
    (dies && available(active, blocked, interval, at, Some(reuse.source))).then_some(at)
}

/// Sends one value to the stack: the one wanted for longest, since its register pays for itself
/// over the most instructions.
fn spill_one(
    assignment: &mut Assignment,
    active: &mut Vec<Held>,
    blocked: &[Blocked],
    interval: Interval,
) {
    // A value whose register the instructions in the way insist on for themselves is no use as a
    // victim, because taking it over would put this value in a register it may not have.
    let victim = active
        .iter()
        .enumerate()
        .filter(|(_, held)| held.class == interval.class)
        .filter(|(_, held)| available(&[], blocked, interval, held.at, None))
        .max_by_key(|(_, held)| held.range.end)
        .map(|(at, held)| (at, held.at, held.range.end));
    match victim {
        Some((victim, at, end)) if end > interval.range.end => {
            let held = active.remove(victim);
            assignment.spill(held.reg, held.class);
            assignment.places[index(interval.reg)] = Some(Place::Reg(at));
            let reg = interval.reg;
            active.push(Held { reg, class: interval.class, range: interval.range, at });
        }
        _ => assignment.spill(interval.reg, interval.class),
    }
}

/// The registers the instructions insist on, and where.
///
/// A physical register an operand names outright counts the same way. Nothing before allocation
/// writes one except an instruction that has to, and it has to for the length of that one
/// instruction, which is the same statement a fixed constraint makes.
fn blocked(func: &Func, order: &Order) -> Vec<Blocked> {
    let mut blocked = Vec::new();
    let mut claimed: Vec<(RegClass, PhysReg)> = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            let operands = &func[func[inst].operands];
            claimed.clear();
            for operand in operands {
                if let Some(at) = insisted(operand) {
                    let key = (operand.class, at);
                    if !claimed.contains(&key) {
                        claimed.push(key);
                    }
                }
            }
            for &(class, at) in &claimed {
                // Both points, whether or not an operand is at them. A register an instruction
                // reads and does not write is destroyed by the time the instruction is done as far
                // as anything here knows, which is what stops the value a call is passed in `rdi`
                // from staying in `rdi` over the call.
                for (point, role) in [(order.early(inst), Role::Use), (order.late(inst), Role::Def)]
                {
                    let mut named = false;
                    for operand in operands {
                        let mine = insisted(operand) == Some(at) && operand.class == class;
                        if !mine || !(operand.role == role || operand.role == Role::EarlyDef) {
                            continue;
                        }
                        named = true;
                        let by = operand.reg.is_virtual().then_some(operand.reg);
                        blocked.push(Blocked { class, at, point, by });
                    }
                    if !named {
                        blocked.push(Blocked { class, at, point, by: None });
                    }
                }
            }
        }
    }
    blocked
}

/// The register an operand has to be in, which is the one a constraint asks for or the one the
/// operand names outright.
fn insisted(operand: &Operand) -> Option<PhysReg> {
    match operand.constraint {
        Constraint::Fixed(at) => Some(at),
        _ => operand.reg.phys(),
    }
}

/// The register each value would rather be in, which is the one an operand naming it insists on.
///
/// A value with two of them keeps the first the function writes down, which is the definition when
/// there is one, since a value written into a fixed register and then moved somewhere else pays
/// for the move at the top of its life rather than at the bottom. Two different fixed registers on
/// one value is rare enough that the second is not worth carrying a list for.
fn hints(func: &Func) -> Vec<Option<PhysReg>> {
    let mut hints = vec![None; func.vregs()];
    for block in func.blocks() {
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                let Constraint::Fixed(at) = operand.constraint else { continue };
                let number = operand.reg.number().and_then(|number| usize::try_from(number).ok());
                let Some(number) = number else { continue };
                if func.class_of(operand.reg) == Some(operand.class) && hints[number].is_none() {
                    hints[number] = Some(at);
                }
            }
        }
    }
    hints
}

/// The values that have to be on the stack whatever else is true of them.
fn forced(func: &Func) -> Vec<Reg> {
    let mut forced = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                if operand.constraint == Constraint::Stack
                    && operand.reg.is_virtual()
                    && !forced.contains(&operand.reg)
                {
                    forced.push(operand.reg);
                }
            }
        }
    }
    forced
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

/// A virtual register's number as a table index.
fn index(reg: Reg) -> usize {
    usize::try_from(reg.number().expect("a virtual register")).expect("a register number")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Opcode, Operand};
    use rucc_target::x86_64::{GPR, R13, R14, R15, RAX, RCX, RDX, REGS, SYSV};

    use super::*;

    /// The x86-64 environment, with the last three of the allocation order held back as scratch.
    fn env() -> Env {
        let (order, scratch) = SYSV.int_order.split_at(SYSV.int_order.len() - 3);
        Env::new().with(GPR, order, scratch)
    }

    /// An environment with that many general purpose registers, for putting a function under
    /// pressure without writing a hundred instructions.
    fn narrow(count: usize) -> Env {
        Env::new().with(GPR, &SYSV.int_order[..count], &SYSV.int_order[count..count + 1])
    }

    /// What a place is called, which is what an assertion reads.
    fn named(place: Option<Place>) -> String {
        match place {
            Some(Place::Reg(reg)) => REGS.name(GPR, reg).expect("a register").to_string(),
            Some(Place::Slot(slot)) => format!("slot {slot}"),
            None => "nowhere".to_string(),
        }
    }

    /// Where every value in a function went.
    fn places(func: &Func, env: &Env) -> Vec<String> {
        let order = Order::of(func);
        let live = Live::of(func, &order);
        let assignment = assign(func, &order, &live, env);
        (0..func.vregs())
            .map(|number| {
                let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
                named(assignment.place(reg))
            })
            .collect()
    }

    #[test]
    fn two_values_that_are_never_both_wanted_share_a_register() {
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

        // The first register in the order, twice, because the first value is finished with before
        // the second one is written.
        assert_eq!(places(&func, &env()), ["rax", "rax"]);
    }

    #[test]
    fn two_values_that_are_both_wanted_do_not() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(first, GPR).finish();
        func.build(block, opcode).uses(second, GPR).finish();

        assert_eq!(places(&func, &env()), ["rax", "rcx"]);
    }

    #[test]
    fn a_value_written_early_that_nothing_reads_still_holds_its_register() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let wanted = func.new_vreg(GPR);
        let spare = func.new_vreg(GPR);
        // A division: a remainder somebody wants, and a quotient nobody does. Both are written by
        // the one instruction and the quotient is written before the operands have been read.
        func.build(block, opcode)
            .def(wanted, GPR)
            .operand(Operand::write_early(spare, GPR))
            .finish();
        func.build(block, opcode).uses(wanted, GPR).finish();

        // Two registers, not one. A value nothing reads is still somewhere, and the instruction
        // that wrote it wrote the other one too, so the two cannot be the same place. Handing them
        // the same register loses the remainder, because the copy that takes the quotient out of
        // the register the machine insisted on goes on top of it. The quotient gets the first
        // register because it is written first, which is the whole of what early means.
        assert_eq!(places(&func, &env()), ["rcx", "rax"]);
    }

    #[test]
    fn the_value_wanted_longest_is_the_one_that_goes_to_the_stack() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let long = func.new_vreg(GPR);
        let short = func.new_vreg(GPR);
        let third = func.new_vreg(GPR);
        func.build(block, opcode).def(long, GPR).finish();
        func.build(block, opcode).def(short, GPR).finish();
        func.build(block, opcode).def(third, GPR).finish();
        func.build(block, opcode).uses(short, GPR).finish();
        func.build(block, opcode).uses(third, GPR).finish();
        func.build(block, opcode).uses(long, GPR).finish();

        // Two registers between three values. The one still wanted at the end of the function is
        // the one whose register is worth the most to everybody else, so it is the one that goes.
        assert_eq!(places(&func, &narrow(2)), ["slot 0", "rcx", "rax"]);
    }

    #[test]
    fn a_register_an_instruction_insists_on_goes_to_the_values_that_asked_for_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let across = func.new_vreg(GPR);
        let dividend = func.new_vreg(GPR);
        let quotient = func.new_vreg(GPR);
        let remainder = func.new_vreg(GPR);
        func.build(block, opcode).def(across, GPR).finish();
        func.build(block, opcode).def(dividend, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(quotient, GPR).with(Constraint::Fixed(RAX)))
            .operand(Operand::write_early(remainder, GPR).with(Constraint::Fixed(RDX)))
            .operand(Operand::read(dividend, GPR).with(Constraint::Fixed(RAX)))
            .finish();
        func.build(block, opcode).uses(across, GPR).finish();

        // The value that has to be across the division is nowhere near `rax` or `rdx`, and each of
        // the three the division names is in the register the division asked for it in. The
        // dividend and the quotient share `rax` because the first is read where the second is
        // written, which is what a division does.
        assert_eq!(places(&func, &env()), ["rcx", "rax", "rax", "rdx"]);
    }

    #[test]
    fn a_value_wanted_after_the_instruction_that_insists_does_not_get_that_register() {
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
        func.build(block, opcode).uses(dividend, GPR).finish();

        // The hint is a preference and not a claim. The dividend would rather be in `rax` and
        // cannot be, because the division writes `rax` and the dividend is wanted afterwards, so
        // it takes the next register and the quotient keeps the one it was promised.
        assert_eq!(places(&func, &env()), ["rcx", "rax"]);
    }

    #[test]
    fn a_value_an_instruction_can_only_read_from_memory_is_on_the_stack() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let value = func.new_vreg(GPR);
        func.build(block, opcode).def(value, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::read(value, GPR).with(Constraint::Stack))
            .finish();

        assert_eq!(places(&func, &env()), ["slot 0"]);
    }

    #[test]
    fn a_two_address_instruction_writes_the_register_it_read_when_it_can() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, opcode).def(left, GPR).finish();
        func.build(block, opcode).def(right, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, opcode).uses(right, GPR).finish();

        // The addition reads the left value for the last time, so the answer goes where that was
        // and the instruction is two address without a move in front of it.
        assert_eq!(places(&func, &env()), ["rax", "rcx", "rax"]);
    }

    #[test]
    fn a_two_address_instruction_that_cannot_gets_a_register_nothing_it_reads_is_in() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, opcode).def(left, GPR).finish();
        func.build(block, opcode).def(right, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, opcode).uses(left, GPR).finish();

        // The left value is wanted afterwards, so the answer cannot have its register. It cannot
        // have the right one's either, because the rewrite is about to write a move into it before
        // the addition has read anything.
        assert_eq!(places(&func, &env()), ["rax", "rcx", "rdx"]);
    }

    #[test]
    fn a_value_live_across_a_whole_loop_holds_its_register_over_all_of_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let body = func.create_block();
        let carried = func.new_vreg(GPR);
        let inside = func.new_vreg(GPR);
        func.build(head, opcode).def(carried, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(body)];
        func.build(body, opcode).def(inside, GPR).finish();
        func.build(body, opcode).uses(inside, GPR).uses(carried, GPR).finish();
        *func.succs_mut(body) = vec![BlockCall::to(body)];

        // The value inside the loop cannot have the carried one's register, even though nothing
        // between the two definitions says so.
        assert_eq!(places(&func, &env()), ["rax", "rcx"]);
    }

    #[test]
    fn a_frame_says_what_each_of_its_slots_is_for() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let assignment = assign(&func, &order, &live, &narrow(1));
        assert_eq!(assignment.spilled(), 1);
        assert_eq!(assignment.slots(), [GPR]);
        // A register that is already a register is where it is, and this has nothing to say about
        // it.
        assert_eq!(assignment.place(Reg::physical(RCX)), None);
        assert_eq!(env().scratch(GPR), [R13, R14, R15]);
    }
}
