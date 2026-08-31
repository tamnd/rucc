//! `#embed`, the C23 directive that puts the bytes of a file into the token stream.
//!
//! Design: `spec/05-preprocessor.md` section 5.4.
//!
//! The directive names a resource the same way `#include` names a header, and is replaced by
//! the bytes of that resource written out as integer constants separated by commas. The point
//! of it is that `static const unsigned char logo[] = {`, `#embed "logo.png"`, `};` replaces
//! the script that every project used to carry for turning a binary into a C array, and the
//! compiler can then read the file once instead of parsing a million integer literals that a
//! script wrote.
//!
//! That last part is the trap, and it is worth being plain about where this implementation
//! stands on it. A byte here becomes a real token, so a one megabyte resource becomes two
//! million tokens and costs what two million tokens cost. The way out is a fast path in the
//! parser that recognises an `#embed` sitting alone in an initializer and fills the array
//! from the bytes without ever making the tokens, which is what section 5.4 describes and
//! what M2 gets to once there is a parser to put it in. Until then the numbers are made as
//! cheaply as they can be: the spellings of the two hundred and fifty six possible bytes are
//! interned once per directive rather than once per byte, so the cost is the token vector and
//! not the interner.
//!
//! The parameters are the interesting part of the design. `limit`, `prefix`, `suffix` and
//! `if_empty` are standard, and `prefix` and `suffix` exist because they are the only way to
//! write a non empty initializer around an embed that might be empty: an empty resource is
//! replaced by `if_empty` alone, and `prefix` and `suffix` are then not emitted at all, so
//! `{` `#embed "x" prefix(0xEF,)` `}` is a valid array either way. Getting that wrong turns
//! an empty file into a syntax error, which is why it has a test.

use rucc_base::{Interner, Symbol};
use rucc_diag::{Diagnostic, Span};
use rucc_lex::{PpTokenKind, Punct, TokenFlags};

use crate::cond;
use crate::include::spelling;
use crate::token::Tok;

/// The parameters written after the resource name.
#[derive(Debug, Default)]
pub(crate) struct Params {
    /// `limit(n)`, the most bytes to take. `None` is no limit, which is not the same as a
    /// limit of zero.
    pub(crate) limit: Option<u64>,
    /// `gnu::offset(n)`, the first byte to take. Zero unless it was asked for.
    pub(crate) offset: u64,
    /// `prefix(...)`, emitted before the bytes and only when there are bytes.
    pub(crate) prefix: Vec<Tok>,
    /// `suffix(...)`, emitted after the bytes and only when there are bytes.
    pub(crate) suffix: Vec<Tok>,
    /// `if_empty(...)`, emitted instead of everything else when there are no bytes.
    pub(crate) if_empty: Vec<Tok>,
}

impl Params {
    /// How many of a resource of `len` bytes this directive actually takes.
    ///
    /// The offset is applied first and then the limit, which is the order that makes
    /// `gnu::offset(4) limit(4)` mean the four bytes after the first four rather than nothing.
    /// An offset past the end is not an error, it is an empty embed, because a caller reading
    /// a file in fixed size chunks would otherwise have to know the length to stop.
    pub(crate) fn taken(&self, len: u64) -> u64 {
        let after_offset = len.saturating_sub(self.offset);
        match self.limit {
            Some(limit) => after_offset.min(limit),
            None => after_offset,
        }
    }
}

/// The names a parameter can have, kept apart from the identifiers so that a misspelling is
/// an error with a list in it rather than a parameter that is quietly ignored.
///
/// An unknown parameter has to be diagnosed rather than skipped. The standard allows an
/// implementation to define its own, and a program that uses one we do not have is a program
/// that expects something to happen; carrying on without it would produce an array with the
/// wrong contents and no message.
struct Names {
    limit: Symbol,
    prefix: Symbol,
    suffix: Symbol,
    if_empty: Symbol,
    gnu: Symbol,
    offset: Symbol,
}

impl Names {
    fn new(interner: &mut Interner) -> Names {
        Names {
            limit: interner.intern("limit"),
            prefix: interner.intern("prefix"),
            suffix: interner.intern("suffix"),
            if_empty: interner.intern("if_empty"),
            gnu: interner.intern("gnu"),
            offset: interner.intern("offset"),
        }
    }
}

/// Splits the tokens after `#embed` into the resource name and the parameters.
///
/// Returns how many tokens the name took. `<a/b.h>` reaches here as separate `<`, `a`, `/`,
/// `b`, `.`, `h`, `>` tokens when the name was computed by a macro, so the end of it is the
/// matching `>` rather than a fixed length. A quoted name is one string token.
pub(crate) fn header_length(line: &[Tok]) -> Option<usize> {
    match line.first()?.kind {
        PpTokenKind::HeaderName | PpTokenKind::StringLit => Some(1),
        PpTokenKind::Punct(Punct::Lt) => {
            let end = line.iter().position(|t| t.is(Punct::Gt))?;
            Some(end + 1)
        }
        _ => None,
    }
}

