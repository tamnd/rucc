//! Putting the blocks in an order, and turning the edges between them into jumps.
//!
//! Design: `spec/10-backend.md` section 10.6.
//!
//! Up to here a function is a set of blocks and a set of edges, and nothing has said which block
//! comes first in memory. A machine has no such thing: it runs the instruction after the one it
//! just ran, so an order is not a presentation detail but the last piece of what the function
//! means. This is what chooses one, and then writes the jumps that make the edges the order did
//! not put next to each other still go where they went.
//!
//! # What the order is
//!
//! Reverse postorder over the CFG, with each block's successors walked in reverse, and anything
//! unreachable put at the end in block order.
//!
//! That is the `-O0` order `spec/10-backend.md` section 10.3 asks for, and it is not arbitrary.
//! Walking the successors in reverse is what makes the first arm of a branch come out first,
//! because a depth-first walk finishes its last child first and reverse postorder then puts that
//! child last. So an `if` with no `else` falls through into its body, and a loop comes out as its
//! header, its body and then whatever follows it, which is the shape where the back edge is the
//! only jump in it. The chain construction weighted by block frequency that section 10.6
//! describes is what replaces this above `-O0`, and it is not written yet.
//!
//! Unreachable blocks are laid out rather than deleted. Deleting one is a decision about what the
//! program does and this pass has no business making it, and a block nothing reaches costs the
//! bytes it occupies and nothing else.
//!
//! # What a block looks like afterwards
//!
//! A block still holds where it goes, and it still holds every arm, which is what keeps the
//! control flow graph readable after this has run. What changes is that the order the arms are in
//! now means something it did not mean before:
//!
//! ```text
//!   no arms      it returns
//!   one arm      it falls into that block if that block is next, and jumps to it if not
//!   two arms     a test and a conditional jump to the first, and the second is always next
//! ```
//!
//! So a jump target is a block without an instruction growing a field for one.
//! `rucc_mir::InstData` is twenty four bytes by assertion and a block reference does not fit in
//! it, and every pass over the graph already reads the arms, so putting the target where the
//! graph already is costs nothing and keeps the two from disagreeing.
//!
//! Which arm is which is no longer which way the condition went, because a block that falls into
//! the arm the condition is true for is a block whose jump has to be taken when it is false. That
//! is what the two conditional jumps in [`BranchInsts`] are for, and it is why the arms may come
//! out swapped: what the condition meant is in the opcode afterwards, and what the arms mean is
//! where the jump goes and what comes next.
//!
//! # The block a branch sometimes needs
//!
//! A branch whose second arm cannot be laid out next, because both its arms are blocks the walk
//! has already been to, would need two jumps in one block. Rather than write one, this makes the
//! block it needs: an empty one on the second edge, laid out immediately after the branch, that
//! jumps where the edge went. That is exactly the critical edge splitting in [`crate::split`],
//! done for a different reason, and it costs the same jump the second jump would have cost while
//! leaving every block with at most one.
//!
//! # Why it runs last
//!
//! [`crate::finish`] finds the blocks a function returns from by looking for the ones that go
//! nowhere. Nothing here creates one of those, but everything here reads and writes the arms, and
//! a pass that reorders them is one nothing before it should be looking at. Running the layout
//! after the prologue and the epilogue are in is also what makes the epilogue something it can
//! lay out around rather than something it has to leave room for.

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::BranchInsts;

/// Puts a function's blocks in an order and writes the jumps that order needs.
///
/// Run last, after [`crate::finish`].
///
/// # Panics
///
/// Panics on a block with more than two successors, which nothing lowers to yet, and on a block
/// with two whose last instruction is not the conditional branch the target named. Both are a
/// function that was built wrongly somewhere earlier, and both are worth finding here rather than
/// as a jump to the wrong place.
pub fn blocks(func: &mut mir::Func, insts: &BranchInsts, names: &mut Interner) {
    let mut order = order(func);
    let mut writer = Writer { func, insts, names };
    let mut at = 0;
    while at < order.len() {
        // A branch that can fall into neither arm asks for a block to put the second jump in, and
        // that block goes immediately after it, which is where the loop reaches it next.
        if let Some(bridge) = writer.edges(order[at], order.get(at + 1).copied()) {
            order.insert(at + 1, bridge);
        }
        at += 1;
    }
    func.set_block_order(&order);
}

