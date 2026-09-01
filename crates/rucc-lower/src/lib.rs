//! The typed AST to IR walk: SSA construction and ABI-directed lowering.
//!
//! Design: `spec/08-ir.md`. Layer rank 9, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! Not implemented. This crate exists from the first commit so that the layer rank it holds
//! is real and `cargo xtask layers` has something to check. The work lands in M2.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-lower/0.2.12")]

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M2";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
