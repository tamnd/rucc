//! What an instruction needs around it for the machine to accept the places its operands were
//! given.
//!
//! Design: `spec/optimizer/39-register-allocation.md` section 39.7, the legalization phase, and
//! tamnd/rucc#1177.
//!
//! The assignment says where every value lives, and that is not yet something the machine will
//! run. A value on the stack has to be read into a register before an instruction that wants it in
//! one, and an answer written into a register has to be stored to the slot it lives in. An operand
//! the instruction insists on having in one register has to be moved there and back when its
//! value lives in another. A two address instruction writes one of the registers it reads, so the
//! value it reads has to be copied into the register the answer lives in first when the two are
//! not already the same. This works all of that out for one instruction at a time and hands back
//! the operands as the machine will read them and the moves either side, in the order they have to
//! be made in. [`crate::rewrite`] files them, and writes the operands into the function.
//!
//! Keeping it apart from the rewrite is what lets it be asked about one instruction and checked
//! against what comes back, rather than only through the function the whole rewrite produces.
//!
//! # How many scratch registers one instruction wants
//!
//! Two of a class, and a target holds two of each back for exactly this. The instruction that asks
//! for most reads two values and writes a third with nothing of the three in a register, and the
//! arithmetic works out because the two reads are what use the two scratch registers and the answer
//! is written back into one of them. Writing over it destroys nothing, since it holds a copy of a
//! value whose home is a stack slot and the instruction has already read it, and the answer is
//! stored away from it afterwards. Giving the answer a scratch register of its own would want a
//! third, which a program with enough live values around a call reaches, and that was issue #350.
//!
//! Which register the answer goes back into depends on what wrote it. A two address instruction
//! writes the register the operand it reuses was read into, because that is what two address means.
//! A three address one, which is `lea` and the compare and set pairs, writes a register that is
//! none of its operands, and there the answer takes the first scratch register of the class again:
//! the reads are done by the time the write happens, so the two uses of that register do not meet.
//! Counting the two jobs in one running number is what made a three address instruction with every
//! end on the stack ask for a third register and abort, which was issue #726.
//!
//! It is only a scratch register the answer may have either way. Where the operand a two address
//! instruction reuses is in a register the assignment gave out, the value in it may be wanted after
//! the instruction, and the assignment only lets one be written over when it is not, which it says
//! by giving the answer that register in the first place. So the answer takes a scratch register
//! there and the two address copy fills it. That one is filled in front of the instruction rather
//! than by it, so it cannot share with a read, and the count still comes to two, because an operand
//! that is in a register is not holding a scratch register.
//!
//! Deciding either way needs to know where the operand it reuses went, so an operand that reuses
//! another and has no register of its own is placed in a second pass over the operands.
//!
//! The count is per class. An instruction reading a spilled value out of each of two files wants
//! the first register of each, since a class holds its own back and nothing on the instruction is
//! in the other's.
//!
//! # What happens when two is not enough after all
//!
//! Two runs out on an instruction that reads three registers and writes none, because then there is
//! no answer to fold back into a register an operand arrived in and the arithmetic above has nothing
//! to work on. The instruction that does this on x86-64 is the indexed store, whose base, index and
//! value are three registers it only reads, and at `-O0` all three of them can be stack slots. That
//! is tamnd/rucc#913, and it stopped brotli and cmocka on the first file that held one.
//!
//! What answers it is borrowing: a register of the class the instruction has not named is
//! taken, whatever is in it is put in a slot of the frame in front of the instruction, and it is
//! brought back behind it. That asks nothing at all of the register, so it does not matter whether
//! the value in it is wanted afterwards, whether the callee owes it back, or whether an argument
//! travels in it, which are the three things that make a register held back hard to find. A third
//! register held back would cost every function in the program one, and on x86-64 the only one
//! available is `rax`, which is the return value, so the bill would be a move at every return. This
//! costs two memory accesses at the one instruction that wanted it and a slot most functions never
//! take.
//!
//! # What a fixed register turns into
//!
//! A move each way. The assignment deliberately gave the value some other register, so a division
//! whose dividend has to be in `rax` gets a move into `rax` in front of it and a move out of `rax`
//! behind it. That is the cost of the rule the assignment follows, and it is the rule that keeps
//! the `-O0` allocator one pass.
//!

