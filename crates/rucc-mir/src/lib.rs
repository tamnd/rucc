//! The machine IR, still in SSA, and its printer and parser.
//!
//! Design: `spec/10-backend.md`. Layer rank 9, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! Not implemented. This crate exists from the first commit so that the layer rank it holds
//! is real and `cargo xtask layers` has something to check. The work lands in M3.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-mir/0.2.3")]

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