/// Reads the parameter list that follows the resource name.
///
/// `expand` is macro expansion, passed in rather than reached for, because this module has no
/// business owning the expander and the two callers differ: the directive expands, and so
/// does `__has_embed`, but they hold the preprocessor differently.
pub(crate) fn parse(
    line: &[Tok],
    at: Span,
    interner: &mut Interner,
    diagnostics: &mut Vec<Diagnostic>,
    expand: &mut dyn FnMut(Vec<Tok>, &mut Interner) -> Vec<Tok>,
) -> Option<Params> {
    let names = Names::new(interner);
    let mut params = Params::default();
    let mut seen: Vec<Symbol> = Vec::new();
    let mut rest = line;
    while let Some(first) = rest.first().copied() {
        let Some(name) = first.ident() else {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "expected an `#embed` parameter, found `{}`",
                        spelling(first, interner)
                    ),
                    first.report_span(),
                )
                .with_code("E0346"),
            );
            return None;
        };
        // The scoped spelling, `gnu::offset`. A scope we do not know is still a parameter we
        // do not implement, so it is diagnosed like any other: a vendor extension that is
        // silently dropped is worse than one that is refused, because the program asked for
        // something and did not get it.
        let (scope, name, used) = match (rest.get(1), rest.get(2).and_then(|t| t.ident())) {
            (Some(colons), Some(inner)) if colons.is(Punct::ColonColon) => {
                (Some(name), inner, 3usize)
            }
            _ => (None, name, 1usize),
        };
        rest = &rest[used..];
        let (operand, after) = match arguments(rest) {
            Some((operand, after)) => (operand, after),
            None => (&rest[..0], 0),
        };
        rest = &rest[after..];
        if seen.contains(&name) {
            diagnostics.push(
                Diagnostic::error("`#embed` parameter given twice", first.report_span())
                    .with_code("E0347"),
            );
            return None;
        }
        seen.push(name);
        let where_written = first.report_span();
        match (scope, name) {
            (None, n) if n == names.limit => {
                let limit = count(operand, "limit", where_written, interner, diagnostics, expand)?;
                params.limit = Some(limit);
            }
            (Some(s), n) if s == names.gnu && n == names.offset => {
                params.offset =
                    count(operand, "offset", where_written, interner, diagnostics, expand)?;
            }
            (None, n) if n == names.prefix => params.prefix = expand(operand.to_vec(), interner),
            (None, n) if n == names.suffix => params.suffix = expand(operand.to_vec(), interner),
            (None, n) if n == names.if_empty => {
                params.if_empty = expand(operand.to_vec(), interner);
            }
            _ => {
                let written = match scope {
                    Some(s) => format!("{}::{}", interner.resolve(s), interner.resolve(name)),
                    None => interner.resolve(name).to_owned(),
                };
                diagnostics.push(
                    Diagnostic::error(format!("unknown `#embed` parameter `{written}`"), at)
                        .with_code("E0349")
                        .note("known parameters are `limit`, `prefix`, `suffix`, `if_empty` and `gnu::offset`", at),
                );
                return None;
            }
        }
    }
    Some(params)
}

/// A parameter whose argument is a number, which is `limit` and `gnu::offset`.
///
/// A free function rather than a closure over the caller's locals, because it needs the
/// interner, the diagnostics and the expander at once and a closure holding all three would
/// stop the match arms around it from touching any of them.
fn count(
    operand: &[Tok],
    what: &str,
    at: Span,
    interner: &mut Interner,
    diagnostics: &mut Vec<Diagnostic>,
    expand: &mut dyn FnMut(Vec<Tok>, &mut Interner) -> Vec<Tok>,
) -> Option<u64> {
    let expanded = expand(operand.to_vec(), interner);
    let value = cond::value(&expanded, interner, diagnostics, at, what)?;
    if value < 0 {
        diagnostics.push(
            Diagnostic::error(format!("`{what}` must not be negative"), at).with_code("E0348"),
        );
        return None;
    }
    Some(value as u64)
}

/// The parenthesised argument of a parameter, and how many tokens it took including the
/// parentheses. `None` when the next token is not `(`, which is legal: a parameter is allowed
/// to have no argument, and none of the ones we implement do anything useful without one.
fn arguments(line: &[Tok]) -> Option<(&[Tok], usize)> {
    if !line.first()?.is(Punct::LParen) {
        return None;
    }
    let mut depth = 1u32;
    for (at, tok) in line.iter().enumerate().skip(1) {
        if tok.is(Punct::LParen) {
            depth += 1;
        } else if tok.is(Punct::RParen) {
            depth -= 1;
            if depth == 0 {
                return Some((&line[1..at], at + 1));
            }
        }
    }
    None
}

