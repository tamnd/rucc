//! `#pragma GCC diagnostic`, which turns a warning off, on or into an error for the lines that
//! follow it.
//!
//! The lines are read as the parser walks past them, the way `pack.rs` reads its own, and kept
//! with where each was written. Nothing here decides a warning: a warning is raised wherever the
//! pass that finds it is, often well after the line it is about has been parsed, so the driver
//! looks each one up by its position in `rucc_diag::Scoped` once everything has been said.
//!
//! What gcc 13 says about a line it cannot read is said here, in its words, under `-Wpragmas`. An
//! option that starts with `-W` and is not one this compiler raises is taken quietly rather than
//! called unknown the way gcc calls a name it has never heard of, since there are several hundred
//! gcc knows and a build that silences one of them expects to hear nothing about the line.
//! `ignored_attributes` is read and does nothing.

use rucc_diag::{DiagnosticPragma, PragmaKind, Span};
use rucc_lex::{Token, TokenKind};

use crate::parser::Parser;

/// The code of every complaint about a `#pragma GCC diagnostic` line.
const MALFORMED: &str = "E0798";

impl Parser<'_> {
    /// One `#pragma GCC diagnostic` line, with the `GCC diagnostic` taken off.
    pub(crate) fn diagnostic_line(&mut self, rest: &[Token], span: Span) {
        let kinds = "`error`, `warning`, `ignored`, `push`, `pop` or `ignored_attributes` after \
                     `#pragma GCC diagnostic`";
        let Some(&word) = rest.first() else {
            self.warn(MALFORMED, format!("missing {kinds}"), span);
            return;
        };
        let kind = match word.ident().map(|name| self.cx.interner.resolve(name)) {
            Some("push") => return self.diagnostic_pragmas.push((span.lo, DiagnosticPragma::Push)),
            Some("pop") => return self.diagnostic_pragmas.push((span.lo, DiagnosticPragma::Pop)),
            Some("ignored_attributes") => return,
            Some("ignored") => PragmaKind::Ignored,
            Some("warning") => PragmaKind::Warning,
            Some("error") => PragmaKind::Error,
            _ => {
                self.warn(MALFORMED, format!("expected {kinds}"), word.span);
                return;
            }
        };
        let mut option = &rest[1..];
        let Some(&written) = option.first().filter(|token| token.kind == TokenKind::Str) else {
            let what = "missing option after `#pragma GCC diagnostic` kind";
            self.warn(MALFORMED, what, word.span);
            return;
        };
        // What comes after the option is not read, by gcc either.
        let Some(text) = self.comment_text(&mut option) else { return };
        match text.strip_prefix("-W") {
            Some(name) if !name.is_empty() => {
                let line = DiagnosticPragma::Set(kind, name.to_owned());
                self.diagnostic_pragmas.push((span.lo, line));
            }
            _ => {
                let what = format!("`{text}` is not an option that controls warnings");
                self.warn(MALFORMED, what, written.span);
            }
        }
    }
}
