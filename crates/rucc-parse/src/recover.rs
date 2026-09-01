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
//! The sink that holds the diagnostics is [`Errors`], in `rucc-diag`, because
//! semantic analysis reports through the same one and the limit is a number for the compiler
//! rather than one per pass. What is here is the half of it that needs a tree: which nodes are
//! poisoned, and therefore which messages are held back.
//!
//! # What is not here yet
//!
//! The unclosed brace heuristic. Section 6.8 asks for the opening location plus a guess at the
//! intended closing point taken from the indentation, which is how a compiler avoids five
//! hundred errors at end of file. It needs the source map rather than the token stream, so it
//! lands with the diagnostic rendering rather than here.

use rucc_ast::{Ast, Decl, DeclId, Expr, ExprId, Stmt, StmtId};
use rucc_diag::{Diagnostic, Errors};
use rucc_lex::Punct;

use crate::cursor::Cursor;

/// Records a diagnostic unless `about` is a node the parser already poisoned.
///
/// A free function rather than a method, because the sink is in `rucc-diag` and what makes a
/// node poisoned is a fact about this tree. [`Errors::push_unless`] is the half that does not
/// need to know that.
pub fn push_about<P: Poison>(errors: &mut Errors, ast: &Ast, about: P, diagnostic: Diagnostic) {
    errors.push_unless(about.is_poisoned(ast), diagnostic);
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
    fn a_poisoned_node_holds_back_the_message_about_it() {
        let mut ast = Ast::new();
        let bad = ast.expr(Expr::Error, Span::empty_at(0));
        let good = ast.expr(Expr::Bool(true), Span::new(0, 4));
        let mut errors = Errors::default();
        let at = Span::empty_at(0);
        push_about(&mut errors, &ast, bad, Diagnostic::error("about the broken one", at));
        assert!(errors.is_empty());
        push_about(&mut errors, &ast, good, Diagnostic::error("about the good one", at));
        assert_eq!(errors.len(), 1);
    }
}
