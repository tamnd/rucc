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
    /// The comparisons a branch on their answer can be folded into, and what each pair becomes.
    ///
    /// Empty is a target that does not do this, and the layout then writes the test every time.
    pub fused: &'static [Fusion],
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
