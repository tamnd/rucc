//! The parser itself: the state every production shares, and the helpers they all use.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.3.
//!
//! The productions live in the modules beside this one and are written as inherent methods on
//! [`Parser`], so they read as one recursive descent parser split across files rather than as a
//! set of functions passing state to each other. What is here is the state, the diagnostics, and
//! the small number of decisions that more than one production needs.

use rucc_ast::{Ast, Decl, DeclId, Expr, ExprId, Stmt, StmtId, StrId};
use rucc_base::{Interner, Symbol};
use rucc_diag::{DEFAULT_ERROR_LIMIT, Diagnostic, Errors, Span};
use rucc_lex::{Keyword, Punct, Token, TokenKind, Tokens};
use rucc_session::Std;

use crate::cursor::Cursor;
use crate::scope::Scopes;

/// How deeply brackets may nest before the parser gives up.
///
/// Recursive descent uses the machine stack for the grammar's nesting, so a file with a
/// thousand open parentheses is a stack overflow rather than a diagnostic unless something
/// stops it. The number is clang's `-fbracket-depth` default, which is the one real code has
/// been measured against, and it is far above anything a human writes and far below anything
/// that costs the stack more than a fraction of a megabyte.
pub const MAX_NESTING: usize = 256;

/// Everything the parser needs that is not the tokens.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The spellings, for the diagnostics that name an identifier.
    pub interner: &'a Interner,
    /// The dialect, which decides whether an old-style definition is an error and whether a
    /// C23 construct is one.
    pub std: Std,
    /// Whether the GNU extensions are on, which is `-std=gnu17` rather than `-std=c17`.
    pub gnu: bool,
    /// Whether `-pedantic` was given.
    pub pedantic: bool,
    /// How many errors to report before stopping, with zero meaning no limit.
    pub error_limit: usize,
}

impl<'a> Context<'a> {
    /// A context with the defaults, for a caller that only has an interner to hand.
    #[must_use]
    pub fn new(interner: &'a Interner, std: Std) -> Context<'a> {
        Context { interner, std, gnu: true, pedantic: false, error_limit: DEFAULT_ERROR_LIMIT }
    }
}

/// What one parse produced.
#[derive(Debug)]
pub struct Parsed {
    /// The tree, which holds poisoned nodes where the source did not parse.
    pub ast: Ast,
    /// What went wrong, in the order it was found.
    pub diagnostics: Vec<Diagnostic>,
}

impl Parsed {
    /// Whether anything was reported at an error severity.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity.is_fatal())
    }
}

/// The parser.
#[derive(Debug)]
pub struct Parser<'a> {
    pub(crate) cursor: Cursor<'a>,
    pub(crate) tokens: &'a Tokens,
    pub(crate) scopes: Scopes,
    pub(crate) errors: Errors,
    pub(crate) ast: Ast,
    pub(crate) cx: Context<'a>,
    /// How many brackets are open, for [`MAX_NESTING`].
    depth: usize,
    /// Whether the nesting cap has already been reported, since reporting it at every level of
    /// a thousand deep nesting is a thousand copies of the same message.
    too_deep: bool,
    /// The `#pragma pack` lines read so far, which is in `pack.rs` with the code that reads them.
    pub(crate) packs: crate::pack::Packs,
}

