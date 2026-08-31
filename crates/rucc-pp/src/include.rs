//! Reading a file, and the parts of `#include` that are not the directive itself.
//!
//! Design: `spec/05-preprocessor.md` section 5.4 and `spec/04-driver-and-cli.md` section 4.4.
//!
//! Phase 4 drives the lexer rather than being handed a finished token vector, and the reason
//! is header names. `<stdio.h>` and a run of comparisons are the same bytes, and only the
//! directive knows which one is possible, so [`rucc_lex::Lexer::header_name`] exists and has
//! to be called at exactly the right moment. Scanning the file first and reconstructing the
//! name from the tokens afterwards works until a header name contains `//`, or a backslash on
//! a Windows path, and then it silently produces a different name.
//!
//! Driving the lexer also means a file is read one line at a time rather than all at once,
//! which is what the memory mapped input and the header cache both want later.

use std::path::{Path, PathBuf};

use rucc_base::Interner;
use rucc_diag::{BytePos, Diagnostic, SourceMap, Span};
use rucc_lex::{Lexer, Options, PpToken, PpTokenKind, TokenFlags};
use rucc_session::{FileSystem, SearchPath};

use crate::token::Tok;

/// Everything phase 4 needs from outside itself.
///
/// Grouped into one struct because `#include` needs all of it at once and threading five
/// references through every directive handler is how a parameter list becomes unreadable.
/// The lifetime is the compilation, and every field of it lives on the session.
pub struct Context<'a> {
    /// The one interner.
    pub interner: &'a mut Interner,
    /// Where an included file is added, and what a span is resolved against.
    pub sources: &'a mut SourceMap,
    /// Where a header is read from.
    pub fs: &'a dyn FileSystem,
    /// Where a header is looked for.
    pub search: &'a SearchPath,
    /// The dialect knobs phase 1 cares about.
    pub lex: Options,
    /// How deep `#include` may nest before it is called a cycle.
    ///
    /// A header that includes itself with no guard is the common way to reach this, and the
    /// alternative to a limit is a stack overflow with no diagnostic at all.
    pub max_include_depth: u32,
}

impl<'a> Context<'a> {
    /// A context with GCC's include depth limit.
    pub fn new(
        interner: &'a mut Interner,
        sources: &'a mut SourceMap,
        fs: &'a dyn FileSystem,
        search: &'a SearchPath,
    ) -> Context<'a> {
        Context { interner, sources, fs, search, lex: Options::new(), max_include_depth: 200 }
    }
}

impl std::fmt::Debug for Context<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("lex", &self.lex)
            .field("max_include_depth", &self.max_include_depth)
            .finish_non_exhaustive()
    }
}

/// One open file, and what an `#include` written in it resolves against.
#[derive(Debug)]
pub(crate) struct Frame {
    /// Where the file was included from, for the too deeply nested diagnostic.
    pub(crate) at: Span,
    /// The file itself, which is what `#pragma once` and the guard optimization remember it
    /// by. A path rather than a device and inode pair, so two names for one file are two
    /// files here, which is what a file system abstraction with no `stat` in it can say.
    pub(crate) path: PathBuf,
    /// The directory the file is in, which a quoted include looks in first.
    pub(crate) dir: Option<PathBuf>,
    /// Where an `#include_next` written in this file starts looking.
    pub(crate) next: usize,
}

/// Pulls tokens out of the lexer one logical line at a time.
///
/// One token of lookahead, because a line ends when the next token says it starts a line and
/// there is no other way to find that out.
pub(crate) struct Reader<'a> {
    lexer: Lexer<'a>,
    pending: Option<PpToken>,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(src: &'a [u8], start: BytePos, opts: Options) -> Reader<'a> {
        Reader { lexer: Lexer::new(src, start, opts), pending: None }
    }

    /// The next token, which is an end of file token forever once the file runs out.
    pub(crate) fn next(&mut self, interner: &mut Interner) -> PpToken {
        match self.pending.take() {
            Some(token) => token,
            None => self.lexer.next_token(interner),
        }
    }

    /// Puts a token back, so the next call to [`Reader::next`] returns it again.
    pub(crate) fn put_back(&mut self, token: PpToken) {
        self.pending = Some(token);
    }

    /// Appends the rest of the current line to `out`, leaving the next line's first token
    /// where the next call will find it.
    pub(crate) fn line(&mut self, interner: &mut Interner, out: &mut Vec<PpToken>) {
        loop {
            let token = self.next(interner);
            if token.is_eof() || token.flags.has(TokenFlags::START_OF_LINE) {
                self.put_back(token);
                return;
            }
            out.push(token);
        }
    }

    /// Scans a header name here, which only an include directive may ask for.
    ///
    /// `None` when the line does not begin with `<` or `"`, which is the computed include
    /// case and has to be answered by macro expansion instead.
    pub(crate) fn header_name(&mut self, interner: &mut Interner) -> Option<PpToken> {
        // Asking after the line has been read would scan the wrong bytes, and the borrow
        // checker cannot see the difference, so the invariant is stated here instead.
        debug_assert!(self.pending.is_none(), "the header name has to be asked for first");
        self.lexer.header_name(interner)
    }

    pub(crate) fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        self.lexer.take_diagnostics()
    }
}

