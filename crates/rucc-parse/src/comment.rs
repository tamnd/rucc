//! `#pragma comment`, which asks the linker for something from inside a source file.
//!
//! Design: `$HOME/notes/Spec/2131/platform/windows/11-headers-and-dialect.md` section 11.5.
//!
//! `#pragma comment(lib, "ws2_32")` is how a lot of Windows C code says it wants a library,
//! instead of `-lws2_32` on the command line, and `#pragma comment(linker, "...")` passes any
//! option at all. Both end up in the object's `.drectve` section, which is where a COFF linker
//! reads options from. The other kinds MSVC has (`compiler`, `exestr`, `user`) put a string in
//! the object for a person to find, and clang takes them and writes nothing, as this does.
//!
//! The lines are read here because this is where the pragma lines are read, and the parser hands
//! what they ask for to the driver, which is the part that knows whether the target has anywhere
//! to put it.

use rucc_diag::Span;
use rucc_lex::{Encoding, Token, TokenKind};

use crate::pack::eat_punct;
use crate::parser::Parser;

/// What one `#pragma comment` line asks the linker for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comment {
    /// `lib`: link against this library, as it was written.
    Lib(String),
    /// `linker`: pass this option to the linker, as it was written.
    Linker(String),
}

impl Parser<'_> {
    /// One `#pragma comment` line, with the word `comment` taken off.
    pub(crate) fn comment_line(&mut self, mut rest: &[Token], span: Span) {
        if !eat_punct(&mut rest, "(") {
            self.warn("E0798", "missing `(` after `#pragma comment` - ignored", span);
            return;
        }
        let Some(kind) = rest.first().and_then(|token| token.ident()) else {
            self.warn("E0798", "malformed `#pragma comment` - ignored", span);
            return;
        };
        let kind = self.cx.interner.resolve(kind).to_owned();
        rest = &rest[1..];
        let text = if eat_punct(&mut rest, ",") {
            let Some(text) = self.comment_text(&mut rest) else {
                self.warn("E0798", "`#pragma comment` wants a string after the comma", span);
                return;
            };
            Some(text)
        } else {
            None
        };
        if !eat_punct(&mut rest, ")") || !rest.is_empty() {
            self.warn("E0798", "malformed `#pragma comment` - ignored", span);
            return;
        }
        match (kind.as_str(), text) {
            ("lib", Some(text)) => self.comments.push(Comment::Lib(text)),
            ("linker", Some(text)) => self.comments.push(Comment::Linker(text)),
            ("lib" | "linker", None) => {
                let what = format!("`#pragma comment({kind})` wants a string - ignored");
                self.warn("E0798", what, span);
            }
            ("compiler" | "exestr" | "user", _) => {}
            _ => {
                let what = format!("unknown kind `{kind}` in `#pragma comment` - ignored");
                self.warn("E0798", what, span);
            }
        }
    }

    /// The string after the comma, which has to be a plain one.
    pub(crate) fn comment_text(&self, rest: &mut &[Token]) -> Option<String> {
        let token = rest.first()?;
        if token.kind != TokenKind::Str {
            return None;
        }
        let literal = self.tokens.strings.get(token.value as usize)?;
        if !matches!(literal.encoding, Encoding::Plain | Encoding::Utf8) {
            return None;
        }
        *rest = &rest[1..];
        let bytes: Vec<u8> = literal.elements.iter().map(|&element| element as u8).collect();
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}
