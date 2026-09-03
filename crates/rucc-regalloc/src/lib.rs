//! Both register allocators and the allocation checker.
//!
//! Design: `spec/10-backend.md`. Layer rank 10, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! Liveness is here, which is the question both allocators ask first: [`order`] lays a function
//! out in the line the encoder will emit it in, and [`live`] says where in that line each value
//! is wanted. So is [`moves`], which puts the moves an edge turns into in an order they can be
//! made in one at a time. The single pass allocator's decision is in [`assign`]: where every value
//! of a function goes, in one linear scan, which is what `-O0` asks for. The rewrite that makes
//! that decision true in the function lands next, then the allocation checker. The backtracking
//! allocator is M4.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-regalloc/0.3.3")]

pub mod assign;
pub mod live;
pub mod moves;
pub mod order;

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