impl<'a> Parser<'a> {
    /// A parser over `tokens`.
    #[must_use]
    pub fn new(tokens: &'a Tokens, cx: Context<'a>) -> Parser<'a> {
        Parser {
            cursor: Cursor::new(&tokens.tokens),
            tokens,
            scopes: Scopes::new(),
            errors: Errors::new(cx.error_limit),
            ast: Ast::new(),
            cx,
            depth: 0,
            too_deep: false,
            packs: crate::pack::Packs::default(),
        }
    }

    /// The tree and the diagnostics, once the parse is over.
    #[must_use]
    pub fn finish(self) -> Parsed {
        Parsed { ast: self.ast, diagnostics: self.errors.finish() }
    }

    /// Reports an error at `span`.
    pub(crate) fn error(&mut self, code: &'static str, message: impl Into<String>, span: Span) {
        self.errors.push(Diagnostic::error(message, span).with_code(code));
    }

    /// Reports a warning at `span`.
    pub(crate) fn warn(&mut self, code: &'static str, message: impl Into<String>, span: Span) {
        self.errors.push(Diagnostic::warning(message, span).with_code(code));
    }

    /// Reports a warning that only `-pedantic` asks for.
    pub(crate) fn pedantic(&mut self, code: &'static str, message: impl Into<String>, span: Span) {
        if self.cx.pedantic {
            self.warn(code, message, span);
        }
    }

    /// Whether the parse should stop, because the error limit was reached.
    pub(crate) fn stopped(&self) -> bool {
        self.errors.stopped()
    }

    /// How a token is named in a diagnostic.
    pub(crate) fn describe(&self, token: Token) -> String {
        match token.kind {
            TokenKind::Eof => "end of file".to_string(),
            TokenKind::Punct(punct) => format!("`{}`", punct.as_str()),
            TokenKind::Keyword(word) => format!("`{}`", word.as_str()),
            TokenKind::Ident => {
                format!("`{}`", self.cx.interner.resolve(Symbol::from_raw(token.value)))
            }
            TokenKind::Int => "an integer constant".to_string(),
            TokenKind::Float => "a floating constant".to_string(),
            TokenKind::Char => "a character constant".to_string(),
            TokenKind::Str => "a string literal".to_string(),
        }
    }

    /// Consumes `punct`, or reports that it is missing without consuming anything.
    ///
    /// The message points at the end of the previous token rather than at the token that turned
    /// up, because a missing semicolon belongs at the end of the line it is missing from and not
    /// at the start of the next one.
    pub(crate) fn expect_punct(&mut self, punct: Punct) -> bool {
        if self.cursor.eat_punct(punct) {
            return true;
        }
        let found = self.describe(self.cursor.current());
        let message = format!("expected `{}`, found {found}", punct.as_str());
        let at = if punct == Punct::Semi { self.cursor.prev_end() } else { self.cursor.span() };
        self.error("E0400", message, at);
        false
    }

    /// Consumes `keyword`, or reports that it is missing.
    pub(crate) fn expect_keyword(&mut self, keyword: Keyword) -> bool {
        if self.cursor.eat_keyword(keyword) {
            return true;
        }
        let found = self.describe(self.cursor.current());
        let message = format!("expected `{}`, found {found}", keyword.as_str());
        self.error("E0400", message, self.cursor.span());
        false
    }

    /// Consumes an identifier and gives back its symbol and span.
    pub(crate) fn expect_ident(&mut self) -> Option<(Symbol, Span)> {
        if let Some(name) = self.cursor.current().ident() {
            let span = self.cursor.span();
            self.cursor.bump();
            return Some((name, span));
        }
        let found = self.describe(self.cursor.current());
        self.error("E0401", format!("expected an identifier, found {found}"), self.cursor.span());
        None
    }

    /// A string literal, copied out of the token stream and into the tree.
    ///
    /// The literal rather than the expression: an `asm` template and a `static_assert` message
    /// are strings in the grammar and not operands, so nothing is allowed to concatenate an
    /// identifier onto one or take its address.
    pub(crate) fn string_literal(&mut self) -> Option<StrId> {
        let token = self.cursor.current();
        if token.kind == TokenKind::Str {
            self.cursor.bump();
            let literal = self.tokens.strings[token.value as usize].clone();
            return Some(self.ast.add_string(literal));
        }
        let found = self.describe(token);
        self.error("E0409", format!("expected a string literal, found {found}"), token.span);
        None
    }

    /// Opens a bracket, and reports the one time the nesting is too deep to continue.
    ///
    /// A caller that is refused must not recurse. It steps over the token that would have
    /// opened the bracket and produces a poisoned node, which is what keeps the outer loops
    /// making progress rather than meeting the same token again.
    #[must_use]
    pub(crate) fn enter(&mut self) -> bool {
        if self.depth >= MAX_NESTING {
            if !self.too_deep {
                self.too_deep = true;
                self.error(
                    "E0402",
                    format!("brackets nested more deeply than {MAX_NESTING} levels"),
                    self.cursor.span(),
                );
            }
            return false;
        }
        self.depth += 1;
        true
    }

    /// Closes a bracket opened by [`Parser::enter`].
    pub(crate) fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Adds an expression to the tree.
    pub(crate) fn add_expr(&mut self, expr: Expr, span: Span) -> ExprId {
        self.ast.expr(expr, span)
    }

    /// Adds a statement to the tree.
    pub(crate) fn add_stmt(&mut self, stmt: Stmt, span: Span) -> StmtId {
        self.ast.stmt(stmt, span)
    }

    /// Adds a declaration to the tree.
    pub(crate) fn add_decl(&mut self, decl: Decl, span: Span) -> DeclId {
        self.ast.decl(decl, span)
    }

    /// An expression node standing in for one that did not parse.
    pub(crate) fn poison_expr(&mut self, span: Span) -> ExprId {
        self.ast.expr(Expr::Error, span)
    }

    /// A statement node standing in for one that did not parse.
    pub(crate) fn poison_stmt(&mut self, span: Span) -> StmtId {
        self.ast.stmt(Stmt::Error, span)
    }

    /// A declaration node standing in for one that did not parse.
    pub(crate) fn poison_decl(&mut self, span: Span) -> DeclId {
        self.ast.decl(Decl::Error, span)
    }

    /// The span from `start` to the end of the token before the current one.
    pub(crate) fn span_from(&self, start: Span) -> Span {
        start.to(self.cursor.prev_end())
    }
}
