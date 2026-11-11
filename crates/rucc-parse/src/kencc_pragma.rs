//! The pragmas of kencc, which `-fkencc` reads in place of the pragmas of gcc.
//!
//! Design: tamnd/rucc#4150, and 06.5 of the rucc-plan9 plan.
//!
//! kencc reads its pragmas in `cc/macbody` and `cc/dpchk.c`. It acts on six of them and ignores
//! every other pragma with no message:
//!
//! - `pack on` and `pack yes` set the alignment of the members and the round of a record to 1.
//!   Any other word, or no word, sets them back to what the target gives. A number sets them to
//!   that number. As with gcc, the value at the closing brace of a record body applies.
//! - `lib "name"` asks the linker for a library. It goes to the driver as `#pragma comment(lib)`
//!   does, and the object writer of the Plan 9 targets puts it in the object.
//! - `incomplete` names a structure or a union whose members the type signature leaves out, as
//!   `pragincomplete` does. The line goes to the checker, which marks the record at the place of
//!   the line. The forms `_off_` and `_on_` change `-T` for the rest of the file in kencc, and no
//!   file of 9front has them, so they are ignored.
//! - `varargck` gives format checking under `-F`. The line goes to the checker, which reads it
//!   at the place of the line, as `pragvararg` does. A line that kencc cannot read is kept as
//!   bad, and the checker gives the kencc error only under `-F`, because kencc reads the line
//!   only then.
//! - `profile` and `fpround` change nothing that rucc writes yet.
//!
//! `#pragma once` never gets here, because the preprocessor reads it. kencc ignores it, so a
//! header included twice is read twice. A header that kencc reads twice with no error gives the
//! same declarations when rucc reads it once, so rucc keeps `once`.

use rucc_ast::RecordKind;
use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_lex::{Keyword, Punct, Token};

use crate::comment::Comment;
use crate::parser::Parser;

impl Parser<'_> {
    /// One `#pragma` line under `-fkencc`.
    pub(crate) fn kencc_pragma(&mut self, line: &[Token], span: Span) {
        let Some(word) = line.first().and_then(|token| token.ident()) else { return };
        match self.cx.interner.resolve(word) {
            "pack" => self.kencc_pack(&line[1..]),
            "lib" => self.kencc_lib(&line[1..], span),
            "incomplete" => self.kencc_incomplete(&line[1..], span),
            "varargck" => {
                let pragma = self.kencc_varargck(&line[1..]);
                let decls = self.pragma_place();
                self.varargck_pragmas.push(VarargckLine { pragma, span, decls });
            }
            _ => {}
        }
    }

    /// `#pragma pack`, as `pragpack` in `cc/dpchk.c` reads it.
    fn kencc_pack(&mut self, rest: &[Token]) {
        let bytes = match (rest.first(), rest.first().and_then(|token| token.ident())) {
            (_, Some(word)) => u32::from(matches!(self.cx.interner.resolve(word), "on" | "yes")),
            (Some(token), None) => {
                self.tokens.int(*token).and_then(|int| u32::try_from(int.value).ok()).unwrap_or(0)
            }
            (None, None) => 0,
        };
        self.packs.set_current(bytes);
    }

    /// `#pragma lib "name"`, which `macprag` in `cc/macbody` reads. The name is a plain string.
    fn kencc_lib(&mut self, mut rest: &[Token], span: Span) {
        match self.comment_text(&mut rest) {
            Some(name) if rest.is_empty() => self.comments.push(Comment::Lib(name)),
            _ => self.error("E0798", "syntax in `#pragma lib`", span),
        }
    }

    /// `#pragma incomplete`, as `pragincomplete` in `cc/dpchk.c` reads it: a name whose type is
    /// the record, or `struct` or `union` and a tag.
    fn kencc_incomplete(&mut self, rest: &[Token], span: Span) {
        let Some(first) = rest.first() else { return };
        let kind = match first.keyword() {
            Some(Keyword::Struct) => Some(RecordKind::Struct),
            Some(Keyword::Union) => Some(RecordKind::Union),
            _ => None,
        };
        let name = match kind {
            Some(_) => match rest.get(1).and_then(|token| token.ident()) {
                Some(name) => name,
                None => {
                    let what = if rest.len() < 2 {
                        "missing struct/union tag in pragma incomplete"
                    } else {
                        "invalid struct/union tag in pragma incomplete"
                    };
                    return self.error("E0798", what, span);
                }
            },
            None => match first.ident() {
                Some(name) => name,
                None => return,
            },
        };
        if kind.is_none() && matches!(self.cx.interner.resolve(name), "_off_" | "_on_") {
            return;
        }
        let decls = self.pragma_place();
        self.incomplete_pragmas.push(IncompletePragma { kind, name, span, decls });
    }
}

