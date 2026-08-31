//! SMT verification of the rule set.
//!
//! Design: `spec/15-testing.md` section 15.5. Outside the layer stack: this runs in CI, not
//! in the compiler.
//!
//! Every rewrite rule and every lowering rule carries a specification, and this crate
//! discharges it. An unverified rule does not enter the rule set. Rules that the solver
//! cannot discharge, usually wide bitvector multiplication, get a bounded proof over
//! restricted widths plus a reviewed justification, and the count of those is a reported
//! metric that going up is a signal about.
//!
//! The approach follows Crocus (ASPLOS 2024), cited in `spec/01-research-2026.md`.
//!
//! # Status
//!
//! Not implemented. Built alongside `rucc-rules` in `M3`.

#![doc(html_root_url = "https://docs.rs/rucc-verify/0.1.0")]

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert_eq!(super::MILESTONE, "M3");
    }
}
