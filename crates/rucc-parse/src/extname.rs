//! `#pragma redefine_extname`, which gives a name with external linkage another name in the
//! object file, from anywhere in the file.
//!
//! `#pragma redefine_extname old new` is `__asm__("new")` on every declaration of `old` with
//! external linkage, written apart from them. It is how Solaris' headers send `open` to `open64`
//! under a large file environment, and gcc predefines `__PRAGMA_REDEFINE_EXTNAME` so a header can
//! ask whether it may. A call to `old` written above the line goes to `new` as well as one below
//! it, so the lines are only read here, and the checker applies them by name once it has seen the
//! whole file.
//!
//! A line that is not two names gets gcc's `-Wpragmas` warning and is ignored, one with something
//! after the two names gets the warning and is applied all the same, and a second line for a name
//! already renamed to something else is ignored with the warning gcc gives it.

use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_lex::Token;

use crate::parser::Parser;

/// What one `#pragma redefine_extname` line asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtnamePragma {
    /// The name the file writes.
    pub old: Symbol,
    /// The name the object file is to have for it.
    pub new: Symbol,
    /// Where `redefine_extname` was written, which is where gcc puts a complaint about the line.
    pub span: Span,
}

impl Parser<'_> {
    /// One `#pragma redefine_extname` line, the word `redefine_extname` included.
    pub(crate) fn extname_line(&mut self, line: &[Token]) {
        let word = line[0].span;
        let (Some(old), Some(new)) =
            (line.get(1).and_then(|token| token.ident()), line.get(2).and_then(|token| token.ident()))
        else {
            return self.warn("E0798", "malformed `#pragma redefine_extname`, ignored", word);
        };
        if line.len() > 3 {
            self.warn("E0798", "junk at end of `#pragma redefine_extname`", word);
        }
        if let Some(before) = self.extname_pragmas.iter().find(|before| before.old == old) {
            if before.new != new {
                self.warn(
                    "E0798",
                    "`#pragma redefine_extname` ignored due to conflict with previous \
                     `#pragma redefine_extname`",
                    word,
                );
            }
            return;
        }
        self.extname_pragmas.push(ExtnamePragma { old, new, span: word });
    }
}
