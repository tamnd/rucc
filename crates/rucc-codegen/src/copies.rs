//! Taking out a move the allocator wrote that puts a value where the machine has it already.
//!
//! Design: `spec/optimizer/37-machine-level-optimization.md` sections 37.4 and 37.6, the group of
//! passes that run after allocation and clean up what the allocator could not.
//!
//! The allocator decides one value at a time. It writes a value out when the range it was given a
//! register for ends, it reads a value back in front of the instruction that wants it, and neither
//! decision looks at the other. So a value written by one instruction and wanted by the next comes
//! out as a store and then the load of the same slot on the very next line:
//!
//! ```text
//!   movq %r10, 16(%rsp)
//!   movq 16(%rsp), %r10
//! ```
//!
//! The load reads a word the store has just written, into the register the store read it out of,
//! so the register already holds what the load would put in it. It is a memory access that cannot
//! change anything, on the two instructions of the pair that are the expensive one.
//!
//! The same thing happens with instructions in between. A value spilled once and read back at
//! three places in a block is three loads of one slot into one scratch register, and the second
//! and the third are reads of a word that register still holds. A value read back and then written
//! out again unchanged is a store of a word the slot still holds. A copy into a register that
//! already holds what it is being given is a copy of nothing. All of them are the same mistake
//! seen from different sides, which is why they are one pass rather than four: a move is dead when
//! what it writes to and what it reads from hold the same value already.
//!
//! The near miss of the same thing is a load of a slot into a register while a different register
//! holds that word. Nothing can be removed there, since the word does have to arrive in the
//! register the load names, but it can come out of the register that has it rather than out of the
//! frame. That is the second thing this does and the rest of the same knowledge answers it.
//!
//! # Why it is not a rule over the instructions
//!
//! A store followed by a load of the same address is not on its own a dead load. The same pair of
//! instructions is what a write to a local variable and a read of it back look like, and when that
//! variable is `volatile` the read is one the program insisted on and the standard says happens.
//! Machine IR does not carry that word, and by the time the pass runs it could not: a volatile
//! access and an ordinary one are the same instruction with the same operands.
//!
//! So this does not look for the pattern. [`crate::finish`] records which instruction each of the
//! allocator's moves became, and this pass only ever takes out one of those. A spill slot belongs
//! to the allocator, nothing else reads it or writes it, and no part of the program said anything
//! about it, which is what makes removing a read of one safe when removing a read of a variable is
//! not.
//!
//! A slot can end up sharing its bytes with a local, which [`crate::slots`] arranges, and that does
//! not change the argument. Two things share a run of the frame only where they are never both
//! wanted, and a slot between a spill and the read back of it is wanted the whole way, so a store
//! to the local it shares with cannot fall in that stretch.
//!
//! # Why after the whole allocator rather than inside it
//!
//! The edits are decided in different places for different reasons, so no one of the decisions is
//! wrong on its own and there is no place inside the allocator that sees them together. What ends
//! up between two of them is settled by [`crate::finish`] writing every edit into the function,
//! which is after the allocator has finished. They are all visible at once here and nowhere
//! earlier.
//!
//! # What a block is walked with
//!
//! One map from a place to a number standing for a value, where a place is a register of a class
//! or a slot of one. Two places with the same number hold the same bits, and that is the whole of
//! what the pass knows: nothing here has to know what the value is, only that a move of one place
//! into another with the same number would write what is there.
//!
//! The map starts empty at the top of every block, because what a register holds on the way in is
//! whatever the block before it left, and which block that is depends on the path. A map carried
//! across edges would be worth something on a straight line of blocks and is a dataflow problem
//! rather than a walk, which is section 37.4's own answer for why this is the cheap half.
//!
//! # What clears it
//!
//! A write to a place clears what was there, which is every definition in an operand vector and
//! the destination of every move.
//!
//! A call clears everything. The registers a convention does not preserve are gone across one,
//! and the ones the arguments travelled in are written down as reads rather than as writes, so
//! the operand vector of a call does not say what it destroys. That is why the machine
//! description is asked which instructions are calls rather than the operands being trusted.
//!
//! An instruction the description does not name clears everything too, on the same reasoning
//! backwards: what a pass cannot look up it cannot claim to have read.
//!
//! An instruction that writes the stack pointer or the frame pointer clears everything, because a
//! slot is named by an offset from one of those and a slot at a new address is not the slot the
//! value was written to. A prologue and an epilogue do this, which costs nothing since neither is
//! in the middle of anything, and so does the instruction that takes room for an array whose size
//! is not known until it runs.
//!
//! # When a load becomes a copy
//!
//! A slot read into a register while no register holds that word is a load and has to stay one. A
//! slot read into one register while another register holds the same word is a load a copy would
//! do instead, and a copy is the cheaper of the two on every machine here: it is fewer bytes, it
//! does not go near the memory unit, and on a machine that renames its registers it often costs
//! nothing to run at all.
//!
//! Which instruction that copy is is the target's answer and not this pass's, and it is the same
//! answer [`crate::finish`] read to write the load. A class of registers is moved between two
//! registers by one named instruction and between a register and the frame by two others, and the
//! three are named together for that class, so asking for the one is asking the description that
//! produced the other.
//!
//! Being named together is also what makes the widths agree. Every entry in the map was put there
//! by one of the allocator's own moves and every one of those moves a whole register of its class,
//! so two places holding one value hold it in all of their bytes rather than in a low part that a
//! wider copy would read past.
//!
//! Where several registers hold the word, the lowest numbered of them is the one written. Any of
//! them would be correct, and the map is a hash map, so writing whichever came out of it first
//! would make the assembly depend on where the addresses happened to land. A compiler whose output
//! moves between two runs of one input is one nobody can compare anything against, which is a
//! worse thing to be than one that picks the second best register.
//!
//! # Why it does not go through the change framework
//!
//! Because the one question [`crate::changes`] would answer about a removal is one that cannot be
//! answered here. The framework refuses to take an instruction out while anything still reads a
//! register it wrote, and it knows that from a count of the reads in the function, which is the
//! whole answer while every register is written once and is not the answer at all afterwards: the
//! register a reload writes is physical by the time this runs, the same one is written and read all
//! over the function about other values, and the count says so. Every removal here would be turned
//! down.
//!
//! What makes these safe is not a count but where the instruction came from. It is one of the
//! allocator's own moves, [`crate::finish`] wrote it, and the value is in the register already
//! because an earlier move of the allocator's put it there. That is a reason the framework has no
//! way to be told, and section 37.2's framework is about the machine's description of itself
//! rather than about the allocator's, so this keeps its own.
//!
//! The framework's own question, whether the target has an instruction of the shape a pass
//! proposes, is one the rewrite does not have to ask either. The copy it writes is the instruction
//! the description names for moving a register of that class, so it is an instruction the target
//! has by where the name came from rather than by a lookup afterwards.
//!
//! # What it does not do
//!
//! Nothing is propagated. A read of a register that another register is known to equal stays a
//! read of the register it names. The two things this does are both to one of the allocator's own
//! moves and to nothing else, for the reason the rest of this is built on: what makes an edit here
//! safe is that the allocator wrote the instruction and owns the slot, and an instruction a
//! lowering rule wrote is neither.
//!
//! Nothing crosses a block, on either half.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_mir::{Block, Func, Inst, Opcode, Reg};
use rucc_regalloc::assign::Place;
use rucc_regalloc::rewrite::Edit;
use rucc_target::{CallRegs, FrameInsts, MachineInsts, PhysReg, RegClass};

