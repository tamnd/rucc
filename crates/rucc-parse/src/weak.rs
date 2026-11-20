//! `#pragma weak`, which makes a name weak, or makes it a weak second name for another symbol,
//! from anywhere in the file.
//!
//! `#pragma weak name` is `__attribute__((weak))` on every declaration of `name`, written apart
//! from them, and `#pragma weak name = target` is `name` declared weak with `alias("target")`,
//! even when nothing in the file declares `name`. That second form is how a C library gives a
//! function its public name beside the reserved one, and Solaris' and the BSDs' headers are full
//! of the first. Neither depends on where the line is: gcc applies a pragma written above the
//! declaration it names and one written below it alike.
//!
//! So the lines are only read here, and the checker applies them by name once it has seen the
//! whole file. A line that is not one of the two forms gets gcc's `-Wpragmas` warning and is
//! ignored, and one with something after the form gets the warning and is applied all the same.

use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_lex::Token;

use crate::pack::eat_punct;
use crate::parser::Parser;

/// What one `#pragma weak` line asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeakPragma {
    /// The name made weak.
    pub name: Symbol,
    /// The symbol `name` is a second name for, when the line said `= target`.
    pub target: Option<Symbol>,
    /// Where the name was written, which is where a complaint about it goes.
    pub span: Span,
}

impl Parser<'_> {
    /// One `#pragma weak` line, the word `weak` included.
    pub(crate) fn weak_line(&mut self, line: &[Token]) {
        let word = line[0].span;
        let mut rest = &line[1..];
        let malformed = "malformed `#pragma weak`, ignored";
        let Some(first) = rest.first().copied() else {
            return self.warn("E0798", malformed, word);
        };
        let Some(name) = first.ident() else {
            return self.warn("E0798", malformed, word);
        };
        rest = &rest[1..];
        let target = if eat_punct(&mut rest, "=") {
            let Some(target) = rest.first().and_then(|token| token.ident()) else {
                return self.warn("E0798", malformed, word);
            };
            rest = &rest[1..];
            Some(target)
        } else {
            None
        };
        if !rest.is_empty() {
            self.warn("E0798", "junk at end of `#pragma weak`", word);
        }
        self.weak_pragmas.push(WeakPragma { name, target, span: first.span });
    }
}