/// The order the blocks are laid out in, which is every block the function has exactly once.
fn order(func: &mir::Func) -> Vec<mir::Block> {
    let mut order = Vec::with_capacity(func.block_count());
    let mut seen = vec![false; func.block_count()];
    if let Some(entry) = func.entry() {
        seen[entry.index()] = true;
        // The walk is explicit rather than recursive because a function with a hundred thousand
        // blocks in it is a function somebody generated, and it should compile rather than run out
        // of stack. Each entry is a block and how many of its arms have been started.
        let mut stack = vec![(entry, 0usize)];
        while let Some((block, next)) = stack.pop() {
            let succs = &func[block].succs;
            let Some(arm) = succs.len().checked_sub(next + 1) else {
                order.push(block);
                continue;
            };
            stack.push((block, next + 1));
            let to = succs[arm].block;
            if !std::mem::replace(&mut seen[to.index()], true) {
                stack.push((to, 0));
            }
        }
        order.reverse();
    }
    // Whatever the walk did not reach, in the order the blocks were made, which is the only order
    // there is anything to be said for when nothing goes to any of them.
    order.extend(func.blocks().filter(|block| !seen[block.index()]));
    order
}

/// The one thing that writes an instruction here, over the function it writes into.
struct Writer<'a> {
    func: &'a mut mir::Func,
    insts: &'a BranchInsts,
    names: &'a mut Interner,
}

impl Writer<'_> {
    /// Writes the jumps one block needs, given the block laid out after it, and gives back the
    /// block that has to go between the two when the branch needed one.
    fn edges(&mut self, block: mir::Block, next: Option<mir::Block>) -> Option<mir::Block> {
        match self.func[block].succs.len() {
            0 => None,
            1 => {
                self.one(block, next);
                None
            }
            2 => self.two(block, next),
            arms => panic!("a block with {arms} arms, and nothing lowers to one"),
        }
    }

    /// A block that goes to one place, which either follows it or has to be jumped to.
    fn one(&mut self, block: mir::Block, next: Option<mir::Block>) {
        if Some(self.func[block].succs[0].block) == next {
            return;
        }
        let opcode = self.opcode(self.insts.jump);
        self.func.build(block, opcode).finish();
    }

    /// A block that goes to two places, which is a test and a jump to one of them.
    ///
    /// The condition is read off the branch the rules selected and the branch is taken out, so the
    /// register the test reads is the one the branch read and no new value is made. That is what
    /// makes this safe to run after allocation: it writes no register that was not already
    /// written and it asks for none that was not already asked for.
    fn two(&mut self, block: mir::Block, next: Option<mir::Block>) -> Option<mir::Block> {
        let condition = self.take(block);

        // Whichever arm is laid out next is the one the block falls into, and the jump is then
        // the one taken when the condition sends it the other way. Falling into the arm the
        // condition is false for leaves the jump taken when it holds, and falling into the arm it
        // is true for leaves the other jump and the arms the other way round.
        let arms: Vec<mir::Block> = self.func[block].succs.iter().map(|arm| arm.block).collect();
        let (name, bridge) = if next == Some(arms[1]) {
            (self.insts.if_true, None)
        } else if next == Some(arms[0]) {
            self.func.succs_mut(block).swap(0, 1);
            (self.insts.if_false, None)
        } else {
            (self.insts.if_true, Some(self.bridge(block)))
        };

        let opcode = self.opcode(self.insts.test);
        self.func.build(block, opcode).operand(condition).finish();
        let opcode = self.opcode(name);
        self.func.build(block, opcode).finish();
        bridge
    }

    /// Takes the conditional branch off the end of a block and gives back what it read.
    fn take(&mut self, block: mir::Block) -> mir::Operand {
        let branch = self.func.terminator(block).expect("a block with two arms has a branch");
        let cond = self.opcode(self.insts.cond);
        assert_eq!(
            self.func[branch].opcode, cond,
            "a block with two arms whose last instruction is not the branch"
        );
        let operands = self.func[branch].operands;
        let condition = self.func[operands][0];
        self.func.remove_inst(branch);
        condition
    }

    /// Puts an empty block on a branch's second edge, so that the branch has something to fall
    /// into and the jump the edge really needs is in a block of its own.
    fn bridge(&mut self, block: mir::Block) -> mir::Block {
        let bridge = self.func.create_block();
        let edge = self.func[block].succs[1].clone();
        *self.func.succs_mut(bridge) = vec![edge];
        self.func.succs_mut(block)[1] = mir::BlockCall::to(bridge);
        bridge
    }

    /// The opcode of that name on this target, which is the name with the target's prefix in
    /// front of it.
    fn opcode(&mut self, name: &str) -> mir::Opcode {
        mir::Opcode::new(self.names.intern(&format!("{}{name}", self.insts.prefix)))
    }
}