use crate::finish::Moves;

/// What one function came to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cleaned {
    /// Moves taken out, because the place each wrote held what it was moving already.
    pub gone: usize,
    /// Loads out of the frame written as copies instead, because a register held the word.
    pub copied: usize,
}

/// Takes out every move of the allocator's that puts a value where it is already, and reads the
/// rest out of a register wherever one has the word the frame does.
///
/// # Panics
///
/// Panics on a move of a class the target did not say how to move, which is the same frame
/// description [`crate::finish`] wrote the move out of and so is the caller handing this a
/// function and a target that were not worked out from each other.
pub fn clean(
    func: &mut Func,
    moves: &Moves,
    machine: &MachineInsts,
    frame: &FrameInsts,
    conv: &CallRegs,
    names: &mut Interner,
) -> Cleaned {
    let blocks: Vec<Block> = func.blocks().collect();
    let mut cleaned = Cleaned::default();
    for block in blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        let mut holds = Holds::default();
        let mut gone: Vec<Inst> = Vec::new();
        for inst in insts {
            // One of the allocator's own moves, which is the only kind of instruction this edits
            // and the only kind it learns anything from.
            if let Some(edit) = moves.at(inst) {
                if holds.same(edit.class, edit.mov.to, edit.mov.from) {
                    gone.push(inst);
                    cleaned.gone += 1;
                    continue;
                }
                // A load of a word a register has. The copy goes where the load was and the load
                // goes, and what arrives in the register is the same either way, so the map is
                // told about the move below whichever of the two instructions is left.
                if let Some((to, from)) = instead(&holds, &edit) {
                    let copy = copy(func, names, frame, edit.class, to, from);
                    func.insert_before(inst, copy);
                    gone.push(inst);
                    cleaned.copied += 1;
                }
                holds.moved(edit.class, edit.mov.to, edit.mov.from);
                continue;
            }
            let name = names.resolve(func[inst].opcode.name());
            if machine.calls(name) || !machine.has(name) {
                holds.nothing();
                continue;
            }
            let mut addressing = false;
            for operand in &func[func[inst].operands] {
                if !operand.role.is_def() {
                    continue;
                }
                let Some(reg) = operand.reg.phys() else { continue };
                addressing |= addresses(conv, operand.class, reg);
                holds.wrote(operand.class, Place::Reg(reg));
            }
            if addressing {
                holds.nothing();
            }
        }
        for inst in gone {
            func.remove_inst(inst);
        }
    }
    cleaned
}

