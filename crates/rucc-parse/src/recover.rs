//! Error recovery: where to resume, and what keeps one error from becoming twenty.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.8.
//!
//! The goal is many useful errors from one run without cascades, and the strategy is per
//! construct rather than global. A statement that will not parse skips to the next `;` or `}`
//! at the current bracket depth. A declaration skips past its `;`, or past the `}` of the
//! function body it turned out to be. An expression skips nothing at all and puts an error node
//! in the tree where the operand should have been, because expressions are short and skipping
//! one costs the rest of the statement.
//!
//! # Why counting errors is not the mechanism
//!
//! Every recovery leaves a poisoned node behind, and a diagnostic about a poisoned node is not
//! reported. That is what actually stops the cascade. A flag saying "something already went
//! wrong here" is close enough to work on small inputs and wrong on real ones, because it
//! either suppresses errors in code that was fine or fails to suppress the third message about
//! the same broken subexpression.
//!
//! # What is not here yet
//!
//! The unclosed brace heuristic. Section 6.8 asks for the opening location plus a guess at the
//! intended closing point taken from the indentation, which is how a compiler avoids five
//! hundred errors at end of file. It needs the source map rather than the token stream, so it
//! lands with the diagnostic rendering rather than here.

use rucc_ast::{Ast, Decl, DeclId, Expr, ExprId, Stmt, StmtId};
use rucc_diag::{Diagnostic, Severity};
use rucc_lex::Punct;

use crate::cursor::Cursor;

/// How many errors are reported before the parser gives up.
///
/// The number is clang's, measured rather than assumed: clang 23.1 stops after twenty with
/// `too many errors emitted, stopping now`, and gcc 13.3 has no default limit at all and will
/// print every error a file produces. Twenty is the better default of the two, because the
/// errors after the twentieth in a file that is this broken are almost always consequences of
/// the ones before them, and the accepted flag for changing it is gcc's `-fmax-errors=N`, with
/// zero meaning no limit.
pub const DEFAULT_ERROR_LIMIT: usize = 20;

/// The diagnostics the parse produced, and the limit on how many it will produce.
#[derive(Debug)]
pub struct Errors {
    diagnostics: Vec<Diagnostic>,
    errors: usize,
    limit: usize,
    stopped: bool,
}

impl Default for Errors {
    fn default() -> Self {
        Errors::new(DEFAULT_ERROR_LIMIT)
    }
}

impl Errors {
    /// A sink that stops after `limit` errors, or that never stops when `limit` is zero.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Errors { diagnostics: Vec::new(), errors: 0, limit, stopped: false }
    }

    /// Records a diagnostic.
    ///
    /// Once the limit is reached nothing more is recorded, warnings included. The parser is
    /// about to stop and a warning arriving after the note that says so reads as though the
    /// compiler carried on regardless.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        if self.stopped {
            return;
        }
        let fatal = diagnostic.severity.is_fatal();
        let span = diagnostic.span;
        self.diagnostics.push(diagnostic);
        if fatal {
            self.errors += 1;
            if self.limit != 0 && self.errors >= self.limit {
                self.diagnostics.push(Diagnostic::new(
                    Severity::Note,
                    "too many errors emitted, stopping now",
                    span,
                ));
                self.stopped = true;
            }
        }
    }

    /// Records a diagnostic unless `about` is a node that recovery already poisoned.
    ///
    /// The suppression is deliberately shallow: it asks whether the node the message is about
    /// is itself poisoned, not whether anything underneath it is. A poisoned operand makes its
    /// parent poisoned at the point the parent is built, so the answer propagates through the
    /// tree rather than through a walk of it, and a walk here would make reporting an error
    /// cost the size of the subtree.
    pub fn push_about<P: Poison>(&mut self, ast: &Ast, about: P, diagnostic: Diagnostic) {
        if !about.is_poisoned(ast) {
            self.push(diagnostic);
        }
    }

    /// Whether the parser should stop, because it has reported as many errors as it will.
    #[inline]
    #[must_use]
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// How many errors have been reported. Warnings and notes are not counted.
    #[inline]
    #[must_use]
    pub fn errors(&self) -> usize {
        self.errors
    }

    /// Whether anything was reported at all.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// How many diagnostics were reported, of every severity.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.diagnostics.len()
    }

    /// What was reported, in the order it was reported.
    #[must_use]
    pub fn finish(self) -> Vec<Diagnostic> {
        self.diagnostics
    }
}