#[cfg(test)]
mod tests {
    use rucc_mir::{BlockCall, Opcode, Operand, Reg};
    use rucc_target::x86_64::{BRANCH, GPR, RAX, REGS};

    use super::*;

    /// A function with that many blocks, none of which goes anywhere yet.
    fn blank(count: usize) -> (Interner, mir::Func, Vec<mir::Block>) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let blocks = (0..count).map(|_| func.create_block()).collect();
        (names, func, blocks)
    }

    /// Puts a conditional branch at the end of a block, on a register that is already physical
    /// the way one is by the time this pass runs.
    fn branch(func: &mut mir::Func, names: &mut Interner, block: mir::Block, arms: &[mir::Block]) {
        let opcode = Opcode::new(names.intern("x64.br_cond_8"));
        func.build(block, opcode).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
        *func.succs_mut(block) = arms.iter().map(|&arm| BlockCall::to(arm)).collect();
    }

    /// Laying the blocks out for the one machine this crate has, and the dump of what came out.
    ///
    /// The dump rather than the function, because where a jump goes is on the block and the dump
    /// is the one place the instruction and the arm are put back together. A test that read the
    /// two separately would pass on a function whose jump and whose edge disagreed, which is the
    /// mistake this pass is most able to make.
    ///
    /// A block is named in the dump by where it is in the layout rather than by the number it was
    /// made with, which is why every expectation below reads that way and why the order is worth
    /// asserting on its own.
    fn laid_out(func: &mut mir::Func, names: &mut Interner) -> Vec<String> {
        blocks(func, &BRANCH, names);
        mir::print_func(func, names, &REGS)
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with("mfunc") && *line != "}")
            .map(|line| line.trim().to_string())
            .collect()
    }

    /// The blocks in layout order, by the number each was made with.
    fn order_of(func: &mir::Func) -> Vec<usize> {
        func.blocks().map(mir::Block::index).collect()
    }

    #[test]
    fn a_block_that_falls_into_the_next_one_gets_no_jump_at_all() {
        let (mut names, mut func, made) = blank(2);
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1])];

        let text = laid_out(&mut func, &mut names);

        // The arm is still on the block, because the graph is still worth reading, and there is
        // no instruction on it because the block it goes to is the one that runs next anyway.
        assert_eq!(text, ["block0:", "block1", "block1:"]);
    }

    #[test]
    fn a_block_that_goes_somewhere_that_is_not_next_gets_a_jump() {
        let (mut names, mut func, made) = blank(2);
        // A loop with nothing in it and no way out, which is the smallest function there is with
        // an edge that runs backwards. Every layout puts the two blocks in this order, so the
        // second one has nothing after it and its edge has to be a jump.
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1])];
        *func.succs_mut(made[1]) = vec![BlockCall::to(made[0])];

        let text = laid_out(&mut func, &mut names);

        assert_eq!(text, ["block0:", "block1", "block1:", "x64.jmp block0"]);
    }

    #[test]
    fn a_branch_that_falls_into_its_false_arm_jumps_when_the_condition_holds() {
        let (mut names, mut func, made) = blank(3);
        // A loop whose body is the block it came from: the arm taken when the condition holds is
        // a block the walk has already been to, so the other arm is what comes next.
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1])];
        branch(&mut func, &mut names, made[1], &[made[0], made[2]]);

        let text = laid_out(&mut func, &mut names);

        assert_eq!(order_of(&func), [0, 1, 2]);
        assert_eq!(
            text,
            [
                "block0:",
                "block1",
                "block1:",
                "x64.test_rr_8 $rax",
                "x64.jcc_ne block0, block2",
                "block2:",
            ]
        );
    }

    #[test]
    fn a_branch_that_falls_into_its_true_arm_jumps_when_the_condition_does_not_hold() {
        let (mut names, mut func, made) = blank(3);
        branch(&mut func, &mut names, made[0], &[made[1], made[2]]);

        let text = laid_out(&mut func, &mut names);

        // The arms come out swapped, because after this the first is where the jump goes and the
        // second is what runs next, and the jump is the one taken when the condition failed.
        assert_eq!(order_of(&func), [0, 1, 2]);
        assert_eq!(
            text,
            ["block0:", "x64.test_rr_8 $rax", "x64.jcc_e block2, block1", "block1:", "block2:"]
        );
    }

    #[test]
    fn a_branch_that_can_fall_into_neither_arm_is_given_a_block_to_jump_from() {
        let (mut names, mut func, made) = blank(2);
        // A loop that goes back to the top or round again, so both arms are blocks the walk has
        // already been to and nothing is left to lay out after it.
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1])];
        branch(&mut func, &mut names, made[1], &[made[0], made[1]]);

        let text = laid_out(&mut func, &mut names);

        // Block two is the one this made. It is empty, it is laid out where the branch falls into
        // it, and the jump the second arm needed is in it rather than being a second jump in the
        // block above.
        assert_eq!(order_of(&func), [0, 1, 2]);
        assert_eq!(
            text,
            [
                "block0:",
                "block1",
                "block1:",
                "x64.test_rr_8 $rax",
                "x64.jcc_ne block0, block2",
                "block2:",
                "x64.jmp block1",
            ]
        );
    }

    #[test]
    fn the_test_reads_the_register_the_branch_read() {
        let (mut names, mut func, made) = blank(3);
        branch(&mut func, &mut names, made[0], &[made[1], made[2]]);

        blocks(&mut func, &BRANCH, &mut names);

        let test = func.insts(made[0]).next().expect("a test");
        let operands = func[test].operands;
        assert_eq!(func[operands], [Operand::read(Reg::physical(RAX), GPR)]);
    }

    #[test]
    fn a_block_nothing_reaches_is_laid_out_at_the_end_rather_than_deleted() {
        let (mut names, mut func, made) = blank(4);
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[3])];

        blocks(&mut func, &BRANCH, &mut names);

        // Blocks one and two are reached by nothing, so they go last, in the order they were
        // made. Deleting one would be a decision about what the program does, and this pass has
        // no business making it.
        assert_eq!(order_of(&func), [0, 3, 1, 2]);
    }

    #[test]
    fn a_function_with_no_blocks_is_left_alone() {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));

        blocks(&mut func, &BRANCH, &mut names);

        assert_eq!(func.block_count(), 0);
    }

    #[test]
    #[should_panic(expected = "a block with 3 arms")]
    fn a_block_with_three_arms_is_refused_rather_than_laid_out_wrongly() {
        let (mut names, mut func, made) = blank(4);
        branch(&mut func, &mut names, made[0], &[made[1], made[2], made[3]]);

        blocks(&mut func, &BRANCH, &mut names);
    }

    #[test]
    #[should_panic(expected = "whose last instruction is not the branch")]
    fn a_block_with_two_arms_and_no_branch_in_it_is_refused() {
        let (mut names, mut func, made) = blank(3);
        let opcode = Opcode::new(names.intern("x64.nop"));
        func.build(made[0], opcode).finish();
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1]), BlockCall::to(made[2])];

        blocks(&mut func, &BRANCH, &mut names);
    }
}
