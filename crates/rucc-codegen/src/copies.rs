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
//! # What it does not do
//!
//! A move that could be a cheaper move is left as it is. A slot read into a register while another
//! register holds the same word is a load that a copy would do instead, and a copy is the cheaper
//! of the two on every machine here. Writing one means naming the instruction that copies a
//! register of that class at that width, which is a question for the description rather than for
//! the map, and it is the next piece of section 37.4's pass rather than part of this one.
//!
//! Nothing is propagated. A read of a register that another register is known to equal stays a
//! read of the register it names, so this takes moves out and never rewrites what is left.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_mir::{Block, Func, Inst};
use rucc_regalloc::assign::Place;
use rucc_target::{CallRegs, MachineInsts, PhysReg, RegClass};

use crate::finish::Moves;

/// Takes out every move of the allocator's that puts a value where it is already, and says how
/// many.
pub fn dead(
    func: &mut Func,
    moves: &Moves,
    machine: &MachineInsts,
    conv: &CallRegs,
    names: &Interner,
) -> usize {
    let blocks: Vec<Block> = func.blocks().collect();
    let mut taken = 0;
    for block in blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        let mut holds = Holds::default();
        let mut gone: Vec<Inst> = Vec::new();
        for inst in insts {
            // One of the allocator's own moves, which is the only kind of instruction this takes
            // out and the only kind it learns anything from.
            if let Some(edit) = moves.at(inst) {
                if holds.same(edit.class, edit.mov.to, edit.mov.from) {
                    gone.push(inst);
                } else {
                    holds.moved(edit.class, edit.mov.to, edit.mov.from);
                }
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
            taken += 1;
        }
    }
    taken
}

/// Whether that register is one the frame is addressed through, so writing it moves every slot.
fn addresses(conv: &CallRegs, class: RegClass, reg: PhysReg) -> bool {
    class == conv.int_class && (reg == conv.stack_pointer || reg == conv.frame_pointer)
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

    /// A function with one block in it, and the names it was built with.
    fn empty() -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        (names, func, block)
    }

    /// The pass, with the one target these tests are written against.
    fn dead(func: &mut Func, moves: &Moves, names: &Interner) -> usize {
        super::dead(func, moves, &MACHINE, &SYSV, names)
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

        assert_eq!(dead(&mut func, &moves, &names), 1);
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

        assert_eq!(dead(&mut func, &moves, &names), 2);
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

        assert_eq!(dead(&mut func, &moves, &names), 2);
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

        assert_eq!(dead(&mut func, &moves, &names), 0);
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

        assert_eq!(dead(&mut func, &moves, &names), 1);
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

        assert_eq!(dead(&mut func, &moves, &names), 1);
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

        assert_eq!(dead(&mut func, &moves, &names), 0);
        assert_eq!(left(&func, block), 2);
    }

    /// A reload into another register puts the word somewhere it is not, so the load does
    /// something even though the word it reads is the one just written.
    #[test]
    fn a_reload_into_a_different_register_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let reload = load(&mut func, &mut names, block, PhysReg::new(11), 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, PhysReg::new(11)));

        assert_eq!(dead(&mut func, &moves, &names), 0);
        assert_eq!(left(&func, block), 2);
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

        assert_eq!(dead(&mut func, &moves, &names), 0);
        assert_eq!(left(&func, block), 2);
    }

    /// The same two instructions, with nothing saying the allocator wrote them, which is what a
    /// write to a local variable and a read of it back look like.
    #[test]
    fn a_store_and_a_load_the_allocator_did_not_write_stay() {
        let (mut names, mut func, block) = empty();
        store(&mut func, &mut names, block, R10, 16);
        load(&mut func, &mut names, block, R10, 16);

        assert_eq!(dead(&mut func, &Moves::default(), &names), 0);
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

        assert_eq!(dead(&mut func, &moves, &names), 0);
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

        assert_eq!(dead(&mut func, &moves, &names), 1);
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

        assert_eq!(dead(&mut func, &moves, &names), 0);
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

        assert_eq!(dead(&mut func, &moves, &names), 0);
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

        assert_eq!(dead(&mut func, &moves, &names), 0);
        assert_eq!(left(&func, block), 3);
    }
}
