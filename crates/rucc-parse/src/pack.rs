//! `#pragma pack`, which is the one pragma the grammar has to know about.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.1 and `spec/13-gnu-compat.md` section 13.6.
//! What the number does to a layout is `spec/12-abi-and-runtime.md` section 12.6.
//!
//! A pragma is not a token in any production, so the conversion keeps the lines beside the
//! stream with the index of the token each one came before. That is enough to answer the only
//! question `pack` asks, which is what was in effect at a given point in the file.
//!
//! # Where it is asked
//!
//! At the closing brace of a record body, and nowhere else. GCC lays a record out when it has
//! read all of it, so the value in effect there is the one that applies, and a line written in
//! the middle of a body settles the whole record rather than the members after it:
//!
//! ```c
//! struct A { char c;
//! #pragma pack(1)
//!   int i; };
//! ```
//!
//! is five bytes aligned to one, not eight with a packed tail, and moving the `pack()` that
//! turns it off inside the body gives the eight back. Both of those were read off gcc 16 rather
//! than reasoned about, because the documentation does not say which point it takes.
//!
//! # Why the lines are read as the parse goes
//!
//! Reading all of them up front would be simpler and would put every complaint about a
//! malformed line ahead of every complaint about the code, which is not the order they were
//! written in. So the lines are consumed as the parser walks past them, and whatever is left
//! over is read at the end of the unit, which is where a line after the last record lands.

use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_lex::{Token, TokenKind};

use crate::parser::Parser;

/// The `#pragma pack` lines read so far, and what they add up to.
#[derive(Debug, Default)]
pub(crate) struct Packs {
    /// How many of the unit's pragma lines have been read.
    next: usize,
    /// What is in effect, in bytes, with [`None`] meaning the target's own alignments stand.
    current: Option<u32>,
    /// What `push` saved, innermost last.
    stack: Vec<Option<u32>>,
}

/// The alignments `#pragma pack` takes, which is what both GCC and MSVC accept.
///
/// Sixteen is the largest because it is the largest alignment any scalar has, and zero is in
/// the set because GCC reads it as the request to stop packing. A number outside the set is a
/// mistake rather than a request, and GCC ignores the line and says so.
const ALLOWED: [u32; 6] = [0, 1, 2, 4, 8, 16];

/// What one `#pragma pack` line asks for, once the line has been read.
///
/// A line is read whole before any of it is applied, so that a `pop` whose parentheses never
/// closed is one complaint about the line rather than a complaint about the stack as well.
#[derive(Debug, Clone, Copy)]
enum Action {
    /// `pack()`, `pack(0)` and `pack(n)`. Zero is the same as writing nothing between the
    /// parentheses, which is what GCC does with it and why the two are one variant here.
    Set(u32),
    /// `pack(push[, id][, n])`, which saves what is in effect and then sets it when a number
    /// was written.
    Push(Option<u32>),
    /// `pack(pop[, id])`, carrying the name so that the complaint can repeat it.
    Pop(Option<Symbol>),
}

