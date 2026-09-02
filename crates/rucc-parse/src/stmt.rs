//! Statements, and the blocks they live in.
//!
//! Design: `spec/06-lexer-and-parser.md` sections 6.5 and 6.7.
//!
//! Nothing here is desugared: a `for` stays a `for`, a `do` keeps its test at the bottom, and a
//! `switch` keeps its cases where they were written. The one thing the shape of the code here
//! does differ from the grammar on is the run of labels in front of a statement, which the
//! grammar nests and this parses with a loop, for the reason on [`Parser::labeled`].
//!
//! # Where a block item is decided
//!
//! A block item is a declaration or a statement, and telling them apart is the typedef question
//! again: `T * x;` declares a pointer when `T` is a type name and multiplies when it is not. Two
//! things are checked before that, in order. A `__label__` is GNU's block-local label list and
//! is neither. An identifier immediately followed by `:` is a label, which is checked first
//! because a typedef name is still a perfectly good label name.

use rucc_ast::{
    Asm, AsmId, AsmOperand, AsmOperandList, AsmQuals, AttrList, Expr, ExprId, ForInit, Stmt,
    StmtId, StmtList, StrList, SymbolList,
};
use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_lex::{Keyword, Punct};

use crate::parser::Parser;
use crate::recover::skip_to_statement_end;

/// A label whose statement has not been parsed yet.
///
/// Collected by [`Parser::labeled`] while it walks the run, and turned into a node once the
/// statement they all label is known.
enum Pending {
    /// `name:`, with the attributes written in front of it.
    Label { name: Symbol, attrs: AttrList },
    /// `case lo:`, or GNU's `case lo ... hi:`.
    Case { lo: ExprId, hi: Option<ExprId> },
    /// `default:`.
    Default,
}

