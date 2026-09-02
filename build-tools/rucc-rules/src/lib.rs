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
//! # The language
//!
//! A rule names what to match, what to put in its place, and why that is sound:
//!
//! ```text
//! (rule (lower (add.i64 (value x) (mul.i64 (value y) (iconst 4))))
//!       (x64.lea (amode_base_index_scale x y 4))
//!       (spec (= (bvadd x (bvmul y 4)) (result))))
//! ```
//!
//! A rule that only holds under a condition says so between the two, where it can be read as
//! part of deciding whether the rule fires rather than as part of what firing produces:
//!
//! ```text
//! (rule (lower (shl.i64 (value x) (iconst k)))
//!       (if (and (>= k 0) (< k 64)))
//!       (x64.shl x k)
//!       (spec (= (bvshl x k) (result))))
//! ```
//!
//! Everything is a term, and a term is a name, a number, or a head applied to arguments. A bare
//! name is a variable and a parenthesised one is an application, which is the whole of the
//! distinction and is why a constructor taking nothing is still written `(result)`. Variables
//! are bound by the pattern and used everywhere else, `(result)` stands for what the
//! replacement computes and so appears only in a specification, and a name may not be bound
//! twice in one pattern, because that would be asking the matcher for an equality test it does
//! not have.
//!
//! The `spec` clause is required rather than optional. `spec/17-milestones.md` asks that a rule
//! the solver cannot discharge never enter the rule set, and making the claim part of the
//! grammar is what gives that somewhere to stand: a rule without one is not an unverified rule,
//! it is a syntax error.
//!
//! # Status
//!
//! The language and its reader are here. Compiling the rules into the matching automaton, and
//! discharging their specifications in `rucc-verify`, are the two pieces that follow, and all
//! three land in `M3` because `spec/10-backend.md` says retrofitting verification onto an
//! existing rule set is the thing not to do.

#![doc(html_root_url = "https://docs.rs/rucc-rules/0.2.20")]

mod ast;
mod error;
mod lex;
mod parse;

pub use ast::{Rule, Term, TermKind};
pub use error::Error;
pub use parse::parse;

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";
