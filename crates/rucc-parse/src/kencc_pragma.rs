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
//! - `varargck` and `incomplete` give format checking and type signatures. They act only under
//!   `-F` and `-T`, and they are read with that work.
//! - `profile` and `fpround` change nothing that rucc writes yet.
//!
//! `#pragma once` never gets here, because the preprocessor reads it. kencc ignores it, so a
//! header included twice is read twice. A header that kencc reads twice with no error gives the
//! same declarations when rucc reads it once, so rucc keeps `once`.

use rucc_diag::Span;
use rucc_lex::Token;

use crate::comment::Comment;
use crate::parser::Parser;

impl Parser<'_> {
    /// One `#pragma` line under `-fkencc`.
    pub(crate) fn kencc_pragma(&mut self, line: &[Token], span: Span) {
        let Some(word) = line.first().and_then(|token| token.ident()) else { return };
        match self.cx.interner.resolve(word) {
            "pack" => self.kencc_pack(&line[1..]),
            "lib" => self.kencc_lib(&line[1..], span),
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
}