impl Parser<'_> {
    /// A `{ ... }`, which the caller has already checked for.
    pub(crate) fn compound_stmt(&mut self) -> StmtId {
        let start = self.cursor.span();
        if !self.enter() {
            self.cursor.bump();
            return self.poison_stmt(start);
        }
        self.cursor.bump();
        self.scopes.push();
        let items = self.block_items();
        self.scopes.pop();
        self.expect_punct(Punct::RBrace);
        self.leave();
        let span = self.span_from(start);
        self.add_stmt(Stmt::Compound(items), span)
    }

    /// The items of a block, up to the `}` that closes it.
    fn block_items(&mut self) -> StmtList {
        let mut items = Vec::new();
        while !self.cursor.at_punct(Punct::RBrace) && !self.cursor.is_eof() && !self.stopped() {
            let before = self.cursor.index();
            let item = self.block_item();
            items.push(item);
            if self.cursor.index() == before {
                // Nothing consumed the token, so it starts no item at all. Stepping over it is
                // what keeps this from being an infinite loop; the error is already reported.
                self.cursor.bump();
            }
        }
        self.ast.add_stmt_list(&items)
    }

    /// One item of a block: a declaration, or a statement.
    pub(crate) fn block_item(&mut self) -> StmtId {
        let start = self.cursor.span();
        let attrs = self.leading_attributes();
        self.block_item_with(attrs, start)
    }

    /// One item of a block, with the attributes in front of it already read.
    fn block_item_with(&mut self, attrs: AttrList, start: Span) -> StmtId {
        if self.at_any_label() {
            return self.labeled(attrs, start);
        }
        // Attributes and then a `;` is an attribute declaration, which is how `[[fallthrough]];`
        // is written and which is a declaration rather than an empty statement with something on
        // it.
        let is_decl =
            self.starts_declaration() || (!attrs.is_empty() && self.cursor.at_punct(Punct::Semi));
        if is_decl {
            let decl = self.declaration(attrs, start);
            let span = self.ast.decl_span(decl);
            return self.add_stmt(Stmt::Decl(decl), span);
        }
        if !attrs.is_empty() {
            // C23 allows attributes on any statement and this tree has nowhere to keep them
            // except on a label, so they are dropped. Saying so is the difference between a
            // construct that does nothing and a construct that silently does nothing.
            let span = self.span_from(start);
            self.warn("E0411", "attributes on this statement are ignored", span);
        }
        self.statement()
    }

    /// The attributes written here, in either syntax, and an empty list when there are none.
    pub(crate) fn leading_attributes(&mut self) -> AttrList {
        if self.at_attribute() { self.attributes() } else { AttrList::EMPTY }
    }

    /// Whether what comes next can only be a declaration.
    pub(crate) fn starts_declaration(&self) -> bool {
        self.at_attribute() || self.cursor.at_keyword(Keyword::StaticAssert) || self.at_decl_specs()
    }

    /// One statement.
    pub(crate) fn statement(&mut self) -> StmtId {
        let start = self.cursor.span();
        if let Some(punct) = self.cursor.current().punct() {
            match punct {
                Punct::LBrace => return self.compound_stmt(),
                Punct::Semi => {
                    self.cursor.bump();
                    return self.add_stmt(Stmt::Empty, start);
                }
                _ => {}
            }
        }
        if let Some(word) = self.cursor.current().keyword() {
            if let Some(stmt) = self.keyword_stmt(word, start) {
                return stmt;
            }
        }
        if self.at_any_label() {
            return self.labeled(AttrList::EMPTY, start);
        }
        self.expr_stmt(start)
    }

    /// The statement a keyword introduces, and [`None`] when the keyword starts an expression
    /// instead, as `sizeof` and `_Generic` and the builtins do.
    fn keyword_stmt(&mut self, word: Keyword, start: Span) -> Option<StmtId> {
        let stmt = match word {
            Keyword::If => self.if_stmt(start),
            Keyword::Switch => self.switch_stmt(start),
            Keyword::While => self.while_stmt(start),
            Keyword::Do => self.do_stmt(start),
            Keyword::For => self.for_stmt(start),
            Keyword::Goto => self.goto_stmt(start),
            Keyword::Continue | Keyword::Break => self.jump_stmt(word, start),
            Keyword::Return => self.return_stmt(start),
            Keyword::Case | Keyword::Default => self.labeled(AttrList::EMPTY, start),
            Keyword::Asm => self.asm_stmt(start),
            Keyword::Label => self.local_labels(start),
            _ => return None,
        };
        Some(stmt)
    }

    /// `if (cond) then`, with the `else` that may follow it.
    ///
    /// The `else` binds to the innermost `if` that has not got one, which is what taking it here
    /// rather than after returning does, and which is what C says.
    fn if_stmt(&mut self, start: Span) -> StmtId {
        self.cursor.bump();
        let cond = self.controlling_expr();
        let then = self.statement();
        let otherwise =
            if self.cursor.eat_keyword(Keyword::Else) { Some(self.statement()) } else { None };
        let span = self.span_from(start);
        self.add_stmt(Stmt::If { cond, then, otherwise }, span)
    }

    /// `switch (scrutinee) body`.
    fn switch_stmt(&mut self, start: Span) -> StmtId {
        self.cursor.bump();
        let scrutinee = self.controlling_expr();
        let body = self.statement();
        let span = self.span_from(start);
        self.add_stmt(Stmt::Switch { scrutinee, body }, span)
    }

    /// `while (cond) body`.
    fn while_stmt(&mut self, start: Span) -> StmtId {
        self.cursor.bump();
        let cond = self.controlling_expr();
        let body = self.statement();
        let span = self.span_from(start);
        self.add_stmt(Stmt::While { cond, body }, span)
    }

    /// `do body while (cond);`.
    fn do_stmt(&mut self, start: Span) -> StmtId {
        self.cursor.bump();
        let body = self.statement();
        self.expect_keyword(Keyword::While);
        let cond = self.controlling_expr();
        self.expect_punct(Punct::Semi);
        let span = self.span_from(start);
        self.add_stmt(Stmt::DoWhile { body, cond }, span)
    }

    /// `for (init; cond; step) body`.
    ///
    /// One scope covers the header and the body together, so that the `i` of `for (int i = 0;;)`
    /// is visible in the body and gone after the loop.
    fn for_stmt(&mut self, start: Span) -> StmtId {
        self.cursor.bump();
        if !self.enter() {
            self.cursor.bump();
            return self.poison_stmt(start);
        }
        self.scopes.push();
        self.expect_punct(Punct::LParen);
        let init = self.for_init();
        let cond = if self.cursor.at_punct(Punct::Semi) { None } else { Some(self.expr()) };
        self.expect_punct(Punct::Semi);
        let step = if self.cursor.at_punct(Punct::RParen) { None } else { Some(self.expr()) };
        self.expect_punct(Punct::RParen);
        let body = self.statement();
        self.scopes.pop();
        self.leave();
        let span = self.span_from(start);
        self.add_stmt(Stmt::For { init, cond, step, body }, span)
    }

    /// The first clause of a `for`, which takes its own `;` with it.
    fn for_init(&mut self) -> ForInit {
        if self.cursor.eat_punct(Punct::Semi) {
            return ForInit::None;
        }
        if self.starts_declaration() {
            let start = self.cursor.span();
            let attrs = self.leading_attributes();
            return ForInit::Decl(self.declaration(attrs, start));
        }
        let value = self.expr();
        self.expect_punct(Punct::Semi);
        ForInit::Expr(value)
    }

    /// `goto name;`, or GNU's `goto *expr;`.
    fn goto_stmt(&mut self, start: Span) -> StmtId {
        self.cursor.bump();
        let stmt = if self.cursor.eat_punct(Punct::Star) {
            Stmt::GotoExpr(self.expr())
        } else {
            match self.expect_ident() {
                Some((name, _)) => Stmt::Goto(name),
                None => Stmt::Error,
            }
        };
        self.expect_punct(Punct::Semi);
        let span = self.span_from(start);
        self.add_stmt(stmt, span)
    }

    /// `continue;` or `break;`.
    fn jump_stmt(&mut self, word: Keyword, start: Span) -> StmtId {
        self.cursor.bump();
        self.expect_punct(Punct::Semi);
        let stmt = if word == Keyword::Continue { Stmt::Continue } else { Stmt::Break };
        let span = self.span_from(start);
        self.add_stmt(stmt, span)
    }

    /// `return expr;`, or `return;`.
    fn return_stmt(&mut self, start: Span) -> StmtId {
        self.cursor.bump();
        let value = if self.cursor.at_punct(Punct::Semi) { None } else { Some(self.expr()) };
        self.expect_punct(Punct::Semi);
        let span = self.span_from(start);
        self.add_stmt(Stmt::Return(value), span)
    }

    /// `__label__ a, b;`, GNU's block-local labels.
    ///
    /// A macro that expands to a block with a label in it needs these, because two expansions in
    /// one function would otherwise declare the same label twice.
    fn local_labels(&mut self, start: Span) -> StmtId {
        self.cursor.bump();
        let mut names = Vec::new();
        while let Some((name, _)) = self.expect_ident() {
            names.push(name);
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::Semi);
        let names = self.ast.add_symbol_list(&names);
        let span = self.span_from(start);
        self.add_stmt(Stmt::LocalLabels(names), span)
    }

    /// An expression evaluated for its effect.
    fn expr_stmt(&mut self, start: Span) -> StmtId {
        let value = self.expr();
        // A missing `;` after an expression that did not parse is a second message about the
        // same mistake, so the recovery happens without it.
        let broken = matches!(self.ast[value], Expr::Error);
        if broken || !self.expect_punct(Punct::Semi) {
            skip_to_statement_end(&mut self.cursor);
        }
        let span = self.span_from(start);
        self.add_stmt(Stmt::Expr(value), span)
    }

    /// Whether a label of any of the three kinds comes next.
    fn at_any_label(&self) -> bool {
        let token = self.cursor.current();
        if matches!(token.keyword(), Some(Keyword::Case | Keyword::Default)) {
            return true;
        }
        token.ident().is_some() && self.cursor.peek(1).punct() == Some(Punct::Colon)
    }

    /// A run of labels and the statement they all label.
    ///
    /// The grammar nests these, so the obvious parser reads one label and recurses for the
    /// statement after it. That costs a stack frame per label, and a run of three hundred `case`
    /// labels with the body on the last one is something a generated dispatch table really
    /// contains. The run is collected in a loop instead and folded into nodes afterwards, which
    /// leaves the same tree and a bounded stack.
    fn labeled(&mut self, attrs: AttrList, start: Span) -> StmtId {
        let mut labels = Vec::new();
        let mut attrs = attrs;
        let mut at = start;
        loop {
            let label = if self.cursor.eat_keyword(Keyword::Case) {
                let lo = self.const_expr();
                let hi = if self.cursor.eat_punct(Punct::Ellipsis) {
                    Some(self.const_expr())
                } else {
                    None
                };
                Pending::Case { lo, hi }
            } else if self.cursor.eat_keyword(Keyword::Default) {
                Pending::Default
            } else {
                // An identifier and a colon, which is what `at_any_label` matched.
                Pending::Label { name: Symbol::from_raw(self.cursor.bump().value), attrs }
            };
            self.expect_punct(Punct::Colon);
            labels.push((label, at));
            at = self.cursor.span();
            attrs = self.leading_attributes();
            if !self.at_any_label() {
                break;
            }
        }
        // Whatever the last round read attributes for is not a label, so they belong to the
        // statement being labelled.
        let body = self.labeled_body(attrs, at);
        let end = self.cursor.prev_end();
        let mut inner = body;
        for (label, at) in labels.into_iter().rev() {
            let stmt = match label {
                Pending::Label { name, attrs } => Stmt::Label { name, body: inner, attrs },
                Pending::Case { lo, hi } => Stmt::Case { lo, hi, body: inner },
                Pending::Default => Stmt::Default { body: inner },
            };
            inner = Some(self.add_stmt(stmt, at.to(end)));
        }
        match inner {
            Some(stmt) => stmt,
            // The run had at least one label in it, so this is unreachable.
            None => self.poison_stmt(start),
        }
    }

    /// What a run of labels labels, which since C23 may be nothing at all.
    fn labeled_body(&mut self, attrs: AttrList, at: Span) -> Option<StmtId> {
        if self.cursor.at_punct(Punct::RBrace) || self.cursor.is_eof() {
            return None;
        }
        Some(self.block_item_with(attrs, at))
    }

    /// The `( cond )` of an `if`, a `switch`, a `while` or a `do`.
    fn controlling_expr(&mut self) -> ExprId {
        let at = self.cursor.span();
        if !self.enter() {
            self.cursor.bump();
            return self.poison_expr(at);
        }
        self.expect_punct(Punct::LParen);
        let cond = self.expr();
        self.expect_punct(Punct::RParen);
        self.leave();
        cond
    }

    /// An `asm` statement.
    fn asm_stmt(&mut self, start: Span) -> StmtId {
        let asm = self.asm_body(start);
        self.expect_punct(Punct::Semi);
        let span = self.span_from(start);
        match asm {
            Some(asm) => self.add_stmt(Stmt::Asm(asm), span),
            None => self.poison_stmt(span),
        }
    }

    /// An `asm` from its keyword through the closing parenthesis, without the `;`.
    ///
    /// The same production serves the statement and the file-scope form, which is a declaration
    /// and takes its own `;`. All of GCC's sections are here, including the labels of an
    /// `asm goto`, because the kernel uses them.
    pub(crate) fn asm_body(&mut self, start: Span) -> Option<AsmId> {
        self.cursor.bump();
        let mut quals = AsmQuals::NONE;
        loop {
            let qual = match self.cursor.current().keyword() {
                Some(Keyword::Volatile) => AsmQuals::VOLATILE,
                Some(Keyword::Inline) => AsmQuals::INLINE,
                Some(Keyword::Goto) => AsmQuals::GOTO,
                _ => break,
            };
            self.cursor.bump();
            quals = quals.with(qual);
        }
        if !self.enter() {
            self.cursor.bump();
            return None;
        }
        self.expect_punct(Punct::LParen);
        let template = self.string_literal();
        let mut outputs = AsmOperandList::EMPTY;
        let mut inputs = AsmOperandList::EMPTY;
        let mut clobbers = StrList::EMPTY;
        let mut labels = SymbolList::EMPTY;
        // `::` is one token since C23, and `asm("" :: "r" (x))` is how half the kernel writes an
        // input-only statement, so a colon may arrive as half of one.
        let mut half = false;
        if self.asm_colon(&mut half) && !half {
            outputs = self.asm_operands();
        }
        if self.asm_colon(&mut half) && !half {
            inputs = self.asm_operands();
        }
        if self.asm_colon(&mut half) && !half {
            clobbers = self.asm_clobbers();
        }
        if self.asm_colon(&mut half) && !half {
            labels = self.asm_labels();
        }
        self.expect_punct(Punct::RParen);
        self.leave();
        let span = self.span_from(start);
        let template = template?;
        Some(self.ast.add_asm(Asm { template, outputs, inputs, clobbers, labels, quals, span }))
    }

    /// Steps over the `:` that starts the next `asm` section, taking a `::` as two.
    ///
    /// `half` holds the second colon of a `::` that has been read but not used yet. A section
    /// reached through one is empty, which is the whole point of writing it that way.
    fn asm_colon(&mut self, half: &mut bool) -> bool {
        if *half {
            *half = false;
            return true;
        }
        if self.cursor.eat_punct(Punct::Colon) {
            return true;
        }
        if self.cursor.eat_punct(Punct::ColonColon) {
            *half = true;
            return true;
        }
        false
    }

    /// Whether the current `asm` section has run out.
    fn at_asm_section_end(&self) -> bool {
        self.cursor.is_eof()
            || matches!(
                self.cursor.current().punct(),
                Some(Punct::Colon | Punct::ColonColon | Punct::RParen)
            )
    }

    /// The operands of one `asm` section.
    fn asm_operands(&mut self) -> AsmOperandList {
        let mut out = Vec::new();
        while !self.at_asm_section_end() {
            let before = self.cursor.index();
            let at = self.cursor.span();
            let name = if self.cursor.eat_punct(Punct::LBracket) {
                let name = self.expect_ident().map(|(name, _)| name);
                self.expect_punct(Punct::RBracket);
                name
            } else {
                None
            };
            let Some(constraint) = self.string_literal() else { break };
            let value = if self.enter() {
                self.expect_punct(Punct::LParen);
                let value = self.expr();
                self.expect_punct(Punct::RParen);
                self.leave();
                value
            } else {
                self.cursor.bump();
                self.poison_expr(at)
            };
            let span = self.span_from(at);
            out.push(AsmOperand { name, constraint, value, span });
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
            if self.cursor.index() == before {
                break;
            }
        }
        self.ast.add_asm_operand_list(&out)
    }

    /// The clobber list of an `asm`.
    fn asm_clobbers(&mut self) -> StrList {
        let mut out = Vec::new();
        while !self.at_asm_section_end() {
            match self.string_literal() {
                Some(clobber) => out.push(clobber),
                None => break,
            }
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.ast.add_str_list(&out)
    }

    /// The labels an `asm goto` may jump to.
    fn asm_labels(&mut self) -> SymbolList {
        let mut out = Vec::new();
        while !self.at_asm_section_end() {
            match self.expect_ident() {
                Some((name, _)) => out.push(name),
                None => break,
            }
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.ast.add_symbol_list(&out)
    }
}
