//! Expressions, as a Pratt parser over a precedence table.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.3.
//!
//! C has fifteen levels of binary precedence. Writing them as fifteen mutually recursive
//! functions is the textbook approach and it means fifteen calls to reach an identifier, so the
//! binary part is one loop over a table instead, which is both shorter to read and measurably
//! faster. The prefix and postfix parts stay as ordinary recursive descent, because they are not
//! a precedence problem: they are a small set of shapes, each of which is its own production.
//!
//! # Where the entry points differ
//!
//! Three of them, and the difference is only which binding power the loop starts at. A full
//! expression takes the comma operator, an assignment expression does not, and a constant
//! expression takes neither the comma nor the assignment. That is exactly what the grammar says
//! in three places that would otherwise be three functions.

use rucc_ast::{BinaryOp, Designator, Expr, ExprId, ExprList, GenericAssoc, TypeNameId, UnaryOp};
use rucc_base::Symbol;
use rucc_lex::{Keyword, Punct, Token, TokenKind};

use crate::parser::Parser;

/// What a punctuator does when it turns up after an operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Infix {
    /// An ordinary binary operator.
    Binary(BinaryOp),
    /// `=`, or a compound assignment with its operator.
    Assign(Option<BinaryOp>),
    /// `,`, which is not a binary operator because it is a sequence point that throws its left
    /// operand away.
    Comma,
    /// `?`, which takes a third operand and its own closing token.
    Cond,
}

/// The binding powers, lowest first.
///
/// Left and right power rather than one power and an associativity flag: an operator binds
/// tighter on the side whose number is larger, so `(4, 3)` is right associative and `(23, 24)`
/// is left associative, and the loop needs no second test. The numbers are gaps of two so that
/// the three entry points can sit between the levels.
fn infix(punct: Punct) -> Option<(Infix, u8, u8)> {
    use BinaryOp as B;
    let (what, lbp, rbp) = match punct {
        Punct::Comma => (Infix::Comma, 1, 2),
        Punct::Eq => (Infix::Assign(None), 4, 3),
        Punct::StarEq => (Infix::Assign(Some(B::Mul)), 4, 3),
        Punct::SlashEq => (Infix::Assign(Some(B::Div)), 4, 3),
        Punct::PercentEq => (Infix::Assign(Some(B::Rem)), 4, 3),
        Punct::PlusEq => (Infix::Assign(Some(B::Add)), 4, 3),
        Punct::MinusEq => (Infix::Assign(Some(B::Sub)), 4, 3),
        Punct::ShlEq => (Infix::Assign(Some(B::Shl)), 4, 3),
        Punct::ShrEq => (Infix::Assign(Some(B::Shr)), 4, 3),
        Punct::AmpEq => (Infix::Assign(Some(B::BitAnd)), 4, 3),
        Punct::CaretEq => (Infix::Assign(Some(B::BitXor)), 4, 3),
        Punct::PipeEq => (Infix::Assign(Some(B::BitOr)), 4, 3),
        Punct::Question => (Infix::Cond, 6, 5),
        Punct::PipePipe => (Infix::Binary(B::LogOr), 7, 8),
        Punct::AmpAmp => (Infix::Binary(B::LogAnd), 9, 10),
        Punct::Pipe => (Infix::Binary(B::BitOr), 11, 12),
        Punct::Caret => (Infix::Binary(B::BitXor), 13, 14),
        Punct::Amp => (Infix::Binary(B::BitAnd), 15, 16),
        Punct::EqEq => (Infix::Binary(B::Eq), 17, 18),
        Punct::Ne => (Infix::Binary(B::Ne), 17, 18),
        Punct::Lt => (Infix::Binary(B::Lt), 19, 20),
        Punct::Gt => (Infix::Binary(B::Gt), 19, 20),
        Punct::Le => (Infix::Binary(B::Le), 19, 20),
        Punct::Ge => (Infix::Binary(B::Ge), 19, 20),
        Punct::Shl => (Infix::Binary(B::Shl), 21, 22),
        Punct::Shr => (Infix::Binary(B::Shr), 21, 22),
        Punct::Plus => (Infix::Binary(B::Add), 23, 24),
        Punct::Minus => (Infix::Binary(B::Sub), 23, 24),
        Punct::Star => (Infix::Binary(B::Mul), 25, 26),
        Punct::Slash => (Infix::Binary(B::Div), 25, 26),
        Punct::Percent => (Infix::Binary(B::Rem), 25, 26),
        _ => return None,
    };
    Some((what, lbp, rbp))
}

