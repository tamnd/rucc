//! The token buffer the parser reads.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.3.
//!
//! The input is the slice of tokens phase 7 produced, which always ends in one
//! [`TokenKind::Eof`]. A cursor is a position in that slice and nothing more: it holds no
//! parser state, it builds nothing and it reports nothing. That is what makes saving one and
//! restoring it safe, and it is why the recovery skips in [`crate::recover`] take a cursor
//! rather than the whole parser.
//!
//! # Why the lookahead is bounded
//!
//! Unbounded backtracking is how a C parser becomes quadratic on the input a fuzzer eventually
//! finds, so [`Cursor::peek`] refuses to look further than [`MAX_LOOKAHEAD`] tokens ahead and
//! panics rather than quietly widening the window. The bound is a budget rather than a fact
//! about the grammar: a decision that cannot be made inside it is either a save and a restore,
//! which is deliberate and rare, or a sign that the decision is being made in the wrong place,
//! and the panic is how that conversation starts.

use rucc_diag::Span;
use rucc_lex::{Keyword, Punct, Token, TokenKind};

/// How far ahead [`Cursor::peek`] will look.
pub const MAX_LOOKAHEAD: usize = 4;

/// A saved position, taken by [`Cursor::save`] and given back to [`Cursor::restore`].
///
/// Opaque on purpose. A position is only meaningful for the cursor that produced it, and an
/// arbitrary index into the token stream is not something the parser should be able to invent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark(usize);

/// A position in the token stream.
#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    tokens: &'a [Token],
    at: usize,
}

impl<'a> Cursor<'a> {
    /// A cursor on the first token of `tokens`.
    ///
    /// # Panics
    ///
    /// Panics if `tokens` does not end in [`TokenKind::Eof`]. Every method here relies on that
    /// token being there: it is what a peek past the end returns and it is what stops every
    /// recovery skip, so a stream without one would turn a malformed file into a hang.
    #[must_use]
    pub fn new(tokens: &'a [Token]) -> Self {
        assert!(
            tokens.last().is_some_and(|token| token.is_eof()),
            "the token stream must end in `Eof`"
        );
        Cursor { tokens, at: 0 }
    }

    /// The token the parser is looking at.
    #[inline]
    #[must_use]
    pub fn current(&self) -> Token {
        self.tokens[self.at]
    }

    /// The token `n` places ahead, which is the final [`TokenKind::Eof`] once the end is
    /// reached rather than an out of range access.
    ///
    /// # Panics
    ///
    /// Panics if `n` is greater than [`MAX_LOOKAHEAD`].
    #[inline]
    #[must_use]
    pub fn peek(&self, n: usize) -> Token {
        assert!(n <= MAX_LOOKAHEAD, "lookahead of {n} tokens, past the bound of {MAX_LOOKAHEAD}");
        self.tokens[(self.at + n).min(self.tokens.len() - 1)]
    }

    /// Where the current token is, which is where a diagnostic about it points.
    #[inline]
    #[must_use]
    pub fn span(&self) -> Span {
        self.current().span
    }

    /// The empty span just after the previous token, which is where something that should have
    /// been written and was not belongs.
    ///
    /// Pointing a missing semicolon at the token that follows it is a small thing that reads
    /// badly, because the token that follows is usually on the next line and is not the
    /// problem. Before the first token there is no previous one, so this is the start of the
    /// current token instead.
    #[must_use]
    pub fn prev_end(&self) -> Span {
        match self.at.checked_sub(1) {
            Some(prev) => Span::empty_at(self.tokens[prev].span.hi),
            None => Span::empty_at(self.current().span.lo),
        }
    }

    /// Whether the parser has reached the end of the translation unit.
    #[inline]
    #[must_use]
    pub fn is_eof(&self) -> bool {
        self.current().is_eof()
    }

    /// Whether the current token is exactly `kind`.
    #[inline]
    #[must_use]
    pub fn at(&self, kind: TokenKind) -> bool {
        self.current().kind == kind
    }

    /// Whether the current token is the punctuator `punct`.
    #[inline]
    #[must_use]
    pub fn at_punct(&self, punct: Punct) -> bool {
        self.current().punct() == Some(punct)
    }

    /// Whether the current token is the keyword `keyword`.
    #[inline]
    #[must_use]
    pub fn at_keyword(&self, keyword: Keyword) -> bool {
        self.current().keyword() == Some(keyword)
    }

    /// Steps over the current token and gives it back.
    ///
    /// Stepping over the end is not an error and does not move: the cursor stays on the final
    /// [`TokenKind::Eof`], so a loop that forgets to check for the end runs out of tokens
    /// instead of reading off the end of the slice. It will still spin, which is what
    /// [`Cursor::index`] is for.
    #[inline]
    pub fn bump(&mut self) -> Token {
        let token = self.current();
        if !token.is_eof() {
            self.at += 1;
        }
        token
    }

    /// Steps over the current token if it is `kind`, and reports whether it did.
    #[inline]
    pub fn eat(&mut self, kind: TokenKind) -> bool {
        let matched = self.at(kind);
        if matched {
            self.bump();
        }
        matched
    }

