//! Taking out a reload that reads back the slot the instruction in front of it has just filled.
//!
//! Design: `spec/optimizer/37-machine-level-optimization.md` sections 37.4 and 37.6, the group of
//! passes that run after allocation and clean up what the allocator could not.
//!
//! The allocator decides one value at a time. It writes a value out when the range it was given a
//! register for ends, and it reads a value back in front of the instruction that wants it, and
//! neither decision looks at the other. So a value written by one instruction and wanted by the
//! next comes out as a store and then the load of the same slot on the very next line:
//!
//! ```text
//!   movq %r10, 16(%rsp)
//!   movq 16(%rsp), %r10
//! ```
//!
//! The load reads a word the store has just written, into the register the store read it out of,
//! and nothing stands between the two, so the register already holds what the load would put in
//! it. It is a memory access that cannot change anything, on the two instructions of the pair that
//! are the expensive one. That is what this takes out.
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
//! to the allocator, nothing else reads it, and no part of the program said anything about it,
//! which is what makes removing a read of one safe when removing a read of a variable is not.
//!
//! # Why after the whole allocator rather than inside it
//!
//! The two edits are decided in different places for different reasons, so neither of the two
//! decisions is wrong on its own and there is no one place inside the allocator that sees the
//! pair. Adjacency is also not a property either decision has: whether anything ends up between
//! them is settled by [`crate::finish`] writing every edit into the function, which is after the
//! allocator has finished. The pair is visible once, here, and nowhere earlier.
//!
//! # What it does not do
//!
//! Only the pair that is next to itself. A reload with an instruction between it and the spill is
//! left alone even when that instruction writes neither the register nor the slot, because
//! answering that needs liveness over the written function rather than a look at the line above.
//! A reload into a different register than the one spilled is left alone too, though a copy would
//! do instead of a load. Both belong to the post-allocation copy propagation of section 37.4,
//! which is a pass rather than a peephole, and this is deliberately the part of it that costs one
//! walk over the function.

use rucc_mir::{Block, Func, Inst};
use rucc_regalloc::assign::Place;
use rucc_regalloc::rewrite::Edit;
use rucc_target::{PhysReg, RegClass};

use crate::finish::Moves;

/// Takes out every reload of a slot the instruction in front of it spilled, and says how many.
pub fn dead(func: &mut Func, moves: &Moves) -> usize {
    let blocks: Vec<Block> = func.blocks().collect();
    let mut taken = 0;
    for block in blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        // What the instruction last looked at wrote out, or `None` when it was not a spill. Only
        // ever the instruction before, so a block starts with nothing and an instruction that is
        // not one of the allocator's moves clears it.
        let mut spilled: Option<(u32, PhysReg, RegClass)> = None;
        let mut gone: Vec<Inst> = Vec::new();
        for inst in insts {
            let edit = moves.at(inst);
            // The same slot, the same register and the same width as the spill in front of it,
            // which makes this a read of a word that register still holds.
            if spilled.is_some() && spilled == edit.and_then(reload) {
                gone.push(inst);
                // The spill stays what the next instruction is compared against, because taking
                // this one out is what puts the next one next to it. A value read back twice
                // running goes in one pass rather than one reload a pass.
                continue;
            }
            spilled = edit.and_then(spill);
        }
        for inst in gone {
            func.remove_inst(inst);
            taken += 1;
        }
    }
    taken
}

/// The slot a move writes a register out to, or `None` for a move that is not a spill.
fn spill(edit: Edit) -> Option<(u32, PhysReg, RegClass)> {
    match (edit.mov.to, edit.mov.from) {
        (Place::Slot(slot), Place::Reg(reg)) => Some((slot, reg, edit.class)),
        _ => None,
    }
}

/// The slot a move reads a register back in from, or `None` for a move that is not a reload.
fn reload(edit: Edit) -> Option<(u32, PhysReg, RegClass)> {
    match (edit.mov.to, edit.mov.from) {
        (Place::Reg(reg), Place::Slot(slot)) => Some((slot, reg, edit.class)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{Mem, Opcode, Operand, Reg};
    use rucc_regalloc::moves::Move;
    use rucc_regalloc::rewrite::At;
    use rucc_target::x86_64::{FRAME, GPR, RAX, XMM};

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

    /// How many instructions a block has left.
    fn left(func: &Func, block: Block) -> usize {
        func.insts(block).count()
    }

    /// The pair the whole pass is about: a word written out and read straight back into the
    /// register it was written out of.
    #[test]
    fn a_reload_of_the_slot_the_instruction_in_front_of_it_spilled_goes() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(dead(&mut func, &moves), 1);
        assert_eq!(left(&func, block), 1, "the spill went too, or the reload stayed");
        assert_eq!(func.insts(block).next(), Some(spill));
    }

    /// Two reads of one word, which is one spill and two reloads written next to each other, and
    /// the second is as dead as the first because taking the first out is what puts it next to the
    /// spill.
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

        assert_eq!(dead(&mut func, &moves), 2);
        assert_eq!(left(&func, block), 1);
    }

    /// The instruction between them is what the value was spilled for, and it may write the
    /// register, so the reload behind it is a read of a word that register no longer holds.
    #[test]
    fn a_reload_with_an_instruction_between_it_and_the_spill_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let between = op(&mut names, "add_rr_64");
        func.build(block, between)
            .def(Reg::physical(R10), GPR)
            .uses(Reg::physical(RAX), GPR)
            .finish();
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(dead(&mut func, &moves), 0);
        assert_eq!(left(&func, block), 3);
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

        assert_eq!(dead(&mut func, &moves), 0);
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

        assert_eq!(dead(&mut func, &moves), 0);
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

        assert_eq!(dead(&mut func, &moves), 0);
        assert_eq!(left(&func, block), 2);
    }

    /// The same two instructions, with nothing saying the allocator wrote them, which is what a
    /// write to a local variable and a read of it back look like.
    #[test]
    fn a_store_and_a_load_the_allocator_did_not_write_stay() {
        let (mut names, mut func, block) = empty();
        store(&mut func, &mut names, block, R10, 16);
        load(&mut func, &mut names, block, R10, 16);

        assert_eq!(dead(&mut func, &Moves::default()), 0);
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

        assert_eq!(dead(&mut func, &moves), 0);
        assert_eq!(left(&func, second), 1);
    }

    /// A copy is a move of the allocator's like any other and is not a spill, so the reload behind
    /// one is compared against nothing.
    #[test]
    fn a_copy_between_the_two_is_not_a_spill_to_read_back() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let copy = op(&mut names, "mov_rr_64");
        let copied = func
            .build(block, copy)
            .def(Reg::physical(RAX), GPR)
            .uses(Reg::physical(R10), GPR)
            .finish();
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(
            copied,
            Edit {
                at: At::StartOf(block),
                mov: Move::new(Place::Reg(RAX), Place::Reg(R10)),
                class: GPR,
            },
        );
        moves.record(reload, back(block, 0, R10));

        assert_eq!(dead(&mut func, &moves), 0);
        assert_eq!(left(&func, block), 3);
    }
}
