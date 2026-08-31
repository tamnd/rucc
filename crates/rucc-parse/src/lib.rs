//! Recursive descent with a Pratt expression parser, declarators, and error recovery.
//!
//! Design: `spec/06-lexer-and-parser.md`. Layer rank 7, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! The spine, and not yet the grammar. What is here is the part every production rests on: the
//! token buffer with its bounded lookahead, the scopes that answer whether an identifier is a
//! type name, and the recovery that decides where to resume and which messages to hold back.
//! Expressions, declarators, declarations and statements land on top of it, in that order, and
//! the crate has no entry point that parses a translation unit until they do.
//!
//! # What the parser reads
//!
//! A slice of [`rucc_lex::Token`], which is what phase 7 produces from the preprocessor's
//! output. Directives are gone by then, adjacent string literals have been joined, constants
//! have been converted, and a spelling no longer means anything. The parser never looks at
//! source text and never asks the lexer a question, which is what makes the typedef decision in
//! [`scope`] the only place the ambiguity in C's grammar is resolved.
//!
//! # What it builds
//!
//! A [`rucc_ast::Ast`], which records what was written rather than what it means. Nothing here
//! resolves a name to a declaration, works out a type, folds a constant or desugars anything.
//! A file that parses is not a file that compiles, and keeping the two apart is what lets the
//! printer put back what it was given.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-parse/0.2.3")]

pub mod cursor;
pub mod recover;
pub mod scope;

pub use crate::cursor::{Cursor, MAX_LOOKAHEAD, Mark};
pub use crate::recover::{
    DEFAULT_ERROR_LIMIT, Errors, Poison, skip_past_declaration, skip_to_statement_end,
};
pub use crate::scope::{IdentKind, Namespace, Scopes, TagKind};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M2";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