/// The binding power a full expression starts at, which takes everything.
const EXPRESSION: u8 = 1;
/// The binding power an assignment expression starts at, which leaves out the comma.
const ASSIGNMENT: u8 = 3;
/// The binding power a constant expression starts at, which leaves out the assignment too.
const CONDITIONAL: u8 = 5;

impl Parser<'_> {
    /// An `expression`, comma operator included.
    pub(crate) fn expr(&mut self) -> ExprId {
        self.expr_bp(EXPRESSION)
    }

    /// An `assignment-expression`, which is what an argument and an initializer are.
    pub(crate) fn assign_expr(&mut self) -> ExprId {
        self.expr_bp(ASSIGNMENT)
    }

    /// A `constant-expression`, which is a conditional expression the grammar promises is
    /// constant. Nothing here checks that it is; that is the constant evaluator's job.
    pub(crate) fn const_expr(&mut self) -> ExprId {
        self.expr_bp(CONDITIONAL)
    }

    /// The operator at the cursor, when there is one and it binds tightly enough to belong to
    /// this call rather than to the one that asked for `min_bp`.
    fn infix_here(&self, min_bp: u8) -> Option<(Infix, u8)> {
        let (what, lbp, rbp) = infix(self.cursor.current().punct()?)?;
        if lbp < min_bp {
            return None;
        }
        Some((what, rbp))
    }

    /// The precedence loop.
    fn expr_bp(&mut self, min_bp: u8) -> ExprId {
        let mut lhs = self.cast_expr();
        while let Some((what, rbp)) = self.infix_here(min_bp) {
            self.cursor.bump();
            let start = self.ast.expr_span(lhs);
            lhs = match what {
                Infix::Cond => {
                    // GNU leaves the middle out in `a ?: b`, which evaluates `a` once. It is a
                    // different node rather than a rewrite, because rewriting it would need a
                    // temporary that the tree has no way to talk about.
                    let then =
                        if self.cursor.at_punct(Punct::Colon) { None } else { Some(self.expr()) };
                    self.expect_punct(Punct::Colon);
                    let otherwise = self.expr_bp(rbp);
                    let span = start.to(self.ast.expr_span(otherwise));
                    self.add_expr(Expr::Cond { cond: lhs, then, otherwise }, span)
                }
                Infix::Comma => {
                    let rhs = self.expr_bp(rbp);
                    let span = start.to(self.ast.expr_span(rhs));
                    self.add_expr(Expr::Comma { lhs, rhs }, span)
                }
                Infix::Assign(op) => {
                    let rhs = self.expr_bp(rbp);
                    let span = start.to(self.ast.expr_span(rhs));
                    self.add_expr(Expr::Assign { op, lhs, rhs }, span)
                }
                Infix::Binary(op) => {
                    let rhs = self.expr_bp(rbp);
                    let span = start.to(self.ast.expr_span(rhs));
                    self.add_expr(Expr::Binary { op, lhs, rhs }, span)
                }
            };
        }
        lhs
    }

    /// A `cast-expression`, which is a unary expression or a type name in parentheses applied
    /// to one.
    ///
    /// The parenthesis is the one place the grammar needs the scopes, because `(A)*B` is a cast
    /// when `A` is a type name and a multiplication when it is not. Once the type name is read
    /// the second ambiguity is one token wide: a `{` after the closing parenthesis makes it a
    /// compound literal, which is an object rather than a conversion.
    fn cast_expr(&mut self) -> ExprId {
        if !self.at_parenthesised_type() {
            return self.unary();
        }
        let start = self.cursor.span();
        if !self.enter() {
            self.cursor.bump();
            return self.poison_expr(start);
        }
        self.cursor.bump();
        let ty = self.type_name();
        self.expect_punct(Punct::RParen);
        self.leave();
        if self.cursor.at_punct(Punct::LBrace) {
            let init = self.braced_init();
            let span = self.span_from(start);
            let literal = self.add_expr(Expr::CompoundLiteral { ty, init }, span);
            return self.postfix_ops(literal);
        }
        let operand = self.cast_expr();
        let span = start.to(self.ast.expr_span(operand));
        self.add_expr(Expr::Cast { ty, operand }, span)
    }

    /// Whether the parser is looking at a parenthesised type name.
    ///
    /// One token of lookahead past the parenthesis, which is all the grammar needs: the token
    /// after a `(` decides between a type name and an expression on its own, since a type name
    /// always starts with a keyword that names a type, a qualifier, or an identifier the scopes
    /// say is a typedef name.
    pub(crate) fn at_parenthesised_type(&self) -> bool {
        self.cursor.at_punct(Punct::LParen) && self.starts_type_name(self.cursor.peek(1))
    }

    /// A `unary-expression`.
    fn unary(&mut self) -> ExprId {
        let start = self.cursor.span();
        let token = self.cursor.current();
        if let Some(punct) = token.punct() {
            // The operand of `++` and `--` is a unary expression and the operand of the rest is
            // a cast expression, which is why `-(int)x` parses and `++(int)x` does not.
            let prefix = match punct {
                Punct::PlusPlus => Some((UnaryOp::PreInc, true)),
                Punct::MinusMinus => Some((UnaryOp::PreDec, true)),
                Punct::Amp => Some((UnaryOp::AddrOf, false)),
                Punct::Star => Some((UnaryOp::Deref, false)),
                Punct::Plus => Some((UnaryOp::Plus, false)),
                Punct::Minus => Some((UnaryOp::Minus, false)),
                Punct::Tilde => Some((UnaryOp::BitNot, false)),
                Punct::Bang => Some((UnaryOp::Not, false)),
                _ => None,
            };
            if let Some((op, unary_operand)) = prefix {
                self.cursor.bump();
                let operand = if unary_operand { self.unary() } else { self.cast_expr() };
                let span = start.to(self.ast.expr_span(operand));
                return self.add_expr(Expr::Unary { op, operand }, span);
            }
            // `&&x` is the address of the label `x`, GNU's, and not the address of an address.
            // The lexer has already joined the two ampersands, so there is nothing to undo.
            if punct == Punct::AmpAmp {
                self.cursor.bump();
                let name = self.expect_ident();
                let span = self.span_from(start);
                return match name {
                    Some((name, _)) => self.add_expr(Expr::LabelAddr(name), span),
                    None => self.poison_expr(span),
                };
            }
        }
        if let Some(word) = token.keyword() {
            match word {
                Keyword::Sizeof => return self.sizeof_expr(),
                Keyword::Alignof | Keyword::GnuAlignof => return self.alignof_expr(),
                Keyword::Extension => {
                    self.cursor.bump();
                    let operand = self.cast_expr();
                    let span = start.to(self.ast.expr_span(operand));
                    return self.add_expr(Expr::Extension(operand), span);
                }
                Keyword::Real | Keyword::Imag => {
                    self.cursor.bump();
                    let op = if word == Keyword::Real { UnaryOp::Real } else { UnaryOp::Imag };
                    let operand = self.cast_expr();
                    let span = start.to(self.ast.expr_span(operand));
                    return self.add_expr(Expr::Unary { op, operand }, span);
                }
                _ => {}
            }
        }
        let base = self.primary();
        self.postfix_ops(base)
    }

    /// `sizeof expr` or `sizeof (type)`.
    fn sizeof_expr(&mut self) -> ExprId {
        let start = self.cursor.span();
        self.cursor.bump();
        if let Some(ty) = self.parenthesised_type_operand() {
            // `sizeof (T){ ... }` measures the compound literal and not the type, which is why
            // the type name alone is not the answer until the `{` has been ruled out.
            if self.cursor.at_punct(Punct::LBrace) {
                let init = self.braced_init();
                let span = self.span_from(start);
                let literal = self.add_expr(Expr::CompoundLiteral { ty, init }, span);
                let operand = self.postfix_ops(literal);
                let span = self.span_from(start);
                return self.add_expr(Expr::SizeofExpr(operand), span);
            }
            let span = self.span_from(start);
            return self.add_expr(Expr::SizeofType(ty), span);
        }
        let operand = self.unary();
        let span = start.to(self.ast.expr_span(operand));
        self.add_expr(Expr::SizeofExpr(operand), span)
    }

    /// `alignof (type)`, or GNU's `__alignof__ expr`.
    fn alignof_expr(&mut self) -> ExprId {
        let start = self.cursor.span();
        self.cursor.bump();
        if let Some(ty) = self.parenthesised_type_operand() {
            let span = self.span_from(start);
            return self.add_expr(Expr::AlignofType(ty), span);
        }
        let operand = self.unary();
        let span = start.to(self.ast.expr_span(operand));
        self.add_expr(Expr::AlignofExpr(operand), span)
    }

    /// Reads `(type-name)` when that is what comes next, and nothing when it is not.
    fn parenthesised_type_operand(&mut self) -> Option<TypeNameId> {
        if !self.at_parenthesised_type() {
            return None;
        }
        self.cursor.bump();
        let ty = self.type_name();
        self.expect_punct(Punct::RParen);
        Some(ty)
    }

    /// The suffixes that bind tighter than any prefix: subscript, call, member, and the two
    /// that are written after their operand.
    fn postfix_ops(&mut self, mut base: ExprId) -> ExprId {
        loop {
            let start = self.ast.expr_span(base);
            let Some(punct) = self.cursor.current().punct() else { break };
            match punct {
                Punct::LBracket => {
                    if !self.enter() {
                        self.cursor.bump();
                        break;
                    }
                    self.cursor.bump();
                    let index = self.expr();
                    self.expect_punct(Punct::RBracket);
                    self.leave();
                    let span = self.span_from(start);
                    base = self.add_expr(Expr::Index { base, index }, span);
                }
                Punct::LParen => {
                    if !self.enter() {
                        self.cursor.bump();
                        break;
                    }
                    self.cursor.bump();
                    let args = self.call_arguments();
                    self.expect_punct(Punct::RParen);
                    self.leave();
                    let span = self.span_from(start);
                    base = self.add_expr(Expr::Call { callee: base, args }, span);
                }
                Punct::Dot | Punct::Arrow => {
                    let arrow = punct == Punct::Arrow;
                    self.cursor.bump();
                    let span_before = start;
                    match self.expect_ident() {
                        Some((name, _)) => {
                            let span = self.span_from(span_before);
                            base = self.add_expr(Expr::Member { base, name, arrow }, span);
                        }
                        None => {
                            let span = self.span_from(span_before);
                            base = self.poison_expr(span);
                        }
                    }
                }
                Punct::PlusPlus | Punct::MinusMinus => {
                    let op =
                        if punct == Punct::PlusPlus { UnaryOp::PostInc } else { UnaryOp::PostDec };
                    self.cursor.bump();
                    let span = self.span_from(start);
                    base = self.add_expr(Expr::Unary { op, operand: base }, span);
                }
                _ => break,
            }
        }
        base
    }

    /// The arguments of a call, with the opening parenthesis already consumed.
    fn call_arguments(&mut self) -> ExprList {
        let mut args = Vec::new();
        if !self.cursor.at_punct(Punct::RParen) {
            loop {
                let before = self.cursor.index();
                args.push(self.assign_expr());
                if !self.cursor.eat_punct(Punct::Comma) {
                    break;
                }
                // An argument that consumed nothing means the token is not the start of any
                // expression, and going round again would not consume it either.
                if self.cursor.index() == before {
                    break;
                }
            }
        }
        self.ast.add_expr_list(&args)
    }

    /// A `primary-expression`, plus the GNU and C23 constructs that are written like one.
    fn primary(&mut self) -> ExprId {
        let token = self.cursor.current();
        let start = token.span;
        match token.kind {
            TokenKind::Ident => {
                self.cursor.bump();
                self.add_expr(Expr::Name(Symbol::from_raw(token.value)), start)
            }
            TokenKind::Int | TokenKind::Float | TokenKind::Char | TokenKind::Str => {
                self.cursor.bump();
                let expr = self.constant(token);
                self.add_expr(expr, start)
            }
            TokenKind::Keyword(word) => self.primary_keyword(word),
            TokenKind::Punct(Punct::LParen) => self.parenthesised(),
            _ => {
                let found = self.describe(token);
                self.error("E0403", format!("expected an expression, found {found}"), start);
                self.poison_expr(start)
            }
        }
    }

    /// The constant a token refers to, copied out of the token stream and into the tree.
    ///
    /// The tree owns its constants, because it outlives the token stream and because the
    /// printer and everything after it should not have to hold both.
    fn constant(&mut self, token: Token) -> Expr {
        let index = token.value as usize;
        match token.kind {
            TokenKind::Int => Expr::Int(self.ast.add_int(self.tokens.ints[index])),
            TokenKind::Float => Expr::Float(self.ast.add_float(self.tokens.floats[index])),
            TokenKind::Char => Expr::Char(self.ast.add_char(self.tokens.chars[index])),
            TokenKind::Str => {
                let literal = self.tokens.strings[index].clone();
                Expr::Str(self.ast.add_string(literal))
            }
            _ => Expr::Error,
        }
    }

    /// A keyword where an operand was expected.
    fn primary_keyword(&mut self, word: Keyword) -> ExprId {
        let start = self.cursor.span();
        match word {
            Keyword::True => {
                self.cursor.bump();
                self.add_expr(Expr::Bool(true), start)
            }
            Keyword::False => {
                self.cursor.bump();
                self.add_expr(Expr::Bool(false), start)
            }
            Keyword::Nullptr => {
                self.cursor.bump();
                self.add_expr(Expr::Nullptr, start)
            }
            Keyword::Generic => self.generic_selection(),
            Keyword::BuiltinOffsetof => self.builtin_offsetof(),
            Keyword::BuiltinChooseExpr => self.builtin_choose_expr(),
            Keyword::BuiltinTypesCompatibleP => self.builtin_types_compatible(),
            Keyword::BuiltinVaArg => self.builtin_va_arg(),
            Keyword::BuiltinVaStart => self.builtin_va_start(),
            Keyword::BuiltinVaEnd => self.builtin_va_end(),
            Keyword::BuiltinVaCopy => self.builtin_va_copy(),
            _ => {
                let found = self.describe(self.cursor.current());
                self.error("E0403", format!("expected an expression, found {found}"), start);
                self.poison_expr(start)
            }
        }
    }

    /// `(expr)`, or GNU's `({ statements })`.
    fn parenthesised(&mut self) -> ExprId {
        let start = self.cursor.span();
        if !self.enter() {
            self.cursor.bump();
            return self.poison_expr(start);
        }
        self.cursor.bump();
        let result = if self.cursor.at_punct(Punct::LBrace) {
            // A statement expression's value is the value of its last statement, which is a
            // semantic rule rather than a syntactic one, so the block is kept as a block.
            let body = self.compound_stmt();
            let span = self.span_from(start);
            self.add_expr(Expr::StmtExpr(body), span)
        } else {
            self.expr()
        };
        self.expect_punct(Punct::RParen);
        self.leave();
        result
    }

    /// `_Generic(control, type: value, ..., default: value)`.
    fn generic_selection(&mut self) -> ExprId {
        let start = self.cursor.span();
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return self.poison_expr(start);
        }
        let control = self.assign_expr();
        let mut assocs = Vec::new();
        while self.cursor.eat_punct(Punct::Comma) {
            let before = self.cursor.index();
            let ty = if self.cursor.eat_keyword(Keyword::Default) {
                None
            } else {
                Some(self.type_name())
            };
            self.expect_punct(Punct::Colon);
            let value = self.assign_expr();
            assocs.push(GenericAssoc { ty, value });
            if self.cursor.index() == before {
                break;
            }
        }
        self.expect_punct(Punct::RParen);
        let assocs = self.ast.add_generic_list(&assocs);
        let span = self.span_from(start);
        self.add_expr(Expr::Generic { control, assocs }, span)
    }

    /// `__builtin_offsetof(type, member.path[3])`.
    ///
    /// The path is not an expression. The members in it are looked up in the type rather than in
    /// any scope, so they are kept as designators, which is the same shape a designated
    /// initializer uses and for the same reason.
    fn builtin_offsetof(&mut self) -> ExprId {
        let start = self.cursor.span();
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return self.poison_expr(start);
        }
        let ty = self.type_name();
        self.expect_punct(Punct::Comma);
        let mut path = Vec::new();
        if let Some((name, _)) = self.expect_ident() {
            path.push(Designator::Field(name));
        }
        loop {
            if self.cursor.eat_punct(Punct::Dot) {
                match self.expect_ident() {
                    Some((name, _)) => path.push(Designator::Field(name)),
                    None => break,
                }
            } else if self.cursor.at_punct(Punct::LBracket) {
                self.cursor.bump();
                let index = self.expr();
                self.expect_punct(Punct::RBracket);
                path.push(Designator::Index(index));
            } else {
                break;
            }
        }
        self.expect_punct(Punct::RParen);
        let path = self.ast.add_designator_list(&path);
        let span = self.span_from(start);
        self.add_expr(Expr::Offsetof { ty, path }, span)
    }

    /// `__builtin_choose_expr(cond, then, otherwise)`.
    fn builtin_choose_expr(&mut self) -> ExprId {
        let start = self.cursor.span();
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return self.poison_expr(start);
        }
        let cond = self.assign_expr();
        self.expect_punct(Punct::Comma);
        let then = self.assign_expr();
        self.expect_punct(Punct::Comma);
        let otherwise = self.assign_expr();
        self.expect_punct(Punct::RParen);
        let span = self.span_from(start);
        self.add_expr(Expr::ChooseExpr { cond, then, otherwise }, span)
    }

    /// `__builtin_types_compatible_p(a, b)`.
    fn builtin_types_compatible(&mut self) -> ExprId {
        let start = self.cursor.span();
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return self.poison_expr(start);
        }
        let a = self.type_name();
        self.expect_punct(Punct::Comma);
        let b = self.type_name();
        self.expect_punct(Punct::RParen);
        let span = self.span_from(start);
        self.add_expr(Expr::TypesCompatible { a, b }, span)
    }

    /// `__builtin_va_arg(list, type)`.
    fn builtin_va_arg(&mut self) -> ExprId {
        let start = self.cursor.span();
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return self.poison_expr(start);
        }
        let list = self.assign_expr();
        self.expect_punct(Punct::Comma);
        let ty = self.type_name();
        self.expect_punct(Punct::RParen);
        let span = self.span_from(start);
        self.add_expr(Expr::VaArg { list, ty }, span)
    }

    /// `__builtin_va_start(list, last)`, which takes two arguments in every dialect.
    ///
    /// C23 made the second argument of the `va_start` macro optional, not the second argument of
    /// the builtin: gcc's own C23 header expands `va_start(ap, ...)` to `__builtin_va_start(ap,
    /// 0)`, so the builtin still gets two. The second one is parsed as optional all the same, so
    /// that a program that leaves it out is told what gcc tells it, that the call has too few
    /// arguments, rather than being told something about a parenthesis.
    fn builtin_va_start(&mut self) -> ExprId {
        let start = self.cursor.span();
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return self.poison_expr(start);
        }
        let list = self.assign_expr();
        let mut last = None;
        if self.cursor.eat_punct(Punct::Comma) {
            last = Some(self.assign_expr());
        }
        self.expect_punct(Punct::RParen);
        let span = self.span_from(start);
        self.add_expr(Expr::VaStart { list, last }, span)
    }

    /// `__builtin_va_end(list)`.
    fn builtin_va_end(&mut self) -> ExprId {
        let start = self.cursor.span();
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return self.poison_expr(start);
        }
        let list = self.assign_expr();
        self.expect_punct(Punct::RParen);
        let span = self.span_from(start);
        self.add_expr(Expr::VaEnd { list }, span)
    }

    /// `__builtin_va_copy(dst, src)`.
    fn builtin_va_copy(&mut self) -> ExprId {
        let start = self.cursor.span();
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return self.poison_expr(start);
        }
        let dst = self.assign_expr();
        self.expect_punct(Punct::Comma);
        let src = self.assign_expr();
        self.expect_punct(Punct::RParen);
        let span = self.span_from(start);
        self.add_expr(Expr::VaCopy { dst, src }, span)
    }
}