/// Whether that register is one the frame is addressed through, so writing it moves every slot.
fn addresses(conv: &CallRegs, class: RegClass, reg: PhysReg) -> bool {
    class == conv.int_class && (reg == conv.stack_pointer || reg == conv.frame_pointer)
}

/// The two registers a copy would be written between, where this edit is a load out of the frame
/// of a word some register holds.
///
/// `None` for every other edit. A store has nowhere else to go, since the word has to reach the
/// frame and no machine here writes the frame from anywhere but a register, and a copy between two
/// registers is already the instruction this would be turning something into.
fn instead(holds: &Holds, edit: &Edit) -> Option<(PhysReg, PhysReg)> {
    let (Place::Reg(to), Place::Slot(_)) = (edit.mov.to, edit.mov.from) else { return None };
    Some((to, holds.register(edit.class, edit.mov.from)?))
}

/// The instruction that copies a register of that class into another on this target.
fn copy(
    func: &mut Func,
    names: &mut Interner,
    frame: &FrameInsts,
    class: RegClass,
    to: PhysReg,
    from: PhysReg,
) -> Inst {
    let moves = frame.moves(class).expect("a class the target says how to move");
    let mov = Opcode::new(names.intern(&format!("{}{}", frame.prefix, moves.mov)));
    func.build_loose(mov).def(Reg::physical(to), class).uses(Reg::physical(from), class).finish()
}

/// Which places are known to hold the same value as each other, over one block.
///
/// A value is a number and nothing more. Where it came from and what it means are questions this
/// does not ask, because the only thing a move is taken out over is two places holding the same
/// one.
#[derive(Debug, Default)]
struct Holds {
    /// What is in each place, by the class it is a place of and the place itself. A class is in
    /// the key because a register is a number inside its class and a slot is a slot of one, so
    /// number four of one file and number four of another are two places.
    what: HashMap<(u8, Place), u32>,
    /// How many values have been named, so the next one is a number no other place holds.
    named: u32,
}