    /// Steps over the current token if it is the punctuator `punct`.
    #[inline]
    pub fn eat_punct(&mut self, punct: Punct) -> bool {
        self.eat(TokenKind::Punct(punct))
    }

    /// Steps over the current token if it is the keyword `keyword`.
    #[inline]
    pub fn eat_keyword(&mut self, keyword: Keyword) -> bool {
        self.eat(TokenKind::Keyword(keyword))
    }

    /// How many tokens the cursor has stepped over.
    ///
    /// The parser's loops compare this across an iteration to check that they made progress. A
    /// production that returns without consuming anything is the classic way a recursive
    /// descent parser hangs on malformed input, and it is a bug in the parser rather than
    /// something to recover from, so the check belongs in an assertion and not in a `if`.
    #[inline]
    #[must_use]
    pub fn index(&self) -> usize {
        self.at
    }

    /// The current position, to be given back to [`Cursor::restore`].
    #[inline]
    #[must_use]
    pub fn save(&self) -> Mark {
        Mark(self.at)
    }

    /// Goes back to a saved position.
    ///
    /// This is not a general backtrack and it does not undo anything but the position. Between
    /// a save and a restore the parser must not report a diagnostic and must not put a node in
    /// the tree, because neither is taken back, and a speculative parse that leaves either
    /// behind produces an error about a reading of the source that was abandoned. The two
    /// constructs that need this are in `spec/06-lexer-and-parser.md` section 6.4.
    ///
    /// # Panics
    ///
    /// Panics if `mark` came from a cursor on a different token stream.
    #[inline]
    pub fn restore(&mut self, mark: Mark) {
        assert!(mark.0 < self.tokens.len(), "restoring a mark from another token stream");
        self.at = mark.0;
    }
}

#[cfg(test)]
mod tests {
    use rucc_lex::TokenFlags;

    use super::*;

    /// A stream of punctuators, one byte each, ending in `Eof`.
    fn stream(puncts: &[Punct]) -> Vec<Token> {
        let mut tokens: Vec<Token> = puncts
            .iter()
            .enumerate()
            .map(|(i, &punct)| Token {
                kind: TokenKind::Punct(punct),
                flags: TokenFlags::EMPTY,
                value: 0,
                span: Span::new(i as u32, i as u32 + 1),
            })
            .collect();
        let end = puncts.len() as u32;
        tokens.push(Token {
            kind: TokenKind::Eof,
            flags: TokenFlags::EMPTY,
            value: 0,
            span: Span::empty_at(end),
        });
        tokens
    }

    #[test]
    fn peeking_past_the_end_gives_eof() {
        let tokens = stream(&[Punct::Semi]);
        let cursor = Cursor::new(&tokens);
        assert!(cursor.peek(0).punct() == Some(Punct::Semi));
        assert!(cursor.peek(1).is_eof());
        assert!(cursor.peek(MAX_LOOKAHEAD).is_eof());
    }

    #[test]
    fn bumping_stops_on_the_end() {
        let tokens = stream(&[Punct::Semi]);
        let mut cursor = Cursor::new(&tokens);
        assert!(cursor.bump().punct() == Some(Punct::Semi));
        for _ in 0..3 {
            assert!(cursor.bump().is_eof());
        }
        assert_eq!(cursor.index(), 1);
    }

    #[test]
    fn eating_only_moves_when_it_matches() {
        let tokens = stream(&[Punct::Semi, Punct::Comma]);
        let mut cursor = Cursor::new(&tokens);
        assert!(!cursor.eat_punct(Punct::Comma));
        assert_eq!(cursor.index(), 0);
        assert!(cursor.eat_punct(Punct::Semi));
        assert!(cursor.at_punct(Punct::Comma));
        assert!(!cursor.eat_keyword(Keyword::Int));
    }

    #[test]
    fn restoring_puts_the_cursor_back() {
        let tokens = stream(&[Punct::LParen, Punct::Star, Punct::RParen]);
        let mut cursor = Cursor::new(&tokens);
        let mark = cursor.save();
        cursor.bump();
        cursor.bump();
        assert!(cursor.at_punct(Punct::RParen));
        cursor.restore(mark);
        assert!(cursor.at_punct(Punct::LParen));
    }

    #[test]
    fn a_missing_token_belongs_after_the_one_before_it() {
        let tokens = stream(&[Punct::LParen, Punct::RParen]);
        let mut cursor = Cursor::new(&tokens);
        assert_eq!(cursor.prev_end(), Span::empty_at(0));
        cursor.bump();
        assert_eq!(cursor.prev_end(), Span::empty_at(1));
    }

    #[test]
    #[should_panic(expected = "past the bound")]
    fn looking_too_far_ahead_is_a_bug() {
        let tokens = stream(&[Punct::Semi]);
        let _ = Cursor::new(&tokens).peek(MAX_LOOKAHEAD + 1);
    }

    #[test]
    #[should_panic(expected = "must end in `Eof`")]
    fn a_stream_without_an_end_is_rejected() {
        let _ = Cursor::new(&[]);
    }
}
