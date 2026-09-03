//! Instruction selection, scheduling, block layout, frames and prologue emission.
//!
//! Design: `spec/10-backend.md`. Layer rank 11, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! The lowering tables are here. `rules/x86-64.rules` is compiled into a matching automaton when
//! this crate is built, and [`select`] is the walk over it: hand it a term and it gives back the
//! rule that fires and what the pattern bound. No lowering is written as `match` arms in this
//! crate and none ever will be, which is the settled decision `spec/10-backend.md` section 10.2
//! records.
//!
//! The selector is here too. [`lower`] walks a function and builds machine IR out of what the
//! table gives back, and [`term`] is how an IR instruction is shown to the matcher. Between them
//! they cover the arithmetic the rule file covers, which is every integer operation at every
//! width the machine has one for.
//!
//! Loads and stores are covered too, and they are the first rules with an effect. What one of
//! those claims is settled the same way everything else is: a term may compute a memory rather
//! than a value, and the two halves of a rule have to agree about which they computed. A return
//! is covered as well, and it is the first rule about the calling convention: what it claims is
//! that the value comes through unchanged, and which register it comes through is a target fact
//! [`rucc_target::x86_64`] states and a test there checks against both conventions.
//!
//! The branches are covered, and they are the rules with the least in them. Where a block goes is
//! on the block in machine IR rather than on its terminator, so a rule for a branch never names a
//! block and an unconditional jump is not a rule at all: the edge is the whole of it. What is
//! left of a conditional branch is the condition, which is what its rule is about. What is still
//! to come is the calls, and a function with one in it is reported as one this cannot lower.
//!
//! [`split`] is what has to run between lowering and allocation now that there are branches. An
//! edge that carries values into a block arrived at more than one way, out of a block that leaves
//! more than one way, has nowhere to put the moves those values turn into, so it is split into two
//! edges that do.
//!
//! [`abi`] is the other side of the same convention and the one part of it that is not a rule at
//! all. Which register an argument arrives in depends on its position and on the classification
//! of every argument before it, and a rule matches one term and can see none of that, so the
//! arguments are built from what [`rucc_target::CallRegs`] says. A function's parameters are
//! bound to the registers they arrived in before its first instruction is looked at, which is
//! what makes a function that takes arguments one this can compile at all: the allocator refuses
//! an entry block with parameters on it, because there is no edge into an entry block for the
//! moves that give a block parameter its value to go on.
//!
//! [`frame`] is what a function's stack looks like while it runs: which registers the prologue has
//! to put back, where every spilled value went, and how many bytes the stack pointer moves. It is
//! worked out after allocation because the largest area in most frames is the spill slots and
//! nothing knows how many of those there are until the allocator has finished running out of
//! registers.
//!
//! [`finish`] writes that frame into the function: the prologue that takes it, the moves the
//! allocator handed back as edits, and the epilogue at the end of every block the function
//! returns from. After it every register is physical and every offset into the frame is a
//! constant, which is the point at which a function is one an encoder could read.
//!
//! What is not here yet is the block layout, which lands in M3.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-codegen/0.3.4")]

pub mod abi;
pub mod finish;
pub mod frame;
pub mod lower;
pub mod select;
pub mod split;
pub mod term;

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
