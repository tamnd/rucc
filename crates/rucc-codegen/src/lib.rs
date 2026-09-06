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
//! left of a conditional branch is the condition, which is what its rule is about.
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
//! The calls are built there too, and for the same reason: a rule pattern sees one term and a
//! call's operands are whatever the signature made them. What the callee is free to destroy is
//! written into the call as a definition of each of those registers, which is the whole of what
//! the allocator needs to keep a value that outlives the call somewhere else. What passes on the
//! stack is refused rather than passed wrongly, on this side as on the other.
//!
//! The addresses are the other thing [`lower`] builds by name rather than by rule, and there are
//! two of them. The address of a local is a `lea` off the stack pointer with a displacement the
//! frame fills in later, and the address of a name at file scope is a `lea` off the instruction
//! pointer with the name on it. Neither is a rule because neither is a claim about bitvectors: one
//! of them is waiting on a number nothing knows yet and the other is right because of what the
//! linker does with a relocation. A cast between a pointer and an integer as wide as one is here
//! for the opposite reason, which is that it is no instruction at all.
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
//! [`layout`] runs last and is what makes a function something a machine could run rather than
//! something a printer could print. It puts the blocks in the order they are laid out in and then
//! writes the jumps that order needs, which is where a conditional branch finally becomes a test
//! and a jump and where an edge to the next block becomes nothing at all.
//!
//! [`pipeline`] is the order all of that runs in, which is the only thing about the back end a
//! caller outside this crate has to know and now the only thing it has to say. It is one function
//! from an IR function to a machine one, and a [`pipeline::Machine`] describing what is being
//! compiled for. The driver's `--emit=mir-final` is a call to it per definition in the module.
//!
//! [`coverage`] is what says whether all of that adds up to a back end. Every IR opcode is lowered
//! by a rule, or somewhere a rule cannot reach and the reason is written down, or nowhere and the
//! issue that closes it is written down. Which of the three each one is is checked rather than
//! believed, and the count of the third is one of the numbers `spec/15-testing.md` says we keep
//! about ourselves. It is not zero yet.
//!
//! The other coverage question is the one only a corpus can answer, which is which of the rules
//! that are written anything ever fires. [`coverage::Fired`] is what records that as the selector
//! goes, and `-Zrule-coverage=FILE` is how a run of the compiler is asked for it.
//!
//! What is not here yet is the optimizing path: no scheduling, no peepholes, and a block order
//! from the shape of the control flow rather than from how often each block runs.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-codegen/0.7.4")]

pub mod abi;
pub mod coverage;
pub mod expand;
pub mod finish;
pub mod frame;
pub mod layout;
pub mod lower;
pub mod pipeline;
pub mod select;
pub mod split;
pub mod varargs;
pub mod widths;

/// The IR as something a rule can match against, which is [`rucc_ir::term`].
///
/// Re-exported rather than reached for through `rucc_ir`, because this crate had it first and
/// every caller here says `crate::term`. It moved down when `rucc-opt` became the second crate
/// to match a rule set against the IR, and where it lives is not something a caller of it has
/// any reason to know.
pub use rucc_ir::term;

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
