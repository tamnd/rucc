//! Recursive descent with a Pratt expression parser, declarators, and error recovery.
//!
//! Design: `spec/06-lexer-and-parser.md`. Layer rank 7, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! The grammar is here: expressions, declaration specifiers, declarators, initializers,
//! statements and declarations, in every dialect from C89 to C23 and with the GNU extensions
//! that real code cannot be read without. [`parse`] takes the tokens phase 7 produced and gives
//! back a tree and the diagnostics it collected on the way. What is not here is the printer, so
//! there is no way to turn the tree back into source yet, and there is nothing that checks the
//! tree means anything.
//!
//! ```
//! use rucc_base::Interner;
//! use rucc_lex::{Convert, Keywords, Options, convert, tokenize};
//! use rucc_parse::{Context, parse};
//! use rucc_session::Std;
//! use rucc_target::{TargetInfo, Triple};
//!
//! let std = Std::C23;
//! // The keyword table is interned first, before any source is read.
//! let mut interner = Interner::new();
//! let keywords = Keywords::new(&mut interner, std, true);
//! let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
//!
//! let source = b"int main(void) { return 0; }";
//! let (pp, _) = tokenize(source, 0, Options::new(), &mut interner);
//! let cx = Convert { keywords: &keywords, interner: &interner, target: &target,
//!                    std, gnu: false, pedantic: false };
//! let (tokens, _) = convert(&pp, &cx);
//!
//! let parsed = parse(&tokens, Context::new(&interner, std));
//! assert!(!parsed.failed());
//! assert_eq!(parsed.ast.top_level().len(), 1);
//! ```
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
//! # Where the productions are
//!
//! [`Parser`] holds the state and the helpers, and the productions are inherent methods on it
//! written across six private modules: expressions, specifiers, declarators, initializers,
//! statements and declarations. They are one recursive descent parser split up for reading
//! rather than six things that call each other, so nothing is exported from them.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-parse/0.2.21")]

pub mod cursor;
pub mod parser;
pub mod recover;
pub mod scope;

mod decl;
mod declarator;
mod expr;
mod init;
mod pack;
mod spec;
mod stmt;

pub use crate::cursor::{Cursor, MAX_LOOKAHEAD, Mark};
pub use crate::parser::{Context, MAX_NESTING, Parsed, Parser};
pub use crate::recover::{Poison, push_about, skip_past_declaration, skip_to_statement_end};
pub use crate::scope::{IdentKind, Scopes, TagKind};

/// Parses a translation unit.
///
/// Always gives back a tree. A file that does not parse produces poisoned nodes where the
/// productions gave up, so that everything after a mistake is still parsed and reported on, and
/// [`Parsed::failed`] is what says whether to carry on with it.
#[must_use]
pub fn parse<'a>(tokens: &'a rucc_lex::Tokens, cx: Context<'a>) -> Parsed {
    let mut parser = Parser::new(tokens, cx);
    parser.translation_unit();
    parser.finish()
}

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M2";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
