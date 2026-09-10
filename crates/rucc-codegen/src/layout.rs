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
//! # The test a comparison makes unnecessary
//!
//! Almost every branch a C program writes is on a comparison, and a comparison has already set
//! the flags by the time the byte it wrote is tested against itself. So where the instruction in
//! front of the branch is that comparison, and the branch is the whole of what reads its byte,
//! the byte and the test both go and the jump names the condition the comparison was asked about
//! instead of naming zero. Three instructions become two, and the two are what the machine has a
//! comparison and a conditional jump for.
//!
//! This is where it happens rather than anywhere earlier because of what the flags are. Between
//! the comparison and the jump they are live and they are not a register: no pass could be told
//! about them, so no pass may put an instruction between the two. After this one there is no pass
//! left, which is the whole of the argument, and it is the same argument
//! `rucc_target::x86_64::Form::CmpSet` is one form rather than two under.
//!
//! What this cannot work out for itself is whether the byte has another reader. Every register is
//! physical by the time this runs and a physical register is written many times in a function, so
//! the question has to be asked while they are still virtual and written once. [`fusable`] is that
//! question, asked before allocation, and its answer is one of the arguments to [`blocks`]. The
//! same arrangement, and for the same reason, as the addresses [`crate::finish`] has still to
//! write and [`crate::fold`] is handed.
//!
//! # Why it runs last
//!
//! [`crate::finish`] finds the blocks a function returns from by looking for the ones that go
//! nowhere. Nothing here creates one of those, but everything here reads and writes the arms, and
//! a pass that reorders them is one nothing before it should be looking at. Running the layout
//! after the prologue and the epilogue are in is also what makes the epilogue something it can
//! lay out around rather than something it has to leave room for.

use std::collections::{HashMap, HashSet};

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::{BranchInsts, Fusion, Role};

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
pub fn blocks(
    func: &mut mir::Func,
    insts: &BranchInsts,
    names: &mut Interner,
    fusable: &HashSet<mir::Inst>,
) {
    let table = table(insts, names);
    let mut order = order(func);
    let mut writer = Writer { func, insts, names, table, fusable };
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

/// The comparisons a branch may be folded into, which [`blocks`] can then find by opcode.
///
/// One entry per name the target's table holds, interned once for the function rather than once
/// per block, since a block that ends in a branch is most of the blocks there are.
fn table(insts: &BranchInsts, names: &mut Interner) -> HashMap<mir::Opcode, &'static Fusion> {
    insts
        .fused
        .iter()
        .map(|fusion| {
            (mir::Opcode::new(names.intern(&format!("{}{}", insts.prefix, fusion.set))), fusion)
        })
        .collect()
}

/// The comparisons a branch on their answer is the whole of what reads, which [`blocks`] may fold
/// the test out of.
///
/// Run before allocation, on the same function [`blocks`] is later given. What it answers is
/// whether anything but the branch reads the byte a comparison wrote, and that is a question about
/// a virtual register: a physical one is written many times in a function and counting its readers
/// would mean asking which of the writes each reader belongs to. So it is asked here, where a
/// register is written once, and the answer is carried to the pass that can use it.
///
/// Being on this list is necessary and not sufficient. Allocation may put a reload between the
/// comparison and the branch, and a comparison that is no longer the instruction in front of the
/// branch is not one the flags survive to, so [`blocks`] checks that again on what it finds.
#[must_use]
pub fn fusable(func: &mir::Func, insts: &BranchInsts, names: &mut Interner) -> HashSet<mir::Inst> {
    let table = table(insts, names);
    let branch = mir::Opcode::new(names.intern(&format!("{}{}", insts.prefix, insts.cond)));
    let reads = crate::fold::reads(func);
    let mut found = HashSet::new();
    for block in func.blocks() {
        let insts: Vec<mir::Inst> = func.insts(block).collect();
        let [.., compare, last] = insts[..] else { continue };
        if func[last].opcode != branch || !table.contains_key(&func[compare].opcode) {
            continue;
        }
        let operands = &func[func[compare].operands];
        let Some(byte) = operands.first().filter(|operand| operand.role != Role::Use) else {
            continue;
        };
        if !byte.reg.is_virtual() || reads.get(&byte.reg) != Some(&1) {
            continue;
        }
        // And it is this branch that reads it rather than one in some other block, which the
        // count alone does not say.
        if func[func[last].operands].first().map(|operand| operand.reg) == Some(byte.reg) {
            found.insert(compare);
        }
    }
    found
}

