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
use crate::live::Live;
use crate::moves::Move;
use crate::order::{Order, Point};

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
    idle: &Idle<'_>,
    inst: Inst,
) -> Legal {
    legalize(func, assignment, env, spare, idle, inst, false)
}

/// Whether [`instruction`] would borrow a register at `inst`, which is whether it takes a slot for
/// one off `spare`.
///
/// It asks the same code rather than a second count of the same thing, and differs only where
/// there is nothing to borrow. [`instruction`] panics there, and this answers yes, since an
/// instruction that wants a register its class has not got wants one all the same. The question
/// is asked of a class that holds one register back, by [`crate::rewrite::fits`], which then
/// allocates the function with two held back, and there it may well find a register free.
pub(crate) fn probe(
    func: &Func,
    assignment: &mut Assignment,
    env: &Env,
    spare: &mut Spare,
    idle: &Idle<'_>,
    inst: Inst,
) -> bool {
    let _ = legalize(func, assignment, env, spare, idle, inst, true);
    spare.iter().any(|slots| !slots.is_empty())
}

/// [`instruction`], and [`probe`] where `probing` is set.
fn legalize(
    func: &Func,
    assignment: &mut Assignment,
    env: &Env,
    spare: &mut Spare,
    idle: &Idle<'_>,
    inst: Inst,
    probing: bool,
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
        match (*place, operand.constraint) {
            (Place::Reg(at), Constraint::Fixed(fixed)) if at != fixed && !operand.role.is_def() => {
                claimed.passed(operand, at);
            }
            (Place::Reg(at), Constraint::Fixed(fixed)) if at != fixed => {
                claimed.landed(operand, at)
            }
            (Place::Reg(at), _) => claimed.named(operand, at),
            _ => {}
        }
        if let Constraint::Fixed(at) = operand.constraint {
            claimed.named(operand, at);
        }
    }
    let mut scratch = Scratch::new(env, assignment, spare, claimed, (idle, inst));
    scratch.probing = probing;

    // The operands already in a register go first, so every move that reads a register the
    // assignment gave out is made before any value is read in off the stack. That is what lets a
    // register an input only passes through on its way to a fixed one be borrowed: by the time
    // something is read into it, what was in it has already gone where the instruction wants it.
    let mut order: Vec<usize> = (0..operands.len()).collect();
    order.sort_by_key(|&index| matches!(places[index], Place::Slot(_)));

    for index in order {
        let operand = &mut operands[index];
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
                    None if operand.role == Role::EarlyDef => {
                        taken.early_into(operand.class, &mut scratch)
                    }
                    None if operand.role.is_def() => {
                        taken.written_into(operand.class, &mut scratch)
                    }
                    None => match answer(&func[list], &places, index, &scratch.claimed) {
                        Some(at) => at,
                        None => taken.read_into(operand.class, &mut scratch),
                    },
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
    // An answer the instruction wrote into the fixed register it asked for is moved on to where it
    // lives, and that register may have been handed out to carry another answer to the stack. So
    // the stores go first and the moves into registers after them. Nothing behind the instruction
    // writes a register except those moves, so no store reads a register they have written.
    let (mut last, landing): (Moves, Moves) =
        after.into_iter().partition(|(mov, _)| matches!(mov.to, Place::Slot(_)));
    last.extend(landing);
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
/// thing that makes `addq %rax, %rax` mean what it looks like. The one exception is an answer
/// written early, such as the RISC-V select, which moves one source into the answer and then
/// reads the other. That answer is live while the reads are, so it counts as one of them.
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

    /// A register of a class for an answer the instruction writes before it has read everything,
    /// which is held from in front of the instruction like a value read in, so it is counted with
    /// those. The answers written late count on from it as well, so that none of them is handed
    /// the same register.
    fn early_into(&mut self, class: RegClass, scratch: &mut Scratch<'_>) -> PhysReg {
        let at = Self::take(&mut self.read, class, scratch, Role::EarlyDef);
        let index = usize::from(class.number());
        if self.written.len() <= index {
            self.written.resize(index + 1, 0);
        }
        self.written[index] = self.written[index].max(self.read[index]);
        at
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
    /// The registers a value arrives in only to be moved on to the fixed register an operand asked
    /// for. They clash the way `reads` do, but one can still be borrowed when nothing else is left,
    /// since the move out of it is made before anything is read into it.
    passed: Vec<(RegClass, PhysReg)>,
    /// The registers an answer is moved into behind the instruction, out of the fixed register its
    /// operand asked for. Nothing is live in one while the instruction runs, so it can carry a value
    /// with nothing put away first.
    landed: Vec<(RegClass, PhysReg)>,
}

impl Claimed {
    /// Records a register an operand named, on the side its value travels.
    fn named(&mut self, operand: &Operand, at: PhysReg) {
        self.side_mut(operand.role).push((operand.class, at));
    }

    /// Records a register a value arrives in and is moved straight out of, into the fixed register
    /// its operand asked for.
    fn passed(&mut self, operand: &Operand, at: PhysReg) {
        self.passed.push((operand.class, at));
    }

    /// Records a register an answer is moved into behind the instruction, out of the fixed register
    /// its operand asked for.
    fn landed(&mut self, operand: &Operand, at: PhysReg) {
        self.landed.push((operand.class, at));
    }

    /// Records a register nothing may be handed for the rest of the instruction, which is one
    /// [`Scratch::borrow`] has just taken.
    fn taken(&mut self, class: RegClass, at: PhysReg) {
        self.reads.push((class, at));
        self.writes.push((class, at));
    }

    /// Whether handing that register out for a value travelling that way would lose a value.
    fn clashes(&self, role: Role, class: RegClass, at: PhysReg) -> bool {
        let early = role == Role::EarlyDef;
        self.side(role).contains(&(class, at))
            || (early && self.reads.contains(&(class, at)))
            || ((early || !role.is_def()) && self.passed.contains(&(class, at)))
    }

    /// Whether the instruction names that register at all, which is what borrowing has to keep off:
    /// what is borrowed is put back behind the instruction, over anything left there.
    fn names(&self, class: RegClass, at: PhysReg) -> bool {
        self.reads.contains(&(class, at))
            || self.writes.contains(&(class, at))
            || self.passed.contains(&(class, at))
            || self.landed.contains(&(class, at))
    }

    /// Whether the instruction names that register only as one an answer is moved into behind it,
    /// which is a register that can be handed out with nothing put away.
    fn only_lands(&self, class: RegClass, at: PhysReg) -> bool {
        self.landed.contains(&(class, at))
            && !self.reads.contains(&(class, at))
            && !self.writes.contains(&(class, at))
            && !self.passed.contains(&(class, at))
    }

    /// Whether the instruction names that register only as one a value passes through on its way
    /// to a fixed register, which is a register borrowing can fall back on.
    fn passes_through(&self, class: RegClass, at: PhysReg) -> bool {
        self.passed.contains(&(class, at))
            && !self.reads.contains(&(class, at))
            && !self.writes.contains(&(class, at))
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
    /// Whether this is [`probe`], which is told that a class with nothing to borrow borrowed.
    probing: bool,
    /// What is kept in each register between instructions, and the instruction this is, which
    /// says which registers can be borrowed with nothing put away.
    idle: (&'a Idle<'a>, Inst),
}

impl<'a> Scratch<'a> {
    /// Nothing borrowed yet at an instruction claiming those registers.
    fn new(
        env: &'a Env,
        assignment: &'a mut Assignment,
        spare: &'a mut Spare,
        claimed: Claimed,
        idle: (&'a Idle<'a>, Inst),
    ) -> Self {
        Self {
            env,
            assignment,
            spare,
            claimed,
            borrowed: Vec::new(),
            saves: Vec::new(),
            restores: Vec::new(),
            probing: false,
            idle,
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
    /// allocator gave a value that is live right across the instruction will do, which is the whole
    /// point of putting the contents away first.
    ///
    /// When every register is claimed, one an input only passes through on its way to the fixed
    /// register it asked for will do. That is i386 inline assembly naming `esi` and `edi`, which are
    /// the two held back, and `eax` and `ecx`, with a fifth operand left that can be anywhere:
    /// `strncat` in arch/x86/lib/string_32.c is one.
    ///
    /// A register no value is kept in across the instruction comes before those too, and nothing
    /// is put away for it either. Whatever it holds is not read again before something writes it.
    /// On i386 that was most of what borrowing cost: `cmpl 8(%ecx), %ebx` with both on the stack
    /// put `ecx` in a slot and took it back out when nothing was in `ecx` at all.
    ///
    /// A register an answer is only moved into behind the instruction comes before all of those,
    /// because nothing is live in it while the instruction runs and so nothing is put away. The
    /// value it carries in or out is moved before the answer arrives, since the stores behind the
    /// instruction go ahead of the moves into registers. That is i386 inline assembly naming
    /// `ecx`, `esi` and `edi` and asking for three more registers with the frame pointer in `ebp`,
    /// when the allocator put the answer in `ecx` somewhere else: `memmove` in
    /// arch/x86/lib/memcpy_32.c up to Linux 6.1 is one.
    ///
    /// # Panics
    ///
    /// Panics if the class has no register the instruction has not already claimed, which is an
    /// instruction naming every register of a file at once. A probe is given a register the
    /// instruction holds instead, since what it wants to know is only that a slot was taken, and
    /// panics only on a class with no registers at all.
    fn borrow(&mut self, class: RegClass) -> PhysReg {
        let index = usize::from(class.number());
        let order = self.env.order(class);
        if let Some(&at) = order.iter().find(|&&reg| self.claimed.only_lands(class, reg)) {
            self.claimed.taken(class, at);
            return at;
        }
        let (idle, inst) = self.idle;
        let unused =
            |reg: PhysReg| !self.claimed.names(class, reg) && !idle.holds(class, reg, inst);
        if let Some(&at) = order.iter().find(|&&reg| unused(reg)) {
            self.claimed.taken(class, at);
            return at;
        }
        let free = order
            .iter()
            .find(|&&reg| !self.claimed.names(class, reg))
            .or_else(|| order.iter().find(|&&reg| self.claimed.passes_through(class, reg)));
        let at = *match free {
            Some(reg) => reg,
            None if self.probing => order
                .first()
                .or_else(|| self.env.scratch(class).first())
                .expect("a class with no registers in it"),
            None => panic!("an instruction naming every register of its class at once"),
        };

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

/// Where every value the assignment put in a register is kept there, arranged to say which
/// registers hold nothing at an instruction.
///
/// A physical register an operand names outright is only named for the length of that instruction,
/// which `crate::assign::blocked` counts on as well, so between instructions what is in a register
/// is a value the assignment put there and nothing else. The pieces of those values are kept by
/// register and ordered by where they start, with the furthest any of the ones so far reaches, so
/// asking whether one covers an instruction is one binary search.
#[derive(Debug, Clone)]
pub struct Idle<'a> {
    order: &'a Order,
    /// By class number and then by register number.
    lanes: Vec<Vec<Lane>>,
}

/// The pieces kept in one register.
#[derive(Debug, Clone, Default)]
struct Lane {
    /// Where each piece starts, in order.
    starts: Vec<Point>,
    /// The furthest point that piece or any in front of it reaches.
    reach: Vec<Point>,
}

impl<'a> Idle<'a> {
    /// Reads where everything went off an assignment of the function laid out by `order`.
    #[must_use]
    pub fn of(func: &Func, assignment: &Assignment, order: &'a Order, live: &Live) -> Self {
        let mut pieces: Vec<Vec<Vec<(Point, Point)>>> = Vec::new();
        for (reg, place) in assignment.placed() {
            let (Place::Reg(at), Some(class), Some(area)) =
                (place, func.class_of(reg), live.area(reg))
            else {
                continue;
            };
            let (class, at) = (usize::from(class.number()), usize::from(at.number()));
            if pieces.len() <= class {
                pieces.resize_with(class + 1, Vec::new);
            }
            if pieces[class].len() <= at {
                pieces[class].resize_with(at + 1, Vec::new);
            }
            pieces[class][at].extend(area.pieces().map(|range| (range.start, range.end)));
        }
        let lane = |mut pieces: Vec<(Point, Point)>| {
            pieces.sort_unstable();
            let mut furthest = 0;
            let reach = pieces
                .iter()
                .map(|&(_, end)| {
                    furthest = furthest.max(end);
                    furthest
                })
                .collect();
            Lane { starts: pieces.into_iter().map(|(start, _)| start).collect(), reach }
        };
        let lanes = pieces.into_iter().map(|lanes| lanes.into_iter().map(lane).collect()).collect();
        Self { order, lanes }
    }

    /// Whether some value is kept in the register anywhere from where the instruction reads to
    /// where it writes.
    fn holds(&self, class: RegClass, at: PhysReg, inst: Inst) -> bool {
        let lane = self
            .lanes
            .get(usize::from(class.number()))
            .and_then(|lanes| lanes.get(usize::from(at.number())));
        let Some(lane) = lane else { return false };
        let ahead = lane.starts.partition_point(|&start| start <= self.order.late(inst));
        ahead > 0 && lane.reach[ahead - 1] >= self.order.early(inst)
    }
}

/// The register the answer tied to that operand lives in, when nothing else the instruction reads
/// is there, so the spilled value it reads can be read straight into it.
///
/// The instruction writes its answer over the operand, so the answer's register is the one the
/// value has to end up in anyway. Read into a scratch register it is copied there in front of the
/// instruction, which is a load and a move for what one load does. The assignment only gave the
/// answer that register because whatever was in it is not wanted past the instruction, and it is
/// free from in front of the instruction as long as no other operand arrives in it.
fn answer(
    operands: &[Operand],
    places: &[Place],
    index: usize,
    claimed: &Claimed,
) -> Option<PhysReg> {
    let source = operands[index];
    operands.iter().zip(places).find_map(|(operand, place)| {
        let Constraint::Reuse(other) = operand.constraint else { return None };
        let Place::Reg(at) = *place else { return None };
        (usize::from(other) == index
            && operand.class == source.class
            && !claimed.clashes(Role::Use, source.class, at))
        .then_some(at)
    })
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
    use rucc_target::x86_64::{GPR, RAX, RCX, RDX, RSI, SYSV};

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
        let order = Order::of(func);
        let live = Live::of(func, &order);
        let idle = Idle::of(func, assignment, &order, &live);
        instruction(func, assignment, &env(), &mut Spare::new(), &idle, inst)
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
    fn an_answer_written_early_is_not_given_the_register_a_value_is_read_into() {
        // The RISC-V select moves one source into its answer and then reads the other. With
        // both on the stack the answer had the register the second source was read into and the
        // select picked the first source either way, which sent the kernel's vmap past its end.
        let (mut func, opcode, block) = func();
        let answer = func.new_vreg(GPR);
        let sources: Vec<Reg> = (0..2).map(|_| func.new_vreg(GPR)).collect();
        let inst = func
            .build(block, opcode)
            .operand(Operand::write_early(answer, GPR))
            .uses(sources[0], GPR)
            .uses(sources[1], GPR)
            .finish();
        let mut assignment = Assignment::empty(func.vregs());
        for value in [answer, sources[0], sources[1]] {
            let slot = assignment.take_slot(GPR);
            assignment.put(value, Place::Slot(slot));
        }

        let legal = legal(&func, &mut assignment, inst);
        let regs = regs(&legal);
        assert_ne!(regs[0], regs[1], "the answer is the first source's register");
        assert_ne!(regs[0], regs[2], "the answer is the second source's register");
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
    fn a_two_address_source_on_the_stack_is_read_into_the_answer() {
        let (mut func, opcode, block) = func();
        let source = func.new_vreg(GPR);
        let answer = func.new_vreg(GPR);
        let inst = func
            .build(block, opcode)
            .operand(Operand::write(answer, GPR).with(Constraint::Reuse(1)))
            .uses(source, GPR)
            .finish();
        let mut assignment = Assignment::empty(func.vregs());
        let slot = assignment.take_slot(GPR);
        assignment.put(source, Place::Slot(slot));
        assignment.put(answer, Place::Reg(RAX));

        let legal = legal(&func, &mut assignment, inst);
        assert_eq!(regs(&legal), [RAX, RAX]);
        assert_eq!(legal.before, [(Move::new(Place::Reg(RAX), Place::Slot(slot)), GPR)]);
    }

    #[test]
    fn a_two_address_source_goes_through_scratch_when_another_operand_is_in_the_answer() {
        let (mut func, opcode, block) = func();
        let source = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let answer = func.new_vreg(GPR);
        let inst = func
            .build(block, opcode)
            .operand(Operand::write(answer, GPR).with(Constraint::Reuse(1)))
            .uses(source, GPR)
            .uses(other, GPR)
            .finish();
        let mut assignment = Assignment::empty(func.vregs());
        let slot = assignment.take_slot(GPR);
        assignment.put(source, Place::Slot(slot));
        assignment.put(other, Place::Reg(RAX));
        assignment.put(answer, Place::Reg(RAX));

        let legal = legal(&func, &mut assignment, inst);
        let scratch = env().scratch(GPR)[0];
        assert_eq!(regs(&legal), [RAX, scratch, RAX]);
        assert_eq!(legal.before[0], (Move::new(Place::Reg(scratch), Place::Slot(slot)), GPR));
    }

    #[test]
    fn a_third_value_on_the_stack_borrows_a_register_and_gives_it_back() {
        let (mut func, opcode, block) = func();
        // Both registers handed out keep a value from in front of the instruction to behind it.
        let kept: Vec<Reg> = (0..2).map(|_| func.new_vreg(GPR)).collect();
        for &value in &kept {
            func.build(block, opcode).def(value, GPR).finish();
        }
        let values: Vec<Reg> = (0..3).map(|_| func.new_vreg(GPR)).collect();
        let build = func.build(block, opcode);
        let inst = values.iter().fold(build, |build, &value| build.uses(value, GPR)).finish();
        func.build(block, opcode).uses(kept[0], GPR).uses(kept[1], GPR).finish();
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(kept[0], Place::Reg(RAX));
        assignment.put(kept[1], Place::Reg(RCX));
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

    #[test]
    fn a_register_holding_nothing_is_borrowed_with_nothing_put_away() {
        let (mut func, opcode, block) = func();
        let values: Vec<Reg> = (0..3).map(|_| func.new_vreg(GPR)).collect();
        for &value in &values {
            func.build(block, opcode).def(value, GPR).finish();
        }
        let build = func.build(block, opcode);
        let inst = values.iter().fold(build, |build, &value| build.uses(value, GPR)).finish();
        let mut assignment = Assignment::empty(func.vregs());
        for &value in &values {
            let slot = assignment.take_slot(GPR);
            assignment.put(value, Place::Slot(slot));
        }

        let legal = legal(&func, &mut assignment, inst);
        assert_eq!(regs(&legal)[2], RAX);
        // Three loads in front, one into each register, and no slot taken for the one borrowed.
        assert_eq!(legal.before.len(), 3);
        assert!(legal.after.is_empty());
        assert_eq!(assignment.spilled(), 3);
    }

    #[test]
    fn a_register_an_answer_only_lands_in_carries_another_answer_to_the_stack() {
        let (mut func, opcode, block) = func();
        let values: Vec<Reg> = (0..4).map(|_| func.new_vreg(GPR)).collect();
        // Both registers held back are fixed answers and both handed out are named, so the only
        // register left is the one the first answer is moved into behind the instruction.
        let inst = func
            .build(block, opcode)
            .operand(Operand::write(values[0], GPR).with(Constraint::Fixed(RDX)))
            .operand(Operand::write(values[1], GPR).with(Constraint::Fixed(RSI)))
            .operand(Operand::write(values[2], GPR))
            .operand(Operand::write(values[3], GPR))
            .finish();
        let mut assignment = Assignment::empty(func.vregs());
        assignment.put(values[0], Place::Reg(RAX));
        let first = assignment.take_slot(GPR);
        assignment.put(values[1], Place::Slot(first));
        assignment.put(values[2], Place::Reg(RCX));
        let last = assignment.take_slot(GPR);
        assignment.put(values[3], Place::Slot(last));

        let legal = legal(&func, &mut assignment, inst);
        assert_eq!(regs(&legal), [RDX, RSI, RCX, RAX]);
        // Nothing is put away, and the answer in `rax` is stored before the first one arrives.
        assert!(legal.before.is_empty());
        assert_eq!(
            legal.after,
            [
                (Move::new(Place::Slot(first), Place::Reg(RSI)), GPR),
                (Move::new(Place::Slot(last), Place::Reg(RAX)), GPR),
                (Move::new(Place::Reg(RAX), Place::Reg(RDX)), GPR),
            ]
        );
        assert_eq!(assignment.spilled(), 2);
    }
}