/// A node that recovery may have poisoned.
///
/// Implemented for the three node kinds that have an error variant, so that a diagnostic can be
/// held back by the node it is about whatever kind of node that is.
pub trait Poison: Copy {
    /// Whether this is a node recovery put in the tree rather than one the source asked for.
    fn is_poisoned(self, ast: &Ast) -> bool;
}

impl Poison for ExprId {
    #[inline]
    fn is_poisoned(self, ast: &Ast) -> bool {
        matches!(ast[self], Expr::Error)
    }
}

impl Poison for StmtId {
    #[inline]
    fn is_poisoned(self, ast: &Ast) -> bool {
        matches!(ast[self], Stmt::Error)
    }
}

impl Poison for DeclId {
    #[inline]
    fn is_poisoned(self, ast: &Ast) -> bool {
        matches!(ast[self], Decl::Error)
    }
}

/// Skips to the end of the statement the parser gave up on.
///
/// Stops just after the next `;` at the current bracket depth, or just before the `}` that
/// closes the block, whichever comes first. A `;` inside brackets is skipped over, because the
/// two in a `for` header do not end anything, and a `}` inside them belongs to a compound
/// literal or a statement expression and not to the enclosing block.
pub fn skip_to_statement_end(cursor: &mut Cursor<'_>) {
    let mut depth = 0u32;
    while !cursor.is_eof() {
        match cursor.current().punct() {
            Some(Punct::Semi) if depth == 0 => {
                cursor.bump();
                return;
            }
            Some(Punct::RBrace) if depth == 0 => return,
            Some(Punct::LBrace | Punct::LParen | Punct::LBracket) => depth += 1,
            Some(Punct::RBrace | Punct::RParen | Punct::RBracket) if depth > 0 => depth -= 1,
            _ => {}
        }
        cursor.bump();
    }
}

/// Skips past the declaration the parser gave up on.
///
/// Stops just after the `;` that ends it, or just after the `}` that ends the function body it
/// turned out to be. The second case is what keeps a broken function signature from costing the
/// next declaration as well: skipping to a `;` alone would run through the whole body and
/// swallow whatever followed it.
pub fn skip_past_declaration(cursor: &mut Cursor<'_>) {
    let mut depth = 0u32;
    while !cursor.is_eof() {
        match cursor.current().punct() {
            Some(Punct::Semi) if depth == 0 => {
                cursor.bump();
                return;
            }
            Some(Punct::LBrace | Punct::LParen | Punct::LBracket) => depth += 1,
            Some(Punct::RBrace | Punct::RParen | Punct::RBracket) if depth > 0 => {
                depth -= 1;
                if depth == 0 && cursor.at_punct(Punct::RBrace) {
                    cursor.bump();
                    // A `}` that ends a body ends the declaration, and a `}` that ends a record
                    // has a `;` after it that is part of the same declaration. Taking the `;`
                    // when it is there costs nothing and saves a second error on it.
                    cursor.eat_punct(Punct::Semi);
                    return;
                }
            }
            _ => {}
        }
        cursor.bump();
    }
}

#[cfg(test)]
mod tests {
    use rucc_diag::Span;
    use rucc_lex::{Token, TokenFlags, TokenKind};

    use super::*;

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
    fn a_statement_resumes_after_its_semicolon() {
        // ( ; ) ; ,
        let tokens =
            stream(&[Punct::LParen, Punct::Semi, Punct::RParen, Punct::Semi, Punct::Comma]);
        let mut cursor = Cursor::new(&tokens);
        skip_to_statement_end(&mut cursor);
        assert!(cursor.at_punct(Punct::Comma));
    }

    #[test]
    fn a_statement_stops_at_the_brace_that_closes_its_block() {
        let tokens = stream(&[Punct::Comma, Punct::RBrace, Punct::Semi]);
        let mut cursor = Cursor::new(&tokens);
        skip_to_statement_end(&mut cursor);
        assert!(cursor.at_punct(Punct::RBrace));
    }

