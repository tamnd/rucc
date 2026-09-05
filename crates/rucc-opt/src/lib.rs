//! The pass manager, the acyclic e-graph, the rewrite rules and the analyses.
//!
//! Design: `spec/09-optimizer.md`. Layer rank 9, see `spec/18-package-layout.md`.
//!
//! # What is here
//!
//! The pass manager and one pass. [`pipeline`] holds the six pipelines, one per optimization
//! level, written out rather than assembled from flags, along with the fuel, the dumps and the
//! verification that section 9.10 asks of every pass. [`fold`] is the first pass through it.
//!
//! The e-graph, the rewrite rule set and the analyses are still M4 work and are not here yet.
//! So is the analysis manager, which section 9.10 also asks for: a pass declares which analyses
//! it requires, preserves and invalidates, and a debug check recomputes one it claimed to
//! preserve and compares. There are no analyses to declare, so that machinery lands with the
//! dominator tree rather than being guessed at now.
//!
//! # Stability
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-opt/0.4.0")]

pub mod fold;
pub mod fuel;
pub mod pass;
pub mod pipeline;

pub use fuel::Fuel;
pub use pass::{PASSES, Pass};
pub use pipeline::{Dump, Dumps, Options, Report, run};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M4";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
