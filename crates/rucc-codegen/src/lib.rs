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
//! width the machine has one for. Nothing with an effect is covered, because no rule for one is
//! written yet.
//!
//! What is not here yet is the frames, the prologues and the block layout. All of it lands in M3.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-codegen/0.3.3")]

pub mod lower;
pub mod select;
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