impl Holds {
    /// Whether both places are known to hold one value, which is what makes a move of the one into
    /// the other write what is there.
    fn same(&self, class: RegClass, to: Place, from: Place) -> bool {
        let read = self.what.get(&(class.number(), from));
        read.is_some() && read == self.what.get(&(class.number(), to))
    }

    /// A register known to hold what that place holds, and the lowest numbered of them where more
    /// than one does.
    ///
    /// Lowest numbered rather than whichever the map hands back first, because the map is a hash
    /// map and which entry comes out of one first is a fact about addresses. Reading it would make
    /// the assembly of one input differ between two runs of the compiler.
    fn register(&self, class: RegClass, place: Place) -> Option<PhysReg> {
        let value = *self.what.get(&(class.number(), place))?;
        self.what
            .iter()
            .filter(|&(&(number, _), &held)| number == class.number() && held == value)
            .filter_map(|(&(_, place), _)| match place {
                Place::Reg(reg) => Some(reg),
                Place::Slot(_) => None,
            })
            .min_by_key(|reg| reg.number())
    }

    /// Records a move that ran, so what it wrote holds what it read.
    ///
    /// A read of a place nothing is known about is what names a value: the bits are whatever they
    /// are, the two ends of the move agree about them from here on, and that agreement is the only
    /// thing this pass ever asks about.
    fn moved(&mut self, class: RegClass, to: Place, from: Place) {
        let value = match self.what.get(&(class.number(), from)) {
            Some(&value) => value,
            None => {
                self.named += 1;
                self.what.insert((class.number(), from), self.named);
                self.named
            }
        };
        self.what.insert((class.number(), to), value);
    }

    /// Records that something wrote a place, so whatever it held is no longer what is there.
    fn wrote(&mut self, class: RegClass, place: Place) {
        self.what.remove(&(class.number(), place));
    }

    /// Forgets the block so far, which is the answer to an instruction whose writes cannot all be
    /// seen.
    fn nothing(&mut self) {
        self.what.clear();
    }
}

#[cfg(test)]
mod tests {
    use rucc_mir::{Mem, Opcode, Operand, Reg};
    use rucc_regalloc::moves::Move;
    use rucc_regalloc::rewrite::{At, Edit};
    use rucc_target::x86_64::{FRAME, GPR, MACHINE, RAX, SYSV, XMM};

    use super::*;

    /// A spill register to write with, which is the one x86-64 holds back for exactly this.
    const R10: PhysReg = PhysReg::new(10);

    /// The second of them, which is what an instruction with both operands on the stack reads the
    /// other one into.
    const R11: PhysReg = PhysReg::new(11);

