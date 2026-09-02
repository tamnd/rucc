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
//! # When the solver gives up
//!
//! Some claims no solver settles in the time anybody will wait, and a multiplication of two
//! unknowns at sixty four bits is the usual one. Such a rule may carry a `bounded` clause
//! saying why a proof at narrower widths would be enough, and then the shrug is answered by
//! asking the same question again at [`BOUNDED_WIDTHS`]. Every one of them has to come back
//! `unsat`, the clause has to be there before any of it happens, and the result is a verdict of
//! its own rather than a discharge, because a claim proved at four, eight and sixteen bits is
//! not the claim the compiler relies on.
//!
//! The clause is a judgement somebody makes and signs for. A tool that fell back to narrow
//! widths on its own would turn every rule the solver is slow on into a rule nobody checked,
//! which is the failure this whole crate exists to prevent, so the fallback is never taken
//! without a written reason and the number of times it was taken is printed.
//!
//! # The gate
//!
//! [`admit`] is the rule set's front door and the `rucc-verify` program is what CI runs it
//! from. A file with anything in it that is not a proof is refused whole rather than having the
//! failing rules dropped, because a compiler built from the rules that happened to pass is a
//! compiler nobody described.
//!
//! # Widths
//!
//! A rule is written at the width its pattern's opcode names, and the terms inside it may name
//! another. `(add.i32 (value.i64 x) (value.i64 y))` is a thirty two bit add of two sixty four bit
//! registers, and both numbers are in the question that gets asked: `x` is declared sixty four
//! bits wide and what the rule computes is thirty two. That is what a rule has to be able to say
//! before `sext`, `zext` and `trunc` can be lowered at all, and the conversions themselves are
//! written the way `spec/10-backend.md` writes them, `(sign_extend 32 64 x)` and
//! `(extract 31 0 x)`, with the widths spelled out rather than inferred.
//!
//! When the machine term is wider than the IR term it replaces, which is what lowering a value
//! into a wider register looks like, the two are asked to agree on the bits the IR term has. What
//! the rest of the register holds is then left to the rule's `spec` clause, which is the only
//! place a target's sign extension rule is written down and so the only place it can be checked.

#![doc(html_root_url = "https://docs.rs/rucc-verify/0.2.21")]

mod model;
mod solver;
mod verify;

pub use model::{DEFAULT_WIDTH, Model, Widths, rule_width};
pub use solver::{Answer, Solver};
pub use verify::{BOUNDED_WIDTHS, Report, Verdict, admit, query, query_at, verify};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";
