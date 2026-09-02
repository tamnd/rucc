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
//! One rule in a hundred has a claim no solver will settle in the time anybody will wait, which
//! is usually a multiplication of two unknowns at full width. Such a rule may carry a last
//! clause saying why a proof at narrower widths would be enough:
//!
//! ```text
//! (rule (lower (mul.i64 (value x) (value y)))
//!       (x64.imul x y)
//!       (spec (= (bvmul x y) (result)))
//!       (bounded "multiplication of two unknowns at 64 bits is out of reach"))
//! ```
//!
//! The clause excuses nothing on its own. The rule is still asked at its own width first, and
//! all the clause does is say what a person is willing to sign for if the answer comes back as
//! a shrug. `rucc-verify` is what acts on it, and what counts how often it had to.
//!
//! # The matcher
//!
//! A rule set compiles into a trie over its patterns rather than into a conditional per rule.
//! Every pattern is flattened into the steps that match it, read in pre-order, and patterns that
//! begin the same way share the steps they agree on, so testing that a term is an `add.i64`
//! happens once however many rules begin with one. Specificity is the shape rather than a sort:
//! the concrete tests at a node are tried before the wildcard, so a rule that names an operand
//! is tried before a rule that takes anything there.
//!
//! # Status
//!
//! The language, its reader and the matcher are here, and `rucc-verify` discharges the
//! specifications. Emitting the matcher as Rust for the compiler to link against is the piece
//! that follows. All of it lands in `M3` because `spec/10-backend.md` says retrofitting
//! verification onto an existing rule set is the thing not to do.

#![doc(html_root_url = "https://docs.rs/rucc-rules/0.2.21")]

mod ast;
mod error;
mod lex;
mod matcher;
mod parse;

pub use ast::{Rule, Term, TermKind};
pub use error::Error;
pub use matcher::{Match, Matcher};
pub use parse::{parse, parse_terms};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";