    /// A function with one block in it, and the names it was built with.
    fn empty() -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        (names, func, block)
    }

    /// The pass, with the one target these tests are written against.
    fn clean(func: &mut Func, moves: &Moves, names: &mut Interner) -> Cleaned {
        super::clean(func, moves, &MACHINE, &FRAME, &SYSV, names)
    }

    /// How many moves went, for a test about the half that removes.
    fn gone(func: &mut Func, moves: &Moves, names: &mut Interner) -> usize {
        clean(func, moves, names).gone
    }

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> Opcode {
        Opcode::new(names.intern(&format!("{}{name}", FRAME.prefix)))
    }

    /// A store of a physical register to a frame address, as [`crate::finish`] writes a spill.
    fn store(func: &mut Func, names: &mut Interner, block: Block, reg: PhysReg, at: i32) -> Inst {
        let store = op(names, "mov_mr_64");
        let base = Operand::read(Reg::physical(RAX), GPR);
        func.build(block, store).uses(Reg::physical(reg), GPR).mem(Mem::at(base).plus(at)).finish()
    }

    /// A load of a physical register from a frame address, as it writes a reload.
    fn load(func: &mut Func, names: &mut Interner, block: Block, reg: PhysReg, at: i32) -> Inst {
        let load = op(names, "mov_rm_64");
        let base = Operand::read(Reg::physical(RAX), GPR);
        func.build(block, load).def(Reg::physical(reg), GPR).mem(Mem::at(base).plus(at)).finish()
    }

    /// A copy of one physical register into another, as it writes one of the allocator's copies.
    fn copy(
        func: &mut Func,
        names: &mut Interner,
        block: Block,
        to: PhysReg,
        from: PhysReg,
    ) -> Inst {
        let copy = op(names, "mov_rr_64");
        func.build(block, copy).def(Reg::physical(to), GPR).uses(Reg::physical(from), GPR).finish()
    }

    /// An instruction that reads a register and writes another, which is what a spilled value was
    /// read back for.
    fn add(
        func: &mut Func,
        names: &mut Interner,
        block: Block,
        to: PhysReg,
        from: PhysReg,
    ) -> Inst {
        let add = op(names, "add_rr_64");
        func.build(block, add).def(Reg::physical(to), GPR).uses(Reg::physical(from), GPR).finish()
    }

    /// What the allocator asked for, in the two shapes this pass is about.
    ///
    /// Where the edit was to go is not read by anything here, since what says two instructions are
    /// next to each other is the function they were written into rather than what the allocator
    /// said about where they belong.
    fn out(block: Block, slot: u32, reg: PhysReg) -> Edit {
        Edit {
            at: At::StartOf(block),
            mov: Move::new(Place::Slot(slot), Place::Reg(reg)),
            class: GPR,
        }
    }

    /// The other direction, and the one a dead reload is.
    fn back(block: Block, slot: u32, reg: PhysReg) -> Edit {
        Edit {
            at: At::StartOf(block),
            mov: Move::new(Place::Reg(reg), Place::Slot(slot)),
            class: GPR,
        }
    }

    /// A move of one register into another, which the allocator writes where the two ends of a
    /// value could not be given the same register.
    fn across(block: Block, to: PhysReg, from: PhysReg) -> Edit {
        Edit {
            at: At::StartOf(block),
            mov: Move::new(Place::Reg(to), Place::Reg(from)),
            class: GPR,
        }
    }

    /// How many instructions a block has left.
    fn left(func: &Func, block: Block) -> usize {
        func.insts(block).count()
    }

    /// What the block says now, one opcode per instruction, which is how a test about a rewrite
    /// says which instruction came out of it.
    fn written(func: &Func, block: Block, names: &Interner) -> Vec<String> {
        func.insts(block).map(|inst| names.resolve(func[inst].opcode.name()).to_owned()).collect()
    }

    /// The registers the instruction at that position in the block reads.
    fn reads(func: &Func, block: Block, at: usize) -> Vec<PhysReg> {
        let inst = func.insts(block).nth(at).expect("an instruction at that position");
        func[func[inst].operands]
            .iter()
            .filter(|operand| !operand.role.is_def())
            .filter_map(|operand| operand.reg.phys())
            .collect()
    }

    /// The pair the pass started as: a word written out and read straight back into the register it
    /// was written out of.
    #[test]
    fn a_reload_of_the_slot_the_instruction_in_front_of_it_spilled_goes() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 1);
        assert_eq!(left(&func, block), 1, "the spill went too, or the reload stayed");
        assert_eq!(func.insts(block).next(), Some(spill));
    }

    /// Two reads of one word, which is one spill and two reloads, and the second is as dead as the
    /// first because the register still holds what the first put in it.
    #[test]
    fn a_run_of_reloads_of_one_slot_goes_in_a_single_pass() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let first = load(&mut func, &mut names, block, R10, 16);
        let second = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(first, back(block, 0, R10));
        moves.record(second, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 2);
        assert_eq!(left(&func, block), 1);
    }

    /// The case the pass was grown for. What the value was read back for stands between the two
    /// reloads, and it writes neither the slot nor the register the word is in, so the second read
    /// is of a word that register still holds.
    #[test]
    fn a_reload_with_an_instruction_between_that_writes_neither_end_goes() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let first = load(&mut func, &mut names, block, R10, 16);
        add(&mut func, &mut names, block, RAX, R10);
        let second = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(first, back(block, 0, R10));
        moves.record(second, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 2);
        assert_eq!(left(&func, block), 2, "the spill and the instruction between are the two");
    }

    /// The instruction between them writes the register this time, which is what a spilled value
    /// is read back into a scratch register for, so the reload behind it reads a word that
    /// register no longer holds.
    #[test]
    fn a_reload_behind_an_instruction_that_writes_the_register_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        add(&mut func, &mut names, block, R10, RAX);
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 3);
    }

    /// A word read back and written out again with nothing touching either end is a store of what
    /// the slot holds already.
    #[test]
    fn a_spill_of_a_word_the_slot_still_holds_goes() {
        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, R10, 16);
        let spill = store(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, R10));
        moves.record(spill, out(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 1);
        assert_eq!(left(&func, block), 1);
        assert_eq!(func.insts(block).next(), Some(reload));
    }

    /// A copy into a register that holds what it is being given writes what is there.
    #[test]
    fn a_copy_of_a_word_the_register_already_holds_goes() {
        let (mut names, mut func, block) = empty();
        let first = copy(&mut func, &mut names, block, RAX, R10);
        let second = copy(&mut func, &mut names, block, RAX, R10);
        let mut moves = Moves::default();
        moves.record(first, across(block, RAX, R10));
        moves.record(second, across(block, RAX, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 1);
        assert_eq!(left(&func, block), 1);
    }

    /// A reload of another slot reads another word, whatever address the two instructions were
    /// written with.
    #[test]
    fn a_reload_of_a_different_slot_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let reload = load(&mut func, &mut names, block, R10, 24);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 1, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 2);
    }

    /// A reload into another register puts the word somewhere it is not, so the instruction has
    /// work to do. Where it takes the word from is the question, and the register that was spilled
    /// still holds it, so the frame is not read.
    #[test]
    fn a_reload_into_a_different_register_becomes_a_copy() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let reload = load(&mut func, &mut names, block, R11, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R11));

        assert_eq!(clean(&mut func, &moves, &mut names), Cleaned { gone: 0, copied: 1 });
        assert_eq!(written(&func, block, &names), ["x64.mov_mr_64", "x64.mov_rr_64"]);
        assert_eq!(reads(&func, block, 1), vec![R10]);
    }

    /// The same reload with nothing having put the word in a register. There is nowhere to read it
    /// from but the frame, so the load is the instruction it was.
    #[test]
    fn a_reload_no_register_holds_the_word_of_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, R11, 16);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, R11));

        assert_eq!(clean(&mut func, &moves, &mut names), Cleaned::default());
        assert_eq!(written(&func, block, &names), ["x64.mov_rm_64"]);
    }

    /// Three registers holding one word is three right answers, and the pass takes the lowest
    /// numbered of them every time rather than whichever the map hands back first.
    #[test]
    fn the_copy_is_written_out_of_the_lowest_numbered_register_that_has_the_word() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let first = copy(&mut func, &mut names, block, R11, R10);
        let second = copy(&mut func, &mut names, block, RAX, R10);
        let reload = load(&mut func, &mut names, block, PhysReg::new(12), 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(first, across(block, R11, R10));
        moves.record(second, across(block, RAX, R10));
        moves.record(reload, back(block, 0, PhysReg::new(12)));

        assert_eq!(clean(&mut func, &moves, &mut names), Cleaned { gone: 0, copied: 1 });
        assert_eq!(reads(&func, block, 3), vec![RAX], "rax is register zero");
    }

    /// A call takes the registers with it, so the word is in the frame and nowhere else and the
    /// reload behind one is a load rather than a copy of a register that no longer has it.
    #[test]
    fn a_reload_behind_a_call_is_not_written_as_a_copy() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let call = op(&mut names, "call");
        func.build(block, call).uses(Reg::physical(RAX), GPR).finish();
        let reload = load(&mut func, &mut names, block, R11, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R11));

        assert_eq!(clean(&mut func, &moves, &mut names), Cleaned::default());
        assert_eq!(written(&func, block, &names), ["x64.mov_mr_64", "x64.call", "x64.mov_rm_64"]);
    }

    /// A word written out to the frame has to go to the frame, so a store is left alone however
    /// many registers hold what it is storing.
    #[test]
    fn a_spill_of_a_word_another_register_holds_stays_a_store() {
        let (mut names, mut func, block) = empty();
        let copied = copy(&mut func, &mut names, block, R11, R10);
        let spill = store(&mut func, &mut names, block, R11, 16);
        let mut moves = Moves::default();
        moves.record(copied, across(block, R11, R10));
        moves.record(spill, out(block, 0, R11));

        assert_eq!(clean(&mut func, &moves, &mut names), Cleaned::default());
        assert_eq!(written(&func, block, &names), ["x64.mov_rr_64", "x64.mov_mr_64"]);
    }

    /// A slot number is a slot number in the class that owns it, so a pair that agrees on
    /// everything but the class is two different words and two registers that share a number.
    #[test]
    fn a_reload_of_another_class_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, Edit { class: XMM, ..back(block, 0, R10) });

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 2);
    }

    /// The same two instructions, with nothing saying the allocator wrote them, which is what a
    /// write to a local variable and a read of it back look like.
    #[test]
    fn a_store_and_a_load_the_allocator_did_not_write_stay() {
        let (mut names, mut func, block) = empty();
        store(&mut func, &mut names, block, R10, 16);
        load(&mut func, &mut names, block, R10, 16);

        assert_eq!(gone(&mut func, &Moves::default(), &mut names), 0);
        assert_eq!(left(&func, block), 2);
    }

    /// The last instruction of one block and the first of another are not next to each other. What
    /// ran before the second block is whatever jumped to it, which is any block that names it and
    /// not the one the text happens to be under.
    #[test]
    fn a_reload_at_the_top_of_another_block_stays() {
        let (mut names, mut func, first) = empty();
        let second = func.create_block();
        let spill = store(&mut func, &mut names, first, R10, 16);
        let reload = load(&mut func, &mut names, second, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(first, 0, R10));
        moves.record(reload, back(first, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, second), 1);
    }

    /// A copy of the word somewhere else does not move the word, so the reload behind one is still
    /// a read of what the register holds.
    #[test]
    fn a_copy_out_of_the_register_between_the_two_does_not_stop_it() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let copied = copy(&mut func, &mut names, block, RAX, R10);
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(copied, across(block, RAX, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 1);
        assert_eq!(left(&func, block), 2);
    }

    /// A call destroys the registers the convention does not preserve and says so with a
    /// definition of each, except for the ones its arguments arrived in, which it names as reads.
    /// So what a call leaves alone is not a question the operand vector answers and the whole map
    /// goes.
    #[test]
    fn a_reload_behind_a_call_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let call = op(&mut names, "call");
        func.build(block, call).uses(Reg::physical(RAX), GPR).finish();
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 3);
    }

    /// A slot is an offset from the stack pointer, so an instruction that moves the pointer moves
    /// every slot, and what a register holds is no longer what is at the address the reload names.
    #[test]
    fn a_reload_behind_a_write_of_the_stack_pointer_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let sub = op(&mut names, "sub_ri_64");
        func.build(block, sub)
            .def(Reg::physical(SYSV.stack_pointer), GPR)
            .uses(Reg::physical(SYSV.stack_pointer), GPR)
            .imm(32)
            .finish();
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 3);
    }

    /// An instruction the description does not name is one nothing is known about, including which
    /// registers it writes.
    #[test]
    fn a_reload_behind_an_instruction_the_target_does_not_have_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let strange = op(&mut names, "nothing_of_that_name");
        func.build(block, strange).finish();
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 3);
    }
}