/// What the two spellings of a header name mean, and the name itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) name: String,
    pub(crate) angled: bool,
}

/// Reads a header name out of the token the lexer produced for one.
///
/// The delimiters come off and nothing else happens: a header name is not a string literal,
/// so a backslash in it is a backslash and `\t` names a file whose name contains a `t`
/// preceded by a backslash, which is what a Windows path needs.
pub(crate) fn header_from_token(spelling: &str) -> Option<Header> {
    let angled = spelling.starts_with('<');
    let close = if angled { '>' } else { '"' };
    let inner = spelling.strip_prefix(if angled { '<' } else { '"' })?;
    let inner = inner.strip_suffix(close).unwrap_or(inner);
    if inner.is_empty() {
        return None;
    }
    Some(Header { name: inner.to_owned(), angled })
}

/// Reads a header name out of the tokens a macro expanded to.
///
/// `#include MACRO` is the computed include, and the standard says only that the tokens are
/// combined in an implementation defined manner. The manner is that spellings are
/// concatenated with nothing between them, which is what GCC does in every case that occurs
/// in real code and is the only choice that makes `<sys/types.h>` come back out as itself.
pub(crate) fn header_from_tokens(spellings: &[&str]) -> Option<Header> {
    let first = *spellings.first()?;
    if first.starts_with('"') && spellings.len() == 1 {
        return header_from_token(first);
    }
    if first != "<" {
        return None;
    }
    let close = spellings.iter().rposition(|s| *s == ">")?;
    if close < 2 {
        return None;
    }
    let name: String = spellings[1..close].concat();
    Some(Header { name, angled: true })
}

/// The directory a file is in, for a quoted include written inside it.
pub(crate) fn directory_of(name: &str) -> Option<PathBuf> {
    Path::new(name).parent().map(Path::to_path_buf)
}

/// The spelling of a token, for the computed include path.
pub(crate) fn spelling(token: Tok, interner: &Interner) -> &str {
    match token.kind {
        PpTokenKind::Punct(p) => p.as_str(),
        _ => token.value.map_or("", |v| interner.resolve(v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_name_keeps_everything_between_the_delimiters() {
        assert_eq!(
            header_from_token("<sys/types.h>"),
            Some(Header { name: "sys/types.h".to_owned(), angled: true })
        );
        assert_eq!(
            header_from_token("\"local.h\""),
            Some(Header { name: "local.h".to_owned(), angled: false })
        );
    }

    #[test]
    fn a_backslash_in_a_header_name_is_a_backslash() {
        // Not an escape. A header name is not a string literal, and `\t` here names a file.
        let header = header_from_token("\"win32\\types.h\"").unwrap();
        assert_eq!(header.name, "win32\\types.h");
    }

    #[test]
    fn an_empty_header_name_is_not_a_header_name() {
        assert_eq!(header_from_token("<>"), None);
        assert_eq!(header_from_token("\"\""), None);
    }

    #[test]
    fn a_computed_include_concatenates_the_spellings() {
        let header = header_from_tokens(&["<", "sys", "/", "types", ".", "h", ">"]).unwrap();
        assert_eq!(header.name, "sys/types.h");
        assert!(header.angled);
    }

    #[test]
    fn a_computed_include_can_expand_to_a_string_literal() {
        let header = header_from_tokens(&["\"local.h\""]).unwrap();
        assert_eq!(header.name, "local.h");
        assert!(!header.angled);
    }

    #[test]
    fn a_computed_include_that_is_neither_is_refused() {
        assert_eq!(header_from_tokens(&[]), None);
        assert_eq!(header_from_tokens(&["1"]), None);
        assert_eq!(header_from_tokens(&["<", "a"]), None);
        assert_eq!(header_from_tokens(&["<", ">"]), None);
    }

    #[test]
    fn the_last_angle_bracket_closes_the_name() {
        // `<a>b>` is not something anyone writes on purpose, but taking the first `>` would
        // silently drop the rest, and taking the last one at least round trips.
        let header = header_from_tokens(&["<", "a", ">", "b", ">"]).unwrap();
        assert_eq!(header.name, "a>b");
    }
}