impl Parser<'_> {
    /// What `#pragma pack` is in effect at the cursor, reading whatever lines it has walked past.
    pub(crate) fn pack_in_effect(&mut self) -> Option<u32> {
        self.read_packs(self.cursor.index());
        self.packs.current
    }

    /// Reads the lines after the last record in the unit, which nothing else would reach.
    ///
    /// A `pop` with nothing under it is a mistake wherever it is written, and one written after
    /// the last structure in the file is the same mistake as one written before the first.
    pub(crate) fn finish_packs(&mut self) {
        self.read_packs(usize::MAX);
    }

    /// Applies every pragma line that stands ahead of token `to`.
    ///
    /// The bound is exclusive, so a line written between the closing brace and whatever follows
    /// it belongs to the next record rather than to the one that just closed.
    fn read_packs(&mut self, to: usize) {
        while let Some(pragma) = self.tokens.pragmas.get(self.packs.next) {
            if pragma.before as usize >= to {
                return;
            }
            self.packs.next += 1;
            // Cloned because applying the line reports through the parser, which is the same
            // borrow the line is being read out of.
            let (line, span) = (pragma.tokens.clone(), pragma.span);
            self.pack_line(&line, span);
        }
    }

    /// One `#pragma` line, which is ignored unless it is a `pack`.
    fn pack_line(&mut self, line: &[Token], span: Span) {
        let Some(first) = line.first() else { return };
        if first.ident().is_none_or(|name| self.cx.interner.resolve(name) != "pack") {
            return;
        }
        let mut rest = &line[1..];
        if !eat_punct(&mut rest, "(") {
            self.warn("E0677", "missing `(` after `#pragma pack` - ignored", span);
            return;
        }
        let Some(action) = self.pack_action(&mut rest, span) else { return };
        if !eat_punct(&mut rest, ")") {
            // The form is named in the complaint, which is what tells a reader of it that a
            // `push` takes more between the parentheses than a bare number does.
            let form = match action {
                Action::Set(_) => "`#pragma pack`",
                Action::Push(_) => "`#pragma pack(push[, id][, <n>])`",
                Action::Pop(_) => "`#pragma pack(pop[, id])`",
            };
            self.warn("E0677", format!("malformed {form} - ignored"), span);
            return;
        }
        self.apply_pack(action, span);
        self.junk(rest, span);
    }

    /// What the line says between the parentheses, with the closing one left where it is.
    fn pack_action(&mut self, rest: &mut &[Token], span: Span) -> Option<Action> {
        if rest.is_empty() {
            self.warn("E0677", "malformed `#pragma pack` - ignored", span);
            return None;
        }
        // `#pragma pack()` puts it back to what the target says, which is what a header writes
        // after the structures it wanted packed.
        if rest.first().is_some_and(is_rparen) {
            return Some(Action::Set(0));
        }
        let Some(word) = rest.first().and_then(|token| token.ident()) else {
            return Some(Action::Set(self.pack_number(rest, span)?));
        };
        match self.cx.interner.resolve(word) {
            "push" => {
                *rest = &rest[1..];
                self.pack_push(rest, span)
            }
            "pop" => {
                *rest = &rest[1..];
                let named = if eat_punct(rest, ",") { self.pack_name(rest) } else { None };
                Some(Action::Pop(named))
            }
            other => {
                let what = format!("unknown action `{other}` for `#pragma pack` - ignored");
                self.warn("E0681", what, span);
                None
            }
        }
    }

    /// The rest of a `push`, which is a name, a number, both or neither.
    ///
    /// The identifier form is MSVC's and names the entry so that a later `pop, id` finds it.
    /// The name is read and dropped: a program that writes one matches its pushes and its pops
    /// anyway, and honouring the name would mean popping several entries at once.
    fn pack_push(&mut self, rest: &mut &[Token], span: Span) -> Option<Action> {
        if !eat_punct(rest, ",") {
            return Some(Action::Push(None));
        }
        if self.pack_name(rest).is_some() && !eat_punct(rest, ",") {
            return Some(Action::Push(None));
        }
        Some(Action::Push(Some(self.pack_number(rest, span)?)))
    }

    /// Applies a line that read cleanly.
    fn apply_pack(&mut self, action: Action, span: Span) {
        match action {
            Action::Set(bytes) => self.packs.current = in_effect(bytes),
            Action::Push(bytes) => {
                self.packs.stack.push(self.packs.current);
                if let Some(bytes) = bytes {
                    self.packs.current = in_effect(bytes);
                }
            }
            Action::Pop(named) => match self.packs.stack.pop() {
                Some(saved) => self.packs.current = saved,
                None => {
                    // GCC writes the two forms with different spacing, and these are its words.
                    let what = match named {
                        Some(name) => {
                            let name = self.cx.interner.resolve(name);
                            format!(
                                "`#pragma pack(pop, {name})` encountered without matching \
                                 `#pragma pack(push, {name})`"
                            )
                        }
                        None => "`#pragma pack (pop)` encountered without matching \
                                 `#pragma pack (push)`"
                            .to_string(),
                    };
                    self.warn("E0678", what, span);
                }
            },
        }
    }

    /// The `n` of a `pack(n)`, which is the alignment the line asks for.
    fn pack_number(&mut self, rest: &mut &[Token], span: Span) -> Option<u32> {
        let Some(value) =
            rest.first().and_then(|token| self.tokens.int(*token)).map(|int| int.value)
        else {
            self.warn("E0677", "malformed `#pragma pack` - ignored", span);
            return None;
        };
        *rest = &rest[1..];
        let allowed = u32::try_from(value).is_ok_and(|value| ALLOWED.contains(&value));
        if !allowed {
            let what = format!("alignment must be a small power of two, not {value}");
            self.warn("E0679", what, span);
            return None;
        }
        u32::try_from(value).ok()
    }

    /// The identifier of an MSVC `push, id` or `pop, id`, if that is what comes next.
    fn pack_name(&mut self, rest: &mut &[Token]) -> Option<Symbol> {
        let name = rest.first().and_then(|token| token.ident())?;
        *rest = &rest[1..];
        Some(name)
    }

    /// Whatever is left on the line after the pragma was read, which GCC calls junk.
    fn junk(&mut self, rest: &[Token], span: Span) {
        if !rest.is_empty() {
            self.warn("E0680", "junk at end of `#pragma pack`", span);
        }
    }
}

/// What a number written on a line leaves in effect, where zero is the target's own alignments.
fn in_effect(bytes: u32) -> Option<u32> {
    (bytes != 0).then_some(bytes)
}

/// Whether a token is the `)` that ends the line.
fn is_rparen(token: &Token) -> bool {
    matches!(token.kind, TokenKind::Punct(punct) if punct.as_str() == ")")
}

/// Takes one punctuator off the front of a line, and says whether it was there.
fn eat_punct(rest: &mut &[Token], spelling: &str) -> bool {
    let found = match rest.first().map(|token| token.kind) {
        Some(TokenKind::Punct(punct)) => punct.as_str() == spelling,
        _ => false,
    };
    if found {
        *rest = &rest[1..];
    }
    found
}
