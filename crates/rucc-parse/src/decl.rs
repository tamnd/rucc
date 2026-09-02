//! Declarations, definitions, and the translation unit they make up.
//!
//! Design: `spec/06-lexer-and-parser.md` sections 6.5 and 6.6.
//!
//! # Where a definition is told from a declaration
//!
//! The grammar says an external declaration is a function definition or a declaration, and the
//! two share a prefix that can be arbitrarily long: `static inline int (*f(int a, int b))[4]` is
//! the start of both. So there is no lookahead that decides it, and the prefix is parsed once and
//! the decision is made after the first declarator, on one token. A `{` is a body. A declaration
//! specifier is the parameter declarations of an old-style definition, which is the only other
//! thing that can follow a declarator. Anything else, including an attribute or an `asm` label,
//! belongs to the declaration.
//!
//! # Where a name starts existing
//!
//! At the end of its declarator and before its initializer, which is what makes
//! `int x = sizeof(x);` legal and `typedef int T; T T;` mean what C says it means. A function
//! definition declares its own name before its body for the same reason, so that the body can
//! call it.

use rucc_ast::{
    AttrList, Decl, DeclId, DeclSpecs, DeclSpecsId, DeclaratorId, Derived, InitDeclarator,
    InitDeclaratorList, ParamKind, StrId,
};
use rucc_diag::Span;
use rucc_lex::{Keyword, Punct};
use rucc_session::Std;

use crate::parser::Parser;
use crate::recover::skip_past_declaration;
use crate::scope::IdentKind;

