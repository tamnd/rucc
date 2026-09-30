//! `#pragma GCC visibility push(...)` and `pop`, which set the visibility of what follows.
//!
//! The lines are read as the parser walks past them, which is how `pack.rs` reads its own, and
//! what is in effect where a declaration starts is written on its specifiers. Sema reads it
//! there when no attribute on the declaration names a visibility.
//!
//! GCC applies the pragma to declarations as well as definitions, which is the whole point of
//! it: the kernel wraps its early startup code in `push(hidden)` so that an `extern` variable
//! is reached relative to the instruction pointer rather than through the GOT, whose entries
//! hold addresses nothing has relocated yet.

use rucc_ast::PushedVisibility;
use rucc_diag::Span;
use rucc_lex::{Keyword, Token};

use crate::pack::eat_punct;
use crate::parser::Parser;

impl Parser<'_> {
    /// What `#pragma GCC visibility` has in effect at the cursor.
    pub(crate) fn visibility_in_effect(&mut self) -> Option<PushedVisibility> {
        self.read_to_cursor();
        self.packs.visibility.last().copied()
    }

    /// One `#pragma GCC ...` line with the `GCC` taken off, of which only `visibility` is read.
    pub(crate) fn visibility_line(&mut self, mut rest: &[Token], span: Span) {
        let Some(word) = rest.first().and_then(|token| token.ident()) else { return };
        if self.cx.interner.resolve(word) != "visibility" {
            return;
        }
        rest = &rest[1..];
        let Some(action) = rest.first().and_then(|token| token.ident()) else {
            self.warn("E0745", "missing `push` or `pop` after `#pragma GCC visibility`", span);
            return;
        };
        rest = &rest[1..];
        match self.cx.interner.resolve(action) {
            "pop" => {
                if self.packs.visibility.pop().is_none() {
                    let what = "no matching push for `#pragma GCC visibility pop`";
                    self.warn("E0745", what, span);
                }
            }
            "push" => {
                let seen = if eat_punct(&mut rest, "(") { rest.first().copied() } else { None };
                // `default` is a keyword by the time the line reaches the parser, and the other
                // three are identifiers.
                let pushed = seen.and_then(|token| match token.keyword() {
                    Some(Keyword::Default) => Some(PushedVisibility::Default),
                    Some(_) => None,
                    None => match self.cx.interner.resolve(token.ident()?) {
                        "hidden" | "internal" => Some(PushedVisibility::Hidden),
                        "protected" => Some(PushedVisibility::Protected),
                        _ => None,
                    },
                });
                match pushed {
                    Some(pushed) => self.packs.visibility.push(pushed),
                    None => {
                        let what = "malformed `#pragma GCC visibility push`";
                        self.warn("E0745", what, span);
                    }
                }
            }
            _ => {
                let what = "missing `push` or `pop` after `#pragma GCC visibility`";
                self.warn("E0745", what, span);
            }
        }
    }
}
