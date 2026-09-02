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
//! # What is asked
//!
//! A rule says that its pattern and its replacement compute the same thing, and its `spec`
//! clause says what that thing is in bitvectors. Two obligations follow and both are asked as
//! one question. The first is the one that matters: what the pattern means and what the
//! replacement means have to be the same, both read out of the machine model rather than out of
//! anybody's description of them. The second is the `spec` clause itself, which is written by
//! hand and so is worth checking rather than trusting, because a rule whose stated claim is not
//! what its pattern actually means would otherwise be verified against its own mistake.
//!
//! The question put to the solver is the negation: is there any assignment to the pattern's
//! variables that makes either of those false? An `unsat` back means there is not, which is the
//! rule discharged. A `sat` back is a counterexample and the rule is wrong. Nothing else counts
//! as a pass, and in particular a solver that gives up is reported as having given up rather
//! than folded into either answer.
//!
//! The guard is an assumption rather than part of the claim, which is what makes a rule that is
//! only true for some constants provable at all.
//!
//! # What is not here yet
//!
//! One width per rule, taken from the suffix on the pattern's opcode. A rule that changes width,
//! such as the RISC-V sign extension example in `spec/10-backend.md`, needs the terms to carry
//! types and that is not built. The bounded proofs the specification asks for are also not
//! built: a rule the solver cannot discharge is reported as unknown, and no restricted-width
//! fallback is attempted.

#![doc(html_root_url = "https://docs.rs/rucc-verify/0.2.20")]

mod model;
mod solver;
mod verify;

pub use model::Model;
pub use solver::{Answer, Solver};
pub use verify::{Report, Verdict, query, verify};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";