    #[test]
    fn a_nested_block_does_not_end_the_statement() {
        // , { ; } ; ,
        let tokens = stream(&[
            Punct::Comma,
            Punct::LBrace,
            Punct::Semi,
            Punct::RBrace,
            Punct::Semi,
            Punct::Comma,
        ]);
        let mut cursor = Cursor::new(&tokens);
        skip_to_statement_end(&mut cursor);
        assert!(cursor.at_punct(Punct::Comma));
        assert_eq!(cursor.index(), 5);
    }

    #[test]
    fn a_declaration_resumes_after_the_body_it_turned_out_to_have() {
        // ( ) { ; } ,
        let tokens = stream(&[
            Punct::LParen,
            Punct::RParen,
            Punct::LBrace,
            Punct::Semi,
            Punct::RBrace,
            Punct::Comma,
        ]);
        let mut cursor = Cursor::new(&tokens);
        skip_past_declaration(&mut cursor);
        assert!(cursor.at_punct(Punct::Comma));
    }

    #[test]
    fn a_record_takes_the_semicolon_after_its_brace_with_it() {
        // { ; } ; ,
        let tokens =
            stream(&[Punct::LBrace, Punct::Semi, Punct::RBrace, Punct::Semi, Punct::Comma]);
        let mut cursor = Cursor::new(&tokens);
        skip_past_declaration(&mut cursor);
        assert!(cursor.at_punct(Punct::Comma));
    }

    #[test]
    fn a_declaration_resumes_after_its_semicolon() {
        let tokens = stream(&[Punct::Star, Punct::Semi, Punct::Comma]);
        let mut cursor = Cursor::new(&tokens);
        skip_past_declaration(&mut cursor);
        assert!(cursor.at_punct(Punct::Comma));
    }

    #[test]
    fn a_skip_always_reaches_the_end() {
        // A stray closer at depth zero is skipped rather than counted, so this terminates
        // instead of underflowing.
        let tokens = stream(&[Punct::RParen, Punct::RBracket, Punct::Comma]);
        let mut cursor = Cursor::new(&tokens);
        skip_to_statement_end(&mut cursor);
        assert!(cursor.is_eof());
        let mut cursor = Cursor::new(&tokens);
        skip_past_declaration(&mut cursor);
        assert!(cursor.is_eof());
    }

    #[test]
    fn the_limit_stops_the_run_and_says_so() {
        let mut errors = Errors::new(3);
        for _ in 0..10 {
            errors.push(Diagnostic::error("no", Span::empty_at(0)));
        }
        assert!(errors.stopped());
        assert_eq!(errors.errors(), 3);
        let diagnostics = errors.finish();
        assert_eq!(diagnostics.len(), 4);
        assert_eq!(diagnostics[3].severity, Severity::Note);
        assert_eq!(diagnostics[3].message, "too many errors emitted, stopping now");
    }

    #[test]
    fn a_limit_of_zero_never_stops() {
        let mut errors = Errors::new(0);
        for _ in 0..64 {
            errors.push(Diagnostic::error("no", Span::empty_at(0)));
        }
        assert!(!errors.stopped());
        assert_eq!(errors.len(), 64);
    }

    #[test]
    fn warnings_do_not_count_against_the_limit() {
        let mut errors = Errors::default();
        assert!(errors.is_empty());
        for _ in 0..64 {
            errors.push(Diagnostic::warning("hmm", Span::empty_at(0)));
        }
        assert_eq!(errors.errors(), 0);
        assert!(!errors.stopped());
    }

    #[test]
    fn a_poisoned_node_holds_back_the_message_about_it() {
        let mut ast = Ast::new();
        let bad = ast.expr(Expr::Error, Span::empty_at(0));
        let good = ast.expr(Expr::Bool(true), Span::new(0, 4));
        let mut errors = Errors::default();
        let at = Span::empty_at(0);
        errors.push_about(&ast, bad, Diagnostic::error("about the broken one", at));
        assert!(errors.is_empty());
        errors.push_about(&ast, good, Diagnostic::error("about the good one", at));
        assert_eq!(errors.len(), 1);
    }
}
