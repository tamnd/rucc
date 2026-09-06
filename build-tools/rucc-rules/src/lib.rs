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
//! There are two kinds and the word after `rule` says which. A `lower` rule puts a machine term
//! in place of an IR one, which is the last thing that happens to a value. A `simplify` rule
//! puts IR in place of IR, which means what it produces is matched again:
//!
//! ```text
//! (rule (simplify (add.i32 (value.i32 x) (iconst.i32 0)))
//!       (value.i32 x)
//!       (spec (= x (result))))
//! ```
//!
//! Everything else about the two is the same. They share the reader, the trie, the emitter and
//! the verification obligation, because a rewrite and a lowering are the same claim about two
//! terms and there is no reason to say it twice. `spec/optimizer/13-rewrite-rules.md` is what
//! the rewrite half is for and it says the rule set comes before the rewriter that runs it.
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
//! A name may be written twice in one pattern, which says that the two places hold the same
//! thing. That is how the identities of `spec/optimizer/13-rewrite-rules.md` section 13.4 are
//! written, and four of them cannot be said without it:
//!
//! ```text
//! (rule (simplify (and.i32 (value.i32 x) (value.i32 x)))
//!       (value.i32 x)
//!       (spec (= x (result))))
//! ```
//!
//! The second occurrence binds nothing. It compiles into a test that the subterm is what the
//! first occurrence took, which sits with the other concrete tests ahead of the wildcard,
//! because a rule about one value in both operands is more specific than a rule about any two.
//! Whether two of a subject's terms are the same thing is a question for the subject, since a
//! term is a place there and two places can hold one value.
//!
//! A rule set is also emitted as Rust, which is how the compiler gets to match with it. The
//! build script of the crate that owns a rule file reads the file, builds the trie, and writes
//! the table into its build directory, so the rules are read from one place and the table is
//! never a copy anybody has to keep up to date. What comes out is data rather than code, except
//! for the guards, which are the one part of a rule that has to be evaluated. The walk over the
//! table is in the crate that includes it, because what it walks there is the compiler's own IR
//! rather than a term.
//!
//! # Status
//!
//! The language, its reader, the matcher and the emitter are here, and `rucc-verify` discharges
//! the specifications. What follows is the selector: the pass that finds the terms in a function
//! worth matching, and that builds machine instructions out of what the table gives back. All of
//! it lands in `M3` because `spec/10-backend.md` says retrofitting verification onto an existing
//! rule set is the thing not to do.

#![doc(html_root_url = "https://docs.rs/rucc-rules/0.3.3")]

mod ast;
mod emit;
mod error;
mod lex;
mod matcher;
mod parse;

pub use ast::{Rule, RuleKind, Term, TermKind};
pub use emit::emit;
pub use error::Error;
pub use matcher::{Match, Matcher};
pub use parse::{parse, parse_terms};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";
