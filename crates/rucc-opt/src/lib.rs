//! The pass manager, the acyclic e-graph, the rewrite rules and the analyses.
//!
//! Design: `spec/09-optimizer.md`. Layer rank 9, see `spec/18-package-layout.md`.
//!
//! # What is here
//!
//! The pass manager and five passes. [`pipeline`] holds the six pipelines, one per optimization
//! level, written out rather than assembled from flags, along with the fuel, the dumps and the
//! verification that section 9.10 asks of every pass. [`gate`] is the other half of the
//! bisection interface, which is `-fdisable-<pass>` and `-fenable-<pass>` over a list of
//! functions, so that which pass and which function are two searches rather than one. [`fold`] is the first pass through it,
//! [`simplify`] is the peephole the e-graph will eventually absorb, [`narrow`] takes the width
//! back off arithmetic that C promoted, [`simplify_cfg`] turns a branch whose condition is known
//! into a jump and removes the blocks that leaves stranded, and [`dce`] is what clears up after
//! all four of them. [`uses`] is the one thing two of them share, which is a count of who reads
//! what.
//!
//! [`stats`] is what a pass has to return, and [`optinfo`] is that printed. A pass reports what
//! it did and what it gave up on, and there is no other way for it to tell the manager it changed
//! anything, so the instrumentation cannot be the thing nobody got round to. Section 42.2 of
//! `spec/optimizer/42-measurement.md` counted what happens otherwise.
//!
//! [`mod@cfg`], [`dom`], [`loops`], [`scev`], [`alias`], [`memssa`] and [`range`] are the analyses
//! so far, and everything in `spec/optimizer/07` through `spec/optimizer/11` is built on them.
//! [`mod@cfg`] is the shape of a function with the instructions taken out, [`dom`] answers what
//! every path has to go through, forwards and backwards, [`loops`] says what loops there are, how
//! they nest, and which cycles are not loops at all, [`scev`] says how a value changes across the
//! iterations of one and how many iterations there are, [`alias`] answers the one question every
//! memory optimization is gated on, which is whether two references can touch the same byte,
//! [`memssa`] puts memory on a chain so a load can walk back to the store it sees, and [`range`]
//! says what values an integer can hold at the place it is asked about, which is not the same
//! question as what it can hold where it was defined.
//!
//! [`profile`] is how likely an edge is taken and how often a block runs, along with the field
//! that says how much either is worth believing. The types come first because section 11.5 of
//! `spec/optimizer/11-profile-and-frequency.md` says what M4 owes the profile work that arrives
//! after it, which is the shape rather than the data: a quality on every number, arithmetic that
//! degrades it, and no way to build one without saying where it came from. Retrofitting that into
//! thirty passes once there is real profile data is the failure mode, and it is GCC's, whose
//! profile maintenance bugs are mostly in passes written before the quality field existed.
//!
//! [`predict`] is where the first of those numbers comes from, which is a guess: ten predictors
//! from section 11.2, first match, each one a syntactic situation somebody measured in the 1990s
//! and a rate it turned out right at. Nothing in here is a measurement and every probability out
//! of it says so.
//!
//! [`frequency`] turns those guesses into the number the consumers actually want, which is how
//! often a block runs compared with the function entry. Section 11.3's method: solve each loop
//! from the inside out, take the chance of going round again, and the header runs one over one
//! minus that many times, which is the sum of the series. A loop nothing predicted an exit for
//! gets a cap rather than a division by zero, an irreducible region gets an answer that is marked
//! as not meaning anything, and the check section 11.5 asks for, which is that what arrives at a
//! block adds up to the block, is in [`frequency::Frequencies::problems`].
//!
//! [`live`] is what is live where, and [`pressure`] is that counted per register class, which is
//! section 40.6's one function with four consumers. In SSA the number of values live at a point is
//! the number of registers the program needs there rather than an estimate of it, which is what
//! makes it worth computing exactly: loop invariant motion, the scheduler, the spill phase and if
//! conversion all ask about the same quantity, and four passes each working out their own would be
//! four chances for two of them to make opposite decisions off different counts of one thing. How
//! many registers there are is the target's and is not here, so the answer is a count and the
//! caller brings the register file.
//!
//! [`purity`] is the other question asked about a call, which is what it is allowed to do. Five
//! answers rather than a boolean, because whether a call reads memory and whether it comes back are
//! separate questions and GCC needs both, and the default is the one that permits everything, so a
//! call nobody has taught it about costs a missed optimization rather than a wrong program. The
//! declaration the user wrote and the answer an analysis works out are kept in separate fields and
//! combined where they are read, which is what makes it possible to check one against the other.
//!
//! [`analysis`] is where a pass gets one from. It computes on demand, caches per function, and
//! throws out what a pass broke, working from what the pass said it preserved rather than from a
//! list kept somewhere else. A pass that claims to preserve an analysis it broke is caught under
//! `--verify`, by recomputing the analysis and comparing.
//!
//! The e-graph and the rewrite rule set are still M4 work and are not here yet.
//!
//! # Stability
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-opt/0.6.1")]

