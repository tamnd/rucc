//! The pass manager, the acyclic e-graph, the rewrite rules and the analyses.
//!
//! Design: `spec/09-optimizer.md`. Layer rank 9, see `spec/18-package-layout.md`.
//!
//! # What is here
//!
//! The pass manager and four passes. [`pipeline`] holds the six pipelines, one per optimization
//! level, written out rather than assembled from flags, along with the fuel, the dumps and the
//! verification that section 9.10 asks of every pass. [`fold`] is the first pass through it,
//! [`simplify`] is the peephole the e-graph will eventually absorb, [`narrow`] takes the width
//! back off arithmetic that C promoted, and [`dce`] is what clears up after all three of them.
//! [`uses`] is the one thing two of them share, which is a count of who reads what.
//!
//! [`stats`] is what a pass has to return, and [`optinfo`] is that printed. A pass reports what
//! it did and what it gave up on, and there is no other way for it to tell the manager it changed
//! anything, so the instrumentation cannot be the thing nobody got round to. Section 42.2 of
//! `spec/optimizer/42-measurement.md` counted what happens otherwise.
//!
//! [`cfg`] and [`dom`] are the first two analyses, and everything in `spec/optimizer/07` through
//! `spec/optimizer/11` is built on them. [`cfg`] is the shape of a function with the
//! instructions taken out, and [`dom`] answers what every path has to go through, forwards and
//! backwards.
//!
//! The e-graph and the rewrite rule set are still M4 work and are not here yet. So is the
//! analysis manager, which section 9.10 also asks for: a pass declares which analyses it
//! requires, preserves and invalidates, and a debug check recomputes one it claimed to preserve
//! and compares. Two analyses is not yet enough to design that around, so the passes build what
//! they need for now and the manager lands when the third one does.
//!
//! # Stability
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-opt/0.4.1")]

pub mod cfg;
pub mod dce;
pub mod dom;
pub mod fold;
pub mod fuel;
pub mod narrow;
pub mod optinfo;
pub mod pass;
pub mod pipeline;
pub mod simplify;
pub mod stats;
#[cfg(test)]
mod testing;
pub mod uses;

pub use cfg::Cfg;
pub use dom::{Dominators, PostDominators};
pub use fuel::Fuel;
pub use optinfo::Wants;
pub use pass::{PASSES, Pass};
pub use pipeline::{Dump, Dumps, Options, Remark, Report, run};
pub use stats::Stats;

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M4";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
