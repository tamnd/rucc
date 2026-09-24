//! The instructions a laid out branch is made of.
//!
//! Design: `spec/10-backend.md` sections 10.6 and 10.8.
//!
//! A lowering rule for a conditional branch says one thing, which is what the branch is on. Where
//! its two arms go is on the block rather than in the instruction, and which of them the block
//! falls through to is not knowable until every block of the function has been put in an order.
//! So the instructions that actually branch are chosen by the block layout, after allocation, and
//! they are named here for the same reason [`crate::FrameInsts`] names a push: the crate that
//! writes them is a pipeline crate and `spec/10-backend.md` section 10.8 says a pipeline crate
//! holds no target-specific code.
//!
//! # What each one has to be
//!
//! The shapes are fixed, because the code that writes them writes one shape each. The test reads
//! one register and sets whatever the machine's condition state is. The three jumps read nothing
//! and write nothing, and where each goes is the first successor of the block it ends, which is
//! how every other arm is already carried.
//!
//! Two conditional jumps rather than one, because which one a block ends with depends on which
//! arm the layout put next. A block that falls into the arm taken when the condition does not
//! hold ends with the jump that is taken when it does, and a block that falls into the other arm
//! ends with the other jump. Neither is more natural than the other and a target that could only
//! name one would force the layout to lay every second branch out backwards.
//!
//! After the layout has run, a block that ends in a conditional jump has exactly two successors:
//! the first is where the jump goes, and the second is the block laid out next, which is where it
//! goes when the jump is not taken. There is never a second jump in the same block, because the
//! layout makes a block for one rather than writing it.
//!
//! # The jump the layout did not write
//!
//! An `asm` template may write one itself, which is how a loop a program spelled out by hand
//! reaches the machine IR: the lowering turns each label into a block and each jump into a block
//! with two arms whose last instruction is already the jump. [`BranchInsts::conditional`] is the
//! list the layout reads to tell one of those, and what it does then is nothing at all, beyond the
//! block on the second arm that every two-armed block needs when neither arm is laid out next.
//! The arms are in the order the jump means, taken first, so the shape is the one above already.
//!
//! # The condition state is not an operand
//!
//! Nothing here mentions the flags, on a machine that has them or on one that does not. What
//! makes that sound is that the test and the jump that reads it are written next to each other,
//! by one pass, after the allocator has finished, so there is nothing left in the compiler that
//! could put an instruction between them.
//!
//! # The test a comparison makes unnecessary
//!
//! Almost every branch in a C program is on a comparison, and a comparison already sets the
//! condition state. The byte a rule selects for it, the test of that byte against itself and the
//! jump on the answer are three instructions where the machine wanted two, and the two it wanted
//! are the comparison with nothing kept and a jump on the condition the comparison was asked
//! about.
//!
//! [`Fusion`] is that pair written down, one entry per comparison a rule can select. The layout
//! looks for one when the instruction in front of the branch is a comparison whose byte the
//! branch is the whole of what reads, and writes the two instructions in the entry instead of the
//! three it found. Which of the two jumps it writes is the same question as before and gets the
//! same answer, so an entry names both.
//!
//! # The select a comparison makes unnecessary
//!
//! A rule selects a choice between two values as a test of a condition byte and a conditional move
//! on the answer, because the byte is the only thing a rule can name. When the byte came from a
//! comparison that is the same three instructions a branch was, a comparison keeping a byte, a
//! test of it and an instruction that reads what the test left, and the same two do the work: the
//! comparison keeping nothing and a move on the condition the comparison was asked about.
//!
//! [`Move`] is that pair written down, one entry per select and condition. The condition is named
//! by the jump a [`Fusion`] takes when its comparison held, so a comparison has one name for what
//! it asked whether a branch or a select reads it.
//!
//! It stays a table rather than becoming an operation on the names. `cmp_set_ae_ri_64` and
//! `cmp_ri_64` and `jcc_ae` are strings a target chose and not a spelling anything here may
//! derive, and a target whose comparisons are shaped differently, or which has no condition state
//! at all, writes a shorter table or an empty one.

/// Every instruction a laid out branch is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchInsts {
    /// What a rule file and the machine IR put in front of this target's opcodes, such as `x64.`,
    /// which says which target a term belongs to and is not part of the opcode.
    pub prefix: &'static str,
    /// What a lowering rule selects for a conditional branch, which is what the layout replaces.
    ///
    /// It reads the condition and does nothing, which is as much of a branch as a rule can say.
    /// Naming it here is what lets the layout find one and be sure it has found one, rather than
    /// assuming that whatever a two-armed block ends with must be the branch.
    pub cond: &'static str,
    /// Reads the register the branch is on and sets the condition state from whether it is zero.
    pub test: &'static str,
    /// Goes to the block's first successor when the condition held.
    pub if_true: &'static str,
    /// Goes to the block's first successor when the condition did not hold.
    pub if_false: &'static str,
    /// Goes to the block's first successor.
    pub jump: &'static str,
    /// Goes to the address in its one operand, which is one of the block's successors and which
    /// of them is not known until the program runs.
    ///
    /// The one branch here the layout does not write. A computed `goto` is selected as this
    /// instruction, because what it reads is a value and reading a value is what selection is for,
    /// and the layout only has to know the name so that it can tell a block that already ends in
    /// one from a block that still wants a jump.
    pub indirect: &'static str,
    /// Every jump that reads the condition state and goes to the block's first successor when what
    /// it reads holds.
    ///
    /// [`Self::if_true`] and [`Self::if_false`] are two of these and the entries below name the
    /// rest, since a condition and its opposite are both jumps of this kind. The layout writes
    /// those two itself and reads this list for the other question: whether the block it is
    /// looking at already ends in one. A block does when an `asm` template wrote the jump, which
    /// is how a loop a program spelled out by hand arrives here, and the layout then writes
    /// nothing in front of it. Empty is a target whose templates never end a block that way.
    pub conditional: &'static [&'static str],
    /// The comparisons a branch on their answer can be folded into, and what each pair becomes.
    ///
    /// Empty is a target that does not do this, and the layout then writes the test every time.
    pub fused: &'static [Fusion],
    /// The conditional moves a select on a comparison's answer can become.
    ///
    /// Empty is a target that does not do this, and every select keeps the test of its byte.
    pub moves: &'static [Move],
}

/// A comparison, and the two instructions a branch on its answer becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fusion {
    /// The comparison a rule selects, which writes a byte saying what it found.
    pub set: &'static str,
    /// The same comparison with the byte gone, which sets the condition state and keeps nothing.
    ///
    /// Its operands are the ones the comparison read, in the same order, with the destination at
    /// the front taken off. The layout rewrites nothing else about them.
    pub cmp: &'static str,
    /// Goes to the block's first successor when the comparison held.
    pub if_true: &'static str,
    /// Goes to the block's first successor when the comparison did not hold.
    pub if_false: &'static str,
}

/// A select, a condition, and the move that makes the choice straight off that condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    /// What a rule selects for a choice on a byte, which tests the byte and then moves.
    pub select: &'static str,
    /// The condition, named by the jump a [`Fusion`] takes when its comparison held.
    pub when: &'static str,
    /// The same move reading the condition state a comparison left, with no test in front of it.
    ///
    /// Its operands are the select's without the byte at the end, in the same order.
    pub cmov: &'static str,
}