use rucc_mir::{Constraint, Func, Inst, Operand, Reg, Role};
use rucc_target::{PhysReg, RegClass};

use crate::assign::{Assignment, Env, Place};
use crate::moves::Move;

/// What one instruction becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Legal {
    /// The operands, each naming the physical register the instruction reads or writes it in.
    pub operands: Vec<Operand>,
    /// The moves in front of the instruction, with the class of each, in the order they are made.
    pub before: Vec<(Move<Place>, RegClass)>,
    /// The moves behind it, the same way.
    pub after: Vec<(Move<Place>, RegClass)>,
}

/// Works out what one instruction's operands are rewritten to and what has to happen either side of
/// it for the machine to accept the places the assignment gave them.
///
/// The assignment is taken by reference and may gain a slot, which is where a register borrowed at
/// an instruction with more spilled operands than the class holds registers back for waits.
/// `spare` is the function's list of those slots, shared by every instruction in it.
///
/// # Panics
///
/// Panics on an instruction naming every register of a class at once, which leaves nothing to
/// borrow, and on an operand whose register the assignment says nothing about and which is not a
/// physical register either.
#[must_use]
pub fn instruction(
    func: &Func,
    assignment: &mut Assignment,
    env: &Env,
    spare: &mut Spare,
    inst: Inst,
) -> Legal {
    let list = func[inst].operands;
    let mut operands: Vec<Operand> = func[list].to_vec();
    let mut before = Moves::new();
    let mut after = Moves::new();
    let mut taken = Taken::new();

    // Where the assignment put each operand's value, taken before anything is rewritten, since
    // rewriting an operand is what loses that. The second pass below reads it.
    let places: Vec<Place> =
        operands.iter().map(|operand| place(assignment, operand.reg)).collect();

    // A spilled operand that reuses another is left for the second pass, because where it goes
    // depends on where the operand it reuses went and that is not known until every operand ahead
    // of it has been placed.
    let mut reusing: Vec<usize> = Vec::new();

    // Every register the instruction has named for itself, which is one an operand's value is
    // already in and one a fixed constraint asked for. Taken before anything is rewritten, for the
    // same reason the places above are: rewriting is what turns an operand's register into a
    // physical one and loses which of the two it was.
    let mut claimed = Claimed::default();
    for (operand, place) in operands.iter().zip(&places) {
        if let Place::Reg(at) = *place {
            claimed.named(operand, at);
        }
        if let Constraint::Fixed(at) = operand.constraint {
            claimed.named(operand, at);
        }
    }
    let mut scratch = Scratch::new(env, assignment, spare, claimed);

    for (index, operand) in operands.iter_mut().enumerate() {
        let fixed = match operand.constraint {
            Constraint::Fixed(at) => Some(at),
            _ => None,
        };
        let at = match (place(scratch.assignment, operand.reg), fixed) {
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
                // Which of the two jobs this register is for. An operand the instruction only
                // writes wants one from the instruction onwards, and an operand it reads wants one
                // from before the instruction until it reads it, so the same register does both
                // and the two are counted apart.
                let at = match fixed {
                    Some(fixed) => fixed,
                    None if operand.role.is_def() => {
                        taken.written_into(operand.class, &mut scratch)
                    }
                    None => taken.read_into(operand.class, &mut scratch),
                };
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
        // Either way this shape wants two of the class and no more. If the operand it reuses is on
        // the stack then it is holding one of them already, and if it is not then it is not
        // holding one at all.
        //
        // This one is asked for as a read even though the instruction writes it, because the copy
        // that fills it goes in front of the instruction. It is live from there, which is the same
        // span a value read in off the stack is live for, so it cannot share with one.
        let other = usize::from(other);
        let at = match places[other] {
            Place::Slot(_) => phys(operands[other].reg),
            Place::Reg(_) => taken.read_into(operands[index].class, &mut scratch),
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

    // A borrowed register is put away in front of everything else and brought back behind
    // everything else, since what happens in between is the instruction using it and the moves
    // that carry its operands in and out. Nothing borrowed at one instruction is still borrowed at
    // the next, which is what lets the slot be shared.
    //
    // An instruction that borrowed nothing has no saves, and then the moves in front are the ones
    // already made. Putting them behind an empty list of saves would copy them into a new one.
    let (saves, restores) = scratch.finish();

    let first = if saves.is_empty() {
        before
    } else {
        let mut first = saves;
        first.extend(before);
        first
    };
    let mut last = after;
    last.extend(restores);
    Legal { operands, before: first, after: last }
}

/// How many scratch registers of each class one instruction has been handed, in each of the two
/// jobs they do.
///
/// Counted per class rather than in one running number, because the classes hold their own back
/// and an instruction reading a spilled value out of each of two files would otherwise skip the
/// first register of the second file for no reason.
///
/// Counted per job as well, and that is the part that keeps the count down. A register a spilled
/// value is read into is live from in front of the instruction until the instruction reads it. A
/// register the instruction writes its answer into is live from the instruction until the store
/// behind it. Those two spans do not meet, so one register does both jobs and the counting starts
/// again rather than carrying on. What that rests on is the machine reading its operands before it
/// writes its answer, which is true of every instruction the backends here emit and is the same
/// thing that makes `addq %rax, %rax` mean what it looks like.
///
/// Where the count runs out is an instruction that reads three registers and writes none, because
/// then there is no answer to fold back into a register an operand arrived in and the trick above
/// has nothing to work on. On x86-64 that instruction is the indexed store, whose base, index and
/// value are three registers it only reads, and at `-O0` all three of them can be stack slots. That
/// is tamnd/rucc#913, and what answers it is [`Scratch::borrow`] rather than a third register held
/// back, since holding a third back costs every function a register and this costs only the
/// instruction that wanted one.
#[derive(Debug, Default)]
struct Taken {
    /// How many of each class hold a value read in ahead of the instruction.
    read: Vec<usize>,
    /// How many of each class hold an answer the instruction writes.
    written: Vec<usize>,
}

impl Taken {
    /// Nothing handed out yet.
    fn new() -> Self {
        Self::default()
    }

    /// A register of a class for a value read in ahead of the instruction.
    fn read_into(&mut self, class: RegClass, scratch: &mut Scratch<'_>) -> PhysReg {
        Self::take(&mut self.read, class, scratch, Role::Use)
    }

    /// A register of a class for an answer the instruction writes.
    fn written_into(&mut self, class: RegClass, scratch: &mut Scratch<'_>) -> PhysReg {
        Self::take(&mut self.written, class, scratch, Role::Def)
    }

    /// The next register of a class out of one of the two counts, passing over any the instruction
    /// has already named itself for a value travelling the same way and borrowing one when the held
    /// back ones run out.
    ///
    /// An operand with a fixed constraint names a register the instruction has to have its value
    /// in, and the move that puts it there is in the same list as the move that would fill a
    /// scratch register. So handing the same register out for both would lose one of the two
    /// values, quietly and at run time. It is passed over instead.
    ///
    /// Which way the value travels is what decides whether there is a clash at all, and [`Claimed`]
    /// says why. A register the instruction only writes is free to carry a value in, which is what a
    /// call wants: a call names every caller saved register as one it writes, and those are the very
    /// registers held back for scratch.
    ///
    /// A clash comes up on a machine where a register held back is one an instruction can also
    /// insist on, and on x86-64 the way in is inline assembly naming `r10` or `r11`.
    fn take(
        counts: &mut Vec<usize>,
        class: RegClass,
        scratch: &mut Scratch<'_>,
        role: Role,
    ) -> PhysReg {
        let index = usize::from(class.number());
        if counts.len() <= index {
            counts.resize(index + 1, 0);
        }
        let held: &[PhysReg] = scratch.env.scratch(class);
        while held.get(counts[index]).is_some_and(|&reg| scratch.claimed.clashes(role, class, reg))
        {
            counts[index] += 1;
        }
        if let Some(&at) = held.get(counts[index]) {
            counts[index] += 1;
            return at;
        }
        scratch.borrow(class)
    }
}

/// The registers the instruction has named for itself, which scratch has to work around.
///
/// A register is kept with the class it was named in, because a register number is only a number
/// into one file and the same one means a different register in another: a call names sixteen vector
/// registers numbered nought to fifteen and sixteen general purpose ones numbered the same, and
/// reading the two lists as one leaves the general purpose file looking entirely spoken for.
///
/// Reading and writing are kept apart because they clash with different things. A register a value
/// arrives in is one no move in front of the instruction may write, and a register an answer leaves
/// in is one no move behind it may write. A call is the case that makes the difference matter: it
/// names every caller saved register as one it writes, `r10` and `r11` among them, and an indirect
/// call through a pointer on the stack has to read that pointer into one of exactly those two.
#[derive(Debug, Default)]
struct Claimed {
    /// The registers a value arrives in, with the class each was named in.
    reads: Vec<(RegClass, PhysReg)>,
    /// The registers an answer leaves in, with the class each was named in.
    writes: Vec<(RegClass, PhysReg)>,
}

impl Claimed {
    /// Records a register an operand named, on the side its value travels.
    fn named(&mut self, operand: &Operand, at: PhysReg) {
        self.side_mut(operand.role).push((operand.class, at));
    }

    /// Records a register nothing may be handed for the rest of the instruction, which is one
    /// [`Scratch::borrow`] has just taken.
    fn taken(&mut self, class: RegClass, at: PhysReg) {
        self.reads.push((class, at));
        self.writes.push((class, at));
    }

    /// Whether handing that register out for a value travelling that way would lose a value.
    fn clashes(&self, role: Role, class: RegClass, at: PhysReg) -> bool {
        self.side(role).contains(&(class, at))
    }

    /// Whether the instruction names that register at all, which is what borrowing has to keep off:
    /// what is borrowed is put back behind the instruction, over anything left there.
    fn names(&self, class: RegClass, at: PhysReg) -> bool {
        self.reads.contains(&(class, at)) || self.writes.contains(&(class, at))
    }

    /// The list for values travelling that way. The lists are one instruction's long, so a scan
    /// beats a set.
    fn side(&self, role: Role) -> &Vec<(RegClass, PhysReg)> {
        if role.is_def() { &self.writes } else { &self.reads }
    }

    /// The same, to write to.
    fn side_mut(&mut self, role: Role) -> &mut Vec<(RegClass, PhysReg)> {
        if role.is_def() { &mut self.writes } else { &mut self.reads }
    }
}

/// Moves waiting to be filed, each with the class of the value it moves.
///
/// The class travels with the move because an [`Edit`] carries one and the consumer needs it to pick
/// the instruction that does the move, and by the time a move is filed the operand it came from is
/// out of reach.
type Moves = Vec<(Move<Place>, RegClass)>;

/// The frame slots a borrowed register's value waits in, one list per class.
///
/// They belong to the function rather than to an instruction, because a borrowed register is given
/// back before the next instruction starts and the slot is dead in between, so one slot serves
/// every instruction in the function that borrows. Most functions never take one at all.
pub type Spare = Vec<Vec<u32>>;

/// What it takes to hand a register to one instruction.
///
/// It is a struct rather than four arguments because [`Scratch::borrow`] writes to all of them at
/// once: it reads the environment, takes a slot off the assignment, remembers the register so a
/// second borrow at the same instruction does not land on it, and files the two moves that make it
/// safe.
struct Scratch<'a> {
    env: &'a Env,
    /// Where every value went, and where a slot for a borrowed register comes from.
    assignment: &'a mut Assignment,
    /// The function's slots for borrowed registers, reused at every instruction.
    spare: &'a mut Spare,
    /// Every register the instruction has named, and then every one borrowed here as it is borrowed.
    claimed: Claimed,
    /// How many of each class have been borrowed at this instruction, which says which slot the
    /// next one uses.
    borrowed: Vec<usize>,
    /// The moves that put a borrowed register's value away, which go in front of everything else.
    saves: Moves,
    /// The moves that bring it back, which go behind everything else.
    restores: Moves,
}

impl<'a> Scratch<'a> {
    /// Nothing borrowed yet at an instruction claiming those registers.
    fn new(
        env: &'a Env,
        assignment: &'a mut Assignment,
        spare: &'a mut Spare,
        claimed: Claimed,
    ) -> Self {
        Self {
            env,
            assignment,
            spare,
            claimed,
            borrowed: Vec::new(),
            saves: Vec::new(),
            restores: Vec::new(),
        }
    }

    /// A register of the class the instruction is not using, with whatever is in it put away in
    /// front of the instruction and brought back behind it.
    ///
    /// This is what a class runs out to, and it works on any machine because it asks nothing at all
    /// of the register it takes. Whatever was in it is somewhere else for the length of one
    /// instruction, so it does not matter whether that value is wanted afterwards, whether the
    /// callee owes the register back, or whether an argument travels in it, which are the three
    /// things that make a register held back hard to find. What it costs is two memory accesses at
    /// the one instruction that wanted it and one slot of the frame, against a register taken off
    /// every function in the program, and `rucc_codegen::pipeline` says why that trade goes this
    /// way round on x86-64.
    ///
    /// The register is any of the class the instruction has not claimed for itself. A register the
    /// allocator gave a value that is live right across the instruction is as good as an idle one,
    /// which is the whole point of putting the contents away first.
    ///
    /// # Panics
    ///
    /// Panics if the class has no register the instruction has not already claimed, which is an
    /// instruction naming every register of a file at once.
    fn borrow(&mut self, class: RegClass) -> PhysReg {
        let index = usize::from(class.number());
        let at = *self
            .env
            .order(class)
            .iter()
            .find(|&&reg| !self.claimed.names(class, reg))
            .expect("an instruction naming every register of its class at once");

        if self.borrowed.len() <= index {
            self.borrowed.resize(index + 1, 0);
        }
        if self.spare.len() <= index {
            self.spare.resize(index + 1, Vec::new());
        }
        let nth = self.borrowed[index];
        if self.spare[index].len() <= nth {
            let slot = self.assignment.take_slot(class);
            self.spare[index].push(slot);
        }
        let slot = self.spare[index][nth];

        self.borrowed[index] = nth + 1;
        self.claimed.taken(class, at);
        self.saves.push((Move::new(Place::Slot(slot), Place::Reg(at)), class));
        self.restores.push((Move::new(Place::Reg(at), Place::Slot(slot)), class));
        at
    }

    /// The moves either side of the instruction, once every register has been handed out.
    fn finish(self) -> (Moves, Moves) {
        (self.saves, self.restores)
    }
}

/// Files a move in front of the instruction or behind it, and turns it round for a value the
/// instruction writes, since that one travels the other way.
fn push(before: &mut Moves, after: &mut Moves, operand: &Operand, mov: Move<Place>) {
    if operand.role.is_def() {
        after.push((Move::new(mov.from, mov.to), operand.class));
    } else {
        before.push((mov, operand.class));
    }
}

/// Where a register is, whether the allocator put it there or it was already somewhere.
pub(crate) fn place(assignment: &Assignment, reg: Reg) -> Place {
    assignment.place(reg).unwrap_or_else(|| Place::Reg(phys(reg)))
}

/// The physical register a register is, once it has to be one.
pub(crate) fn phys(reg: Reg) -> PhysReg {
    reg.phys().expect("a register the assignment says nothing about and that is not a register")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::Opcode;
    use rucc_target::x86_64::{GPR, RAX, RCX, SYSV};

    use super::*;

    /// Two registers to hand out and two held back after them.
    fn env() -> Env {
        Env::new().with(GPR, &SYSV.int_order[..2], &SYSV.int_order[2..4])
    }

    fn func() -> (Func, Opcode, rucc_mir::Block) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        (func, opcode, block)
    }

    fn legal(func: &Func, assignment: &mut Assignment, inst: Inst) -> Legal {
        instruction(func, assignment, &env(), &mut Spare::new(), inst)
    }

    fn regs(legal: &Legal) -> Vec<PhysReg> {
        legal.operands.iter().map(|operand| phys(operand.reg)).collect()
    }

    #[test]
    fn an_instruction_whose_values_are_where_it_wants_them_needs_nothing() {
        let (mut func, opcode, block) = func();
        let value = func.new_vreg(GPR);
        let inst = func.build(block, opcode).uses(value, GPR).finish();
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(value, Place::Reg(RCX));

        let legal = legal(&func, &mut assignment, inst);
        assert_eq!(regs(&legal), [RCX]);
        assert!(legal.before.is_empty() && legal.after.is_empty());
    }

    #[test]
    fn a_register_the_instruction_insists_on_is_filled_in_front_of_it() {
        let (mut func, opcode, block) = func();
        let value = func.new_vreg(GPR);
        let inst = func
            .build(block, opcode)
            .operand(Operand::read(value, GPR).with(Constraint::Fixed(RAX)))
            .finish();
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(value, Place::Reg(RCX));

        let legal = legal(&func, &mut assignment, inst);
        assert_eq!(regs(&legal), [RAX]);
        assert_eq!(legal.before, [(Move::new(Place::Reg(RAX), Place::Reg(RCX)), GPR)]);
        assert!(legal.after.is_empty());
    }

    #[test]
    fn an_answer_written_where_it_does_not_live_is_taken_away_behind_it() {
        let (mut func, opcode, block) = func();
        let value = func.new_vreg(GPR);
        let inst = func
            .build(block, opcode)
            .operand(Operand::write(value, GPR).with(Constraint::Fixed(RAX)))
            .finish();
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(value, Place::Reg(RCX));

        let legal = legal(&func, &mut assignment, inst);
        assert_eq!(regs(&legal), [RAX]);
        assert!(legal.before.is_empty());
        assert_eq!(legal.after, [(Move::new(Place::Reg(RCX), Place::Reg(RAX)), GPR)]);
    }

    #[test]
    fn a_value_on_the_stack_is_read_into_a_register_held_back() {
        let (mut func, opcode, block) = func();
        let value = func.new_vreg(GPR);
        let inst = func.build(block, opcode).uses(value, GPR).finish();
        let mut assignment = Assignment::empty(func.vregs());
        let slot = assignment.take_slot(GPR);
        assignment.put(value, Place::Slot(slot));

        let legal = legal(&func, &mut assignment, inst);
        let scratch = env().scratch(GPR)[0];
        assert_eq!(regs(&legal), [scratch]);
        assert_eq!(legal.before, [(Move::new(Place::Reg(scratch), Place::Slot(slot)), GPR)]);
    }

    #[test]
    fn a_two_address_answer_apart_from_its_source_is_copied_into_first() {
        let (mut func, opcode, block) = func();
        let source = func.new_vreg(GPR);
        let answer = func.new_vreg(GPR);
        let inst = func
            .build(block, opcode)
            .operand(Operand::write(answer, GPR).with(Constraint::Reuse(1)))
            .uses(source, GPR)
            .finish();
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(source, Place::Reg(RCX));
        assignment.put(answer, Place::Reg(RAX));

        let legal = legal(&func, &mut assignment, inst);
        assert_eq!(regs(&legal), [RAX, RCX]);
        assert_eq!(legal.before, [(Move::new(Place::Reg(RAX), Place::Reg(RCX)), GPR)]);
    }

    #[test]
    fn a_third_value_on_the_stack_borrows_a_register_and_gives_it_back() {
        let (mut func, opcode, block) = func();
        let values: Vec<Reg> = (0..3).map(|_| func.new_vreg(GPR)).collect();
        let build = func.build(block, opcode);
        let inst = values.iter().fold(build, |build, &value| build.uses(value, GPR)).finish();
        let mut assignment = Assignment::empty(func.vregs());
        for &value in &values {
            let slot = assignment.take_slot(GPR);
            assignment.put(value, Place::Slot(slot));
        }

        let legal = legal(&func, &mut assignment, inst);
        let borrowed = regs(&legal)[2];
        assert!(!env().scratch(GPR).contains(&borrowed));
        // The one borrowed is put away first and brought back last, around everything else.
        let spare = Place::Slot(3);
        assert_eq!(legal.before[0], (Move::new(spare, Place::Reg(borrowed)), GPR));
        assert_eq!(legal.after.last(), Some(&(Move::new(Place::Reg(borrowed), spare), GPR)));
        assert_eq!(assignment.spilled(), 4);
    }
}