impl Parser<'_> {
    /// The token that the line being read stands ahead of. [`Parser::finish`] makes it a count
    /// of declarations.
    fn pragma_place(&self) -> u32 {
        u32::try_from(self.pragma_token).unwrap_or(u32::MAX)
    }

    /// `#pragma varargck`, as `pragvararg` in `cc/dpchk.c` reads it.
    fn kencc_varargck(&self, rest: &[Token]) -> Varargck {
        let word = rest.first().and_then(|token| token.ident());
        match word.map(|word| self.cx.interner.resolve(word)) {
            // `#pragma varargck argpos warn 2`
            Some("argpos") => {
                let Some(name) = rest.get(1).and_then(|token| token.ident()) else {
                    return Varargck::Bad;
                };
                let param = rest.get(2).and_then(|&token| self.tokens.int(token));
                match param.and_then(|int| u32::try_from(int.value).ok()) {
                    Some(param) => Varargck::Argpos { name, param },
                    None => Varargck::Bad,
                }
            }
            // `#pragma varargck flag 'c'`
            Some("flag") => {
                let flag = rest.get(1).and_then(|&token| self.tokens.character(token));
                match flag.and_then(|flag| u32::try_from(flag.value & 0xffff_ffff).ok()) {
                    Some(flag) if flag != u32::from(b'\n') => Varargck::Flag(flag),
                    _ => Varargck::Bad,
                }
            }
            // `#pragma varargck type "O" int`, with a `*` for each level of pointer.
            Some("type") => {
                let Some(format) = rest.get(1).and_then(|&token| self.tokens.string(token)) else {
                    return Varargck::Bad;
                };
                let format: Vec<u8> =
                    format.elements.iter().map(|&element| element as u8).collect();
                // `fmtbuf` holds 99 bytes, and a blank ends the line in kencc.
                if format.len() >= 100 || format.iter().any(|&byte| matches!(byte, b' ' | b'\n')) {
                    return Varargck::Bad;
                }
                let Some(&token) = rest.get(2) else { return Varargck::Bad };
                let ty = match (token.ident(), token.keyword()) {
                    (Some(name), _) => VarargckType::Name(name),
                    (None, Some(keyword)) => VarargckType::Keyword(keyword),
                    (None, None) => return Varargck::Bad,
                };
                let stars =
                    rest[3..].iter().take_while(|token| token.punct() == Some(Punct::Star)).count();
                Varargck::Type { format, ty, stars: u32::try_from(stars).unwrap_or(u32::MAX) }
            }
            _ => Varargck::Bad,
        }
    }
}

/// One `#pragma varargck` line and its place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarargckLine {
    /// What the line says.
    pub pragma: Varargck,
    /// Where the line is, for the messages.
    pub span: Span,
    /// How many external declarations of the unit end before the line. The checker reads the line
    /// just before it checks the next declaration. A line in a function body is read before the
    /// function. The place is kept as a count because the spans of a header and of the file that
    /// includes it are not in the order of the text.
    pub decls: u32,
}

/// What a `#pragma varargck` line says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Varargck {
    /// `argpos NAME N`: the argument `N` of a call of `NAME` is a format, counted from one.
    Argpos {
        /// The function.
        name: Symbol,
        /// The place of the format.
        param: u32,
    },
    /// `type "FMT" TYPE`: the verb `FMT`, with its flags, takes an argument of the type.
    Type {
        /// The verb and its flags, such as `lld`, as bytes.
        format: Vec<u8>,
        /// The name of the type.
        ty: VarargckType,
        /// The number of `*` after the name.
        stars: u32,
    },
    /// `flag 'c'`: the character is a flag that the verbs ignore.
    Flag(u32),
    /// A line that kencc cannot read, which gives `syntax in #pragma varargck` under `-F`.
    Bad,
}

/// The name of a type in `#pragma varargck type`, which is a typedef name or one of the
/// keywords that name a type in kencc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarargckType {
    /// A typedef name, or any other ordinary name.
    Name(Symbol),
    /// A keyword, such as `int` or `void`.
    Keyword(Keyword),
}

/// What one `#pragma incomplete` line names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncompletePragma {
    /// `struct` or `union` when the line names a tag, and nothing when it names an ordinary
    /// name whose type is the record.
    pub kind: Option<RecordKind>,
    /// The name.
    pub name: Symbol,
    /// Where the line is, for the messages.
    pub span: Span,
    /// How many external declarations of the unit end before the line. The checker reads the line
    /// just before it checks the next declaration. A line in a function body is read before the
    /// function. The place is kept as a count because the spans of a header and of the file that
    /// includes it are not in the order of the text.
    pub decls: u32,
}
