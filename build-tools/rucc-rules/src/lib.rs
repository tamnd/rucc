//! The rule DSL compiler.
//!
//! Design: `spec/09-optimizer.md` and `spec/10-backend.md`. Outside the layer stack: this is
//! a build dependency, never a runtime one.
//!
//! Middle-end rewrites and instruction selection patterns are written once, in one language,
//! and this crate compiles them into the matching code the compiler runs. The same rule text
//! is what `rucc-verify` discharges against an SMT solver, which is the point: a rule that is
//! verified and a rule that is applied cannot drift apart if they are the same text.
//!
//! # Status
//!
//! Not implemented. The DSL, its compiler and its verifier are built together in `M3`,
//! because `spec/10-backend.md` says retrofitting verification onto an existing rule set is
//! the thing not to do.

#![doc(html_root_url = "https://docs.rs/rucc-rules/0.1.0")]

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert_eq!(super::MILESTONE, "M3");
    }
}