pub mod alias;
pub mod analysis;
pub mod cfg;
pub mod dce;
pub mod dom;
pub mod fold;
pub mod frequency;
pub mod frontier;
pub mod fuel;
pub mod gate;
pub mod live;
pub mod loops;
pub mod memssa;
pub mod narrow;
pub mod optinfo;
pub mod pass;
pub mod pipeline;
pub mod predict;
pub mod pressure;
pub mod profile;
pub mod purity;
pub mod range;
pub mod rules;
pub mod scev;
pub mod simplify;
pub mod simplify_cfg;
pub mod stats;
#[cfg(test)]
mod testing;
pub mod uses;

// `alias::Options` is deliberately not re-exported: [`pipeline::Options`] already has that name
// here and two of them at the top of the crate would be one import mistake away from a flag going
// to the wrong place.
pub use alias::{Access, Alias, Answer, Counts, Escapes, Origin, Reason};
pub use analysis::{Analyses, Analysis, Preserved};
pub use cfg::Cfg;
pub use dom::{Dominators, PostDominators};
pub use frequency::Frequencies;
pub use frontier::{ControlDependence, Frontiers};
pub use fuel::Fuel;
pub use gate::Gates;
pub use live::{LiveHere, Liveness};
pub use loops::{Exit, LoopId, Loops};
// `memssa::Counts` is deliberately not re-exported either, for the same reason: [`alias::Counts`]
// has that name here, the two count different things, and a pass reporting one under the other's
// name would be read as a much worse number than it is. `memssa::build` stays behind its module
// because a bare `build` at the top of an optimizer says nothing about what it builds.
pub use memssa::{Clobber, Step, Walk};
pub use optinfo::Wants;
pub use pass::{PASSES, Pass};
pub use pipeline::{Dump, Dumps, Options, Remark, Report, run};
pub use predict::{Callees, Predictions, Predictor};
pub use pressure::Pressure;
pub use profile::{Frequency, Hotness, Probability, Quality};
// `purity::Callee` and `purity::Facts` stay behind their module. `Callee` is one letter away from
// [`predict::Callees`], which is a different thing about the same instructions, and `Facts` at the
// top of an optimizer says nothing about which facts. [`purity::Purity`] is the answer everything
// asks for and is worth having here.
pub use purity::Purity;
// `range::query::Options` and `range::query::Counts` stay behind their module for the two reasons
// already given above, which is that both names are taken at the top of this crate and neither of
// the things holding them is the thing a caller would mean.
pub use range::query::Ranges;
pub use range::{Bits, Range};
pub use scev::{Assumption, Bound, Chrec, Count, Estimate, Evolution, Invariant, Scev};
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