/// The one thing that writes an instruction here, over the function it writes into.
struct Writer<'a> {
    func: &'a mut mir::Func,
    insts: &'a BranchInsts,
    names: &'a mut Interner,
    table: HashMap<mir::Opcode, &'static Fusion>,
    fusable: &'a HashSet<mir::Inst>,
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
        // Asked before the branch is taken out, because what it looks at is the instruction in
        // front of the branch and taking the branch out would make that the last one.
        let fused = self.fused(block);
        let condition = self.take(block);

        // Whichever arm is laid out next is the one the block falls into, and the jump is then
        // the one taken when the condition sends it the other way. Falling into the arm the
        // condition is false for leaves the jump taken when it holds, and falling into the arm it
        // is true for leaves the other jump and the arms the other way round.
        let (if_true, if_false) = match fused {
            Some((_, fusion)) => (fusion.if_true, fusion.if_false),
            None => (self.insts.if_true, self.insts.if_false),
        };
        let arms: Vec<mir::Block> = self.func[block].succs.iter().map(|arm| arm.block).collect();
        let (name, bridge) = if next == Some(arms[1]) {
            (if_true, None)
        } else if next == Some(arms[0]) {
            self.func.succs_mut(block).swap(0, 1);
            (if_false, None)
        } else {
            (if_true, Some(self.bridge(block)))
        };

        match fused {
            Some((compare, fusion)) => self.keep_only_the_flags(compare, fusion),
            None => {
                let opcode = self.opcode(self.insts.test);
                self.func.build(block, opcode).operand(condition).finish();
            }
        }
        let opcode = self.opcode(name);
        self.func.build(block, opcode).finish();
        bridge
    }

    /// The comparison the block's branch can be folded into, when there is one.
    ///
    /// Three things have to hold and [`fusable`] has already answered the one that cannot be
    /// answered here. What is left is that the comparison is still the instruction in front of the
    /// branch, since allocation may have put a reload between them and the flags do not survive
    /// one, and that the byte the branch reads is the byte that comparison wrote, since the
    /// allocator has since given both of them a physical register and two registers that were
    /// different could have become the same one.
    fn fused(&self, block: mir::Block) -> Option<(mir::Inst, &'static Fusion)> {
        let insts: Vec<mir::Inst> = self.func.insts(block).collect();
        let [.., compare, last] = insts[..] else { return None };
        if !self.fusable.contains(&compare) {
            return None;
        }
        let fusion = *self.table.get(&self.func[compare].opcode)?;
        let byte = self.func[self.func[compare].operands].first()?.reg;
        (self.func[self.func[last].operands].first()?.reg == byte).then_some((compare, fusion))
    }

    /// Turns a comparison that wrote a byte into the same comparison that writes nothing.
    ///
    /// The instruction stays where it is and keeps its immediate, which is the point: what it does
    /// to the flags is what it already did, and the jump written behind it reads those. Only the
    /// operand at the front goes, which is the byte, and the opcode changes to the one that has no
    /// operand there.
    fn keep_only_the_flags(&mut self, compare: mir::Inst, fusion: &Fusion) {
        let read: Vec<mir::Operand> =
            self.func[self.func[compare].operands].iter().skip(1).copied().collect();
        let operands = self.func.push_operands(&read);
        self.func[compare].opcode = self.opcode(fusion.cmp);
        self.func[compare].operands = operands;
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
    use rucc_target::x86_64::{BRANCH, GPR, RAX, RCX, REGS};

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
        // Both halves, in the order the pipeline runs them, so that a test which builds a
        // comparison in front of its branch sees what a compiled function would see.
        let fusable = fusable(func, &BRANCH, names);
        blocks(func, &BRANCH, names, &fusable);
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

        let fusable = fusable(&func, &BRANCH, &mut names);
        blocks(&mut func, &BRANCH, &mut names, &fusable);

        let test = func.insts(made[0]).next().expect("a test");
        let operands = func[test].operands;
        assert_eq!(func[operands], [Operand::read(Reg::physical(RAX), GPR)]);
    }

    #[test]
    fn a_block_nothing_reaches_is_laid_out_at_the_end_rather_than_deleted() {
        let (mut names, mut func, made) = blank(4);
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[3])];

        let fusable = fusable(&func, &BRANCH, &mut names);
        blocks(&mut func, &BRANCH, &mut names, &fusable);

        // Blocks one and two are reached by nothing, so they go last, in the order they were
        // made. Deleting one would be a decision about what the program does, and this pass has
        // no business making it.
        assert_eq!(order_of(&func), [0, 3, 1, 2]);
    }

    #[test]
    fn a_function_with_no_blocks_is_left_alone() {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));

        let fusable = fusable(&func, &BRANCH, &mut names);
        blocks(&mut func, &BRANCH, &mut names, &fusable);

        assert_eq!(func.block_count(), 0);
    }

    #[test]
    #[should_panic(expected = "a block with 3 arms")]
    fn a_block_with_three_arms_is_refused_rather_than_laid_out_wrongly() {
        let (mut names, mut func, made) = blank(4);
        branch(&mut func, &mut names, made[0], &[made[1], made[2], made[3]]);

        let fusable = fusable(&func, &BRANCH, &mut names);
        blocks(&mut func, &BRANCH, &mut names, &fusable);
    }

    #[test]
    #[should_panic(expected = "whose last instruction is not the branch")]
    fn a_block_with_two_arms_and_no_branch_in_it_is_refused() {
        let (mut names, mut func, made) = blank(3);
        let opcode = Opcode::new(names.intern("x64.nop"));
        func.build(made[0], opcode).finish();
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1]), BlockCall::to(made[2])];

        let fusable = fusable(&func, &BRANCH, &mut names);
        blocks(&mut func, &BRANCH, &mut names, &fusable);
    }

    /// Puts a comparison and a branch on its answer at the end of a block.
    ///
    /// The byte is a virtual register, which is what it is when [`fusable`] is asked and is not
    /// what it is when [`blocks`] runs. Nothing in either half cares which it is except the
    /// counting, so a test that runs both over one function has to use the register the counting
    /// wants, and what it costs is that this is one thing the unit tests cannot check about the
    /// two halves running at different times. `crate::pipeline` runs them the real way round.
    fn compare(
        func: &mut mir::Func,
        names: &mut Interner,
        block: mir::Block,
        arms: &[mir::Block],
    ) -> Reg {
        let byte = func.new_vreg(GPR);
        let opcode = Opcode::new(names.intern("x64.cmp_set_l_32"));
        func.build(block, opcode)
            .def(byte, GPR)
            .operand(Operand::read(Reg::physical(RAX), GPR))
            .operand(Operand::read(Reg::physical(RCX), GPR))
            .finish();
        let opcode = Opcode::new(names.intern("x64.br_cond_8"));
        func.build(block, opcode).operand(Operand::read(byte, GPR)).finish();
        *func.succs_mut(block) = arms.iter().map(|&arm| BlockCall::to(arm)).collect();
        byte
    }

    /// A branch on a comparison is the comparison and a jump on what it found.
    ///
    /// Three instructions go in and two come out. The byte goes because nothing reads it, the test
    /// goes because the comparison set the flags the test was going to set, and the jump names the
    /// condition rather than naming zero. Which condition it names is the opposite of the one the
    /// comparison asked about, since the block falls into the arm the comparison is true for.
    #[test]
    fn a_branch_on_a_comparison_is_the_comparison_and_a_jump_on_what_it_found() {
        let (mut names, mut func, made) = blank(3);
        compare(&mut func, &mut names, made[0], &[made[1], made[2]]);

        let text = laid_out(&mut func, &mut names);

        assert_eq!(
            text,
            [
                "block0:",
                "x64.cmp_rr_32 $rax, $rcx",
                "x64.jcc_ge block2, block1",
                "block1:",
                "block2:",
            ]
        );
    }

    /// The same comparison with something else reading its answer, which keeps everything.
    ///
    /// Folding the byte away when a second instruction wants it would be deleting a value the
    /// program computes. This is the whole of what [`fusable`] is asked before allocation, and the
    /// second reader here is in another block so that it is a question about the function rather
    /// than about the block the branch is in.
    #[test]
    fn a_comparison_whose_answer_something_else_reads_keeps_its_byte_and_its_test() {
        let (mut names, mut func, made) = blank(3);
        let byte = compare(&mut func, &mut names, made[0], &[made[1], made[2]]);
        let opcode = Opcode::new(names.intern("x64.mov_rr_64"));
        func.build(made[1], opcode)
            .def(Reg::physical(RAX), GPR)
            .operand(Operand::read(byte, GPR))
            .finish();

        let text = laid_out(&mut func, &mut names);

        assert!(text.contains(&"x64.test_rr_8 %0".to_owned()), "{text:?}");
        assert!(text.contains(&"x64.jcc_e block2, block1".to_owned()), "{text:?}");
    }

    /// A comparison allocation moved away from its branch, which keeps its test.
    ///
    /// [`fusable`] says the byte has one reader and says nothing about where the two instructions
    /// end up, because allocation runs between the two halves and may put a reload in front of the
    /// branch. The flags do not survive one, so the second half looks again, and this is the case
    /// where it finds something and refuses. The instruction is put in between the two calls
    /// because that is when allocation would have put it there.
    #[test]
    fn a_comparison_that_is_no_longer_in_front_of_its_branch_keeps_its_test() {
        let (mut names, mut func, made) = blank(3);
        compare(&mut func, &mut names, made[0], &[made[1], made[2]]);
        let fusable = fusable(&func, &BRANCH, &mut names);
        assert_eq!(fusable.len(), 1, "the comparison is one the byte's count allows");

        let branch = func.terminator(made[0]).expect("a block with two arms has a branch");
        let opcode = Opcode::new(names.intern("x64.mov_rr_64"));
        let reload = func
            .build_loose(opcode)
            .def(Reg::physical(RCX), GPR)
            .operand(Operand::read(Reg::physical(RAX), GPR))
            .finish();
        func.insert_before(branch, reload);
        blocks(&mut func, &BRANCH, &mut names, &fusable);
        let text = mir::print_func(&func, &names, &REGS);

        assert!(text.contains("x64.cmp_set_l_32"), "{text}");
        assert!(text.contains("x64.test_rr_8"), "{text}");
        assert!(!text.contains("x64.cmp_rr_32"), "{text}");
    }
}
