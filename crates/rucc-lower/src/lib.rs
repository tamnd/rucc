//! The typed AST to IR walk: SSA construction and ABI-directed lowering.
//!
//! Design: `spec/08-ir.md`. Layer rank 9, see `spec/18-package-layout.md`.
//!
//! # What is here so far
//!
//! [`Ssa`], which is SSA construction by Braun's algorithm: the thing that lets a local
//! variable become a value without ever having been a stack slot. The walk over the typed tree
//! that drives it follows, in M2.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-lower/0.2.12")]

mod ssa;

pub use ssa::{Ssa, Var};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M2";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