/// Writes the resource out as the tokens that replace the directive.
///
/// The whole replacement gets the span of the `#`, because there is nowhere else to point:
/// the bytes are not in a source file that has lines and columns, and a diagnostic about the
/// three hundredth of them is more useful pointing at the directive that produced it than at
/// an offset into a PNG.
pub(crate) fn tokens(
    bytes: &[u8],
    params: &Params,
    hash: Span,
    interner: &mut Interner,
    out: &mut Vec<Tok>,
) {
    let began = out.len();
    let taken = params.taken(bytes.len() as u64);
    if taken == 0 {
        // An empty resource is `if_empty` and nothing else. Not `prefix` and `suffix` with
        // nothing between them, which would be the obvious implementation and would put a
        // stray comma into the array the program was building.
        out.extend(params.if_empty.iter().copied());
        opens_the_line(out, began);
        return;
    }
    let start = params.offset as usize;
    let bytes = &bytes[start..start + taken as usize];
    // The tokens of `prefix(...)` keep the spacing they were written with inside the
    // parentheses, which is what GCC and clang both print, so `prefix(0xEF,)` produces
    // `0xEF,` with the comma against the number.
    out.extend(params.prefix.iter().copied());
    // Two hundred and fifty six interned strings, made once. Interning per byte would put a
    // hash of a short string in the inner loop of a directive whose whole reason to exist is
    // being faster than the array it replaces.
    let mut spellings: [Option<Symbol>; 256] = [None; 256];
    let mut first = true;
    for &byte in bytes {
        if !first {
            out.push(Tok::synthetic(
                PpTokenKind::Punct(Punct::Comma),
                None,
                TokenFlags::EMPTY,
                hash,
            ));
        }
        let sym =
            *spellings[byte as usize].get_or_insert_with(|| interner.intern(itoa(byte).as_str()));
        // A space before every number but the first, which is the spacing the reference
        // compilers print and so the spacing the differential expects. The first one has none
        // because it follows whatever `prefix` ended with, and a prefix ending in a comma
        // wants the number against it.
        let flags = if first { TokenFlags::EMPTY } else { TokenFlags::LEADING_SPACE };
        out.push(Tok::synthetic(PpTokenKind::Number, Some(sym), flags, hash));
        first = false;
    }
    // The suffix does get separated from the last number, because otherwise `suffix(,0xFE)`
    // runs a comma onto a digit and the line reads as though the comma belonged to the bytes.
    if let Some((head, tail)) = params.suffix.split_first() {
        let mut head = *head;
        head.flags = head.flags.with(TokenFlags::LEADING_SPACE);
        out.push(head);
        out.extend(tail.iter().copied());
    }
    opens_the_line(out, began);
}

/// Marks the first token of the replacement as starting a line, which is where the directive
/// was.
///
/// Without it the bytes run onto whatever the previous line ended with, and `x` `#embed` `y`
/// preprocesses to `x7` rather than to `x 7`. That is a different token, not a different
/// layout, so it is a correctness fix and not a cosmetic one.
fn opens_the_line(out: &mut [Tok], began: usize) {
    if let Some(first) = out.get_mut(began) {
        first.flags = first.flags.with(TokenFlags::START_OF_LINE);
    }
}

/// A byte as decimal text, without allocating.
struct Decimal {
    digits: [u8; 3],
    from: usize,
}

impl Decimal {
    fn as_str(&self) -> &str {
        // Every byte of `digits` from `from` on was written as an ASCII digit just above.
        std::str::from_utf8(&self.digits[self.from..]).unwrap_or("0")
    }
}

fn itoa(mut byte: u8) -> Decimal {
    let mut digits = [b'0'; 3];
    let mut from = 3;
    loop {
        from -= 1;
        digits[from] = b'0' + byte % 10;
        byte /= 10;
        if byte == 0 {
            return Decimal { digits, from };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_byte_is_written_in_decimal_without_leading_zeros() {
        assert_eq!(itoa(0).as_str(), "0");
        assert_eq!(itoa(7).as_str(), "7");
        assert_eq!(itoa(10).as_str(), "10");
        assert_eq!(itoa(99).as_str(), "99");
        assert_eq!(itoa(100).as_str(), "100");
        assert_eq!(itoa(255).as_str(), "255");
    }

    #[test]
    fn the_offset_is_applied_before_the_limit() {
        // `gnu::offset(4) limit(4)` on an eight byte file is the second half, not nothing and
        // not the first four. Applying the limit first would give four bytes and then move
        // past all of them.
        let params = Params { limit: Some(4), offset: 4, ..Params::default() };
        assert_eq!(params.taken(8), 4);
        assert_eq!(params.taken(6), 2);
        // An offset past the end is an empty embed rather than an error, so that a program
        // reading a file in chunks does not have to know the length to know when to stop.
        assert_eq!(params.taken(2), 0);
    }

    #[test]
    fn no_limit_is_not_a_limit_of_zero() {
        let params = Params::default();
        assert_eq!(params.taken(9), 9);
        let none_left = Params { limit: Some(0), ..Params::default() };
        assert_eq!(none_left.taken(9), 0);
    }
}