impl Parser<'_> {
    /// Every external declaration of the translation unit.
    pub(crate) fn translation_unit(&mut self) {
        while !self.cursor.is_eof() && !self.stopped() {
            let before = self.cursor.index();
            let decl = self.external_declaration();
            self.ast.add_top_level(decl);
            if self.cursor.index() == before {
                // The token starts nothing at all and the error is already reported, so this is
                // what keeps the loop from meeting it again.
                self.cursor.bump();
            }
        }
    }

    /// One declaration or definition at file scope.
    fn external_declaration(&mut self) -> DeclId {
        let start = self.cursor.span();
        if self.cursor.eat_punct(Punct::Semi) {
            // Every real project has one of these, usually after a macro that ends in a
            // semicolon of its own. C23 allows it and everything before it warned.
            self.pedantic("E0414", "extra `;` outside of a function", start);
            let specs = self.ast.add_specs(DeclSpecs::empty(start));
            let declarators = InitDeclaratorList::EMPTY;
            return self.add_decl(Decl::Var { specs, declarators }, start);
        }
        let leading = self.leading_attributes();
        self.declaration(leading, start)
    }

    /// One declaration, which may turn out to be a function definition.
    ///
    /// The same production serves file scope and block scope. A definition inside a block is
    /// GNU's nested function, which parses like any other definition and which semantic analysis
    /// decides about.
    pub(crate) fn declaration(&mut self, leading: AttrList, start: Span) -> DeclId {
        if self.cursor.at_keyword(Keyword::StaticAssert) {
            let (cond, message) = self.static_assert_body();
            self.expect_punct(Punct::Semi);
            let span = self.span_from(start);
            return self.add_decl(Decl::StaticAssert { cond, message }, span);
        }
        if self.cursor.at_keyword(Keyword::Asm) {
            let asm = self.asm_body(start);
            self.expect_punct(Punct::Semi);
            let span = self.span_from(start);
            return match asm {
                Some(asm) => self.add_decl(Decl::Asm(asm), span),
                None => self.poison_decl(span),
            };
        }
        if !leading.is_empty() && self.cursor.eat_punct(Punct::Semi) {
            let span = self.span_from(start);
            return self.add_decl(Decl::Attributes(leading), span);
        }

        let specs = self.decl_specs_with(leading, start);
        if self.cursor.eat_punct(Punct::Semi) {
            // `struct S { int x; };` declares a tag and nothing else, which is a declaration with
            // no declarators rather than a mistake.
            let span = self.span_from(start);
            let declarators = InitDeclaratorList::EMPTY;
            return self.add_decl(Decl::Var { specs, declarators }, span);
        }

        let mut at = self.cursor.span();
        let mut declarator = self.declarator();
        if self.at_definition() {
            return self.function_definition(specs, declarator, start);
        }

        let mut items = Vec::new();
        loop {
            let item = self.init_declarator(specs, declarator, at);
            items.push(item);
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
            at = self.cursor.span();
            let before = self.cursor.index();
            declarator = self.declarator();
            if self.cursor.index() == before {
                break;
            }
        }
        if !self.expect_punct(Punct::Semi) {
            skip_past_declaration(&mut self.cursor);
        }
        let declarators = self.ast.add_init_declarator_list(&items);
        let span = self.span_from(start);
        self.add_decl(Decl::Var { specs, declarators }, span)
    }

    /// One declarator of a declaration, with the assembler name, the attributes and the
    /// initializer that may follow it.
    fn init_declarator(
        &mut self,
        specs: DeclSpecsId,
        declarator: DeclaratorId,
        at: Span,
    ) -> InitDeclarator {
        let asm_label = if self.cursor.at_keyword(Keyword::Asm) { self.asm_label() } else { None };
        let attrs = self.attributes();
        self.declare_name(specs, declarator);
        let init = if self.cursor.eat_punct(Punct::Eq) {
            // A declaration that deduces its type takes an expression and not a braced list,
            // since there is no object yet for a list to be laid out in. gcc and clang both
            // stop here rather than in the checking, and the message a reader gets is the one
            // about the expression that was expected.
            Some(if self.ast[specs].deduces().is_some() {
                self.assign_init()
            } else {
                self.initializer()
            })
        } else {
            None
        };
        let span = self.span_from(at);
        InitDeclarator { declarator, init, asm_label, attrs, span }
    }

    /// The `asm("name")` that gives a declaration an assembler name of its own.
    ///
    /// The C library headers are full of these and the kernel uses them to rename symbols, so
    /// this is not an obscure corner. It is not an `asm` statement and has no operands.
    fn asm_label(&mut self) -> Option<StrId> {
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return None;
        }
        let name = self.string_literal();
        self.expect_punct(Punct::RParen);
        name
    }

    /// Puts a declared name into the scope, as a type name when the declaration is a `typedef`.
    fn declare_name(&mut self, specs: DeclSpecsId, declarator: DeclaratorId) {
        let Some(name) = self.ast[declarator].name else { return };
        let kind =
            if self.ast[specs].is_typedef() { IdentKind::Typedef } else { IdentKind::Ordinary };
        self.scopes.declare(name, kind);
    }

    /// Whether what follows the first declarator is a function body rather than more of the
    /// declaration.
    ///
    /// A specifier here is an old-style definition's parameter declarations. An attribute is
    /// not, and it has to be excluded by name: `__attribute__` is a specifier keyword
    /// everywhere else in the grammar, so the `x` in `int x __attribute__((weak));` would
    /// otherwise look like it was followed by one.
    fn at_definition(&self) -> bool {
        if self.cursor.at_keyword(Keyword::Attribute) {
            return false;
        }
        self.cursor.at_punct(Punct::LBrace) || self.at_decl_specs()
    }

    /// A function definition, from the parameter declarations of an old-style one to the body.
    fn function_definition(
        &mut self,
        specs: DeclSpecsId,
        declarator: DeclaratorId,
        start: Span,
    ) -> DeclId {
        self.declare_name(specs, declarator);
        // The parameters live in a scope outside the body's, so that a declaration in the body
        // shadows a parameter rather than colliding with it.
        self.scopes.push();
        self.declare_params(declarator);
        let params = self.old_style_params();
        if self.cx.std >= Std::C23 && (!params.is_empty() || self.has_identifier_list(declarator)) {
            let message = match self.ast[declarator].name {
                Some(name) => {
                    format!(
                        "`{}` is defined in the old style, which C23 removed",
                        self.spelling(name)
                    )
                }
                None => "this is an old-style definition, which C23 removed".to_string(),
            };
            self.error("E0412", message, start);
        }
        let body = if self.cursor.at_punct(Punct::LBrace) {
            self.compound_stmt()
        } else {
            let found = self.describe(self.cursor.current());
            let at = self.cursor.span();
            self.error("E0412", format!("expected a function body, found {found}"), at);
            skip_past_declaration(&mut self.cursor);
            self.poison_stmt(at)
        };
        self.scopes.pop();
        let params = self.ast.add_decl_list(&params);
        let span = self.span_from(start);
        self.add_decl(Decl::Function { specs, declarator, params, body }, span)
    }

    /// Whether a definition's parameter list is a list of bare identifiers.
    fn has_identifier_list(&self, declarator: DeclaratorId) -> bool {
        let derived = self.ast[declarator].derived;
        match self.ast[derived].first() {
            Some(Derived::Function { kind, .. }) => *kind == ParamKind::Identifiers,
            _ => false,
        }
    }

    /// The declarations between an old-style parameter list and the body.
    ///
    /// They are parsed in the scope the parameters were just declared in, because that is where
    /// their names are and because a `struct` declared here is visible in the body.
    fn old_style_params(&mut self) -> Vec<DeclId> {
        let mut params = Vec::new();
        while self.at_decl_specs() && !self.stopped() {
            let at = self.cursor.span();
            let before = self.cursor.index();
            let decl = self.declaration(AttrList::EMPTY, at);
            params.push(decl);
            if self.cursor.index() == before {
                break;
            }
        }
        params
    }
}
