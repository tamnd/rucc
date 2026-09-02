//! Declarators, which are the part of C's grammar that reads inside out.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.5.
//!
//! # The fold
//!
//! In `int (*f[3])(char)` the name is `f` and the type is read outward from it: array of three,
//! pointer to, function taking `char`, returning `int`. So what comes out of here is a name plus
//! a flat list of steps in exactly that spoken order, which the type system folds onto the
//! specifier type from the end back to the start.
//!
//! The list is built by one rule applied at every parenthesis level:
//!
//! ```text
//! list = inner ++ suffixes ++ reverse(pointers)
//! ```
//!
//! where `inner` is the list from the declarator inside the grouping parentheses, `suffixes` are
//! the `[...]` and `(...)` written after it at this level, and `pointers` are the stars written
//! before it. The reversal is what makes `int * const * p` a pointer to a const pointer rather
//! than the other way round.
//!
//! # Why it is a loop and not a recursion
//!
//! The nesting that a declarator can have is unbounded and it is three lines of generated code
//! to write a thousand levels of it, so the levels are kept in a vector on the heap and capped
//! at [`MAX_DECLARATOR_DEPTH`]. The parameter lists inside a declarator are still recursion,
//! because a parameter holds a whole declaration, and that recursion is bounded by the bracket
//! counter in [`Parser::enter`](crate::parser::Parser).

use rucc_ast::{
    ArraySize, AttrList, Declarator, DeclaratorId, Derived, DerivedList, MAX_DECLARATOR_DEPTH,
    Param, ParamKind, ParamList, Quals, TypeName, TypeNameId,
};
use rucc_lex::{Keyword, Punct};

use crate::parser::Parser;
use crate::scope::IdentKind;

/// Whether the declarator being read is allowed a name, and whether it must have one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeclKind {
    /// A declarator that names something, as in a declaration or a definition.
    Concrete,
    /// A declarator with no name, as in a type name or a `sizeof`.
    Abstract,
    /// Either, which is what a parameter is: `int f(int, int x)` has one of each.
    Either,
}

impl Parser<'_> {
    /// A `declarator`, which names what it declares.
    pub(crate) fn declarator(&mut self) -> DeclaratorId {
        self.any_declarator(DeclKind::Concrete)
    }

    /// A `type-name`, which is a specifier list and an abstract declarator.
    pub(crate) fn type_name(&mut self) -> TypeNameId {
        let start = self.cursor.span();
        let before = self.cursor.index();
        let specs = self.decl_specs();
        if self.cursor.index() == before {
            let found = self.describe(self.cursor.current());
            self.error("E0407", format!("expected a type name, found {found}"), start);
        }
        let declarator = self.any_declarator(DeclKind::Abstract);
        let span = self.span_from(start);
        self.ast.add_type_name(TypeName { specs, declarator, span })
    }

    /// A declarator of any of the three kinds.
    pub(crate) fn any_declarator(&mut self, kind: DeclKind) -> DeclaratorId {
        let start = self.cursor.span();
        // One entry per grouping parenthesis descended into, holding the pointers written
        // before it, which are applied after everything inside it.
        let mut outer: Vec<Vec<Derived>> = Vec::new();
        let mut pointers = self.pointers();
        let mut name = None;
        let mut name_span = self.cursor.prev_end();

        loop {
            if self.at_grouping_paren(kind) {
                if outer.len() >= MAX_DECLARATOR_DEPTH || !self.enter() {
                    self.declarator_too_deep();
                    break;
                }
                self.cursor.bump();
                outer.push(pointers);
                // An attribute may open a grouping parenthesis, and what it appertains to is
                // whatever is declared inside it, so it is read here and carried onto the first
                // pointer written after it. `int (ATTR *)(void)` is the shape that needs this.
                let leading = self.attributes();
                pointers = self.pointers();
                self.attach_leading(leading, &mut pointers);
                continue;
            }
            if let Some(symbol) = self.cursor.current().ident() {
                if kind != DeclKind::Abstract {
                    name = Some(symbol);
                    name_span = self.cursor.span();
                    self.cursor.bump();
                }
            } else if kind == DeclKind::Concrete {
                let found = self.describe(self.cursor.current());
                let at = self.cursor.span();
                self.error("E0401", format!("expected a name, found {found}"), at);
            }
            break;
        }

        let mut derived = Vec::new();
        loop {
            self.declarator_suffixes(&mut derived);
            derived.extend(pointers.iter().rev().copied());
            match outer.pop() {
                Some(enclosing) => {
                    self.expect_punct(Punct::RParen);
                    self.leave();
                    pointers = enclosing;
                }
                None => break,
            }
        }

        let derived = self.ast.add_derived_list(&derived);
        let span = self.span_from(start);
        self.ast.add_declarator(Declarator { name, name_span, derived, span })
    }

    /// Reports the depth cap, once.
    fn declarator_too_deep(&mut self) {
        let at = self.cursor.span();
        let message = format!("declarator nested more deeply than {MAX_DECLARATOR_DEPTH} levels");
        self.error("E0410", message, at);
    }

    /// Whether the `(` here opens a grouping parenthesis rather than a parameter list.
    ///
    /// The two are told apart by one token. A parameter list is empty, starts with `...`, or
    /// starts with a declaration specifier, and a typedef name counts as one, which is the
    /// second place the scope stack decides the parse. Anything else in there is a declarator,
    /// so the parenthesis is grouping: `int (*f)(char)` and `int (f)(char)` both take this
    /// path, and `int (int)` does not.
    fn at_grouping_paren(&mut self, kind: DeclKind) -> bool {
        if !self.cursor.at_punct(Punct::LParen) {
            return false;
        }
        let next = self.cursor.peek(1);
        if next.punct() == Some(Punct::RParen) || next.punct() == Some(Punct::Ellipsis) {
            return false;
        }
        // An attribute is the one token that begins both kinds of thing, since a parameter's
        // declaration can start with one and so can the declarator inside a grouping
        // parenthesis. What decides is the token after the attribute, and an attribute is more
        // tokens than the lookahead bound allows anybody to peek past, so it is stepped over
        // and the cursor put back where it was.
        if self.at_attribute_after_paren() {
            return self.grouping_paren_past_attributes(kind);
        }
        if self.starts_decl_specs(next) {
            return false;
        }
        // An abstract declarator has no name to group, so `(` there is only grouping when
        // something that is not a name follows it, which is a pointer or another parenthesis.
        if kind == DeclKind::Abstract && next.ident().is_some() {
            return false;
        }
        true
    }

    /// Whether an attribute is written just after the `(` at the cursor.
    fn at_attribute_after_paren(&self) -> bool {
        let next = self.cursor.peek(1);
        next.keyword() == Some(Keyword::Attribute)
            || (next.punct() == Some(Punct::LBracket)
                && self.cursor.peek(2).punct() == Some(Punct::LBracket))
    }

    /// Whether the `(` at the cursor is grouping, decided by what follows the attributes in it.
    ///
    /// `int (ATTR *)(void)` is a pointer to a function and `int f(ATTR int x)` is a parameter
    /// list, and the attribute in front tells them apart in neither case. What tells them apart
    /// is the `*`. Nothing is read here, in the sense the cursor means it: no diagnostic is
    /// reported and no node is added between the save and the restore, because a speculative
    /// parse that left either behind would be describing a reading of the source that was
    /// abandoned.
    fn grouping_paren_past_attributes(&mut self, kind: DeclKind) -> bool {
        let mark = self.cursor.save();
        self.cursor.bump();
        self.skip_attributes();
        let next = self.cursor.current();
        let grouping = next.punct() == Some(Punct::Star)
            || next.punct() == Some(Punct::LParen)
            || (kind != DeclKind::Abstract
                && next.ident().is_some()
                && !self.starts_decl_specs(next));
        self.cursor.restore(mark);
        grouping
    }

    /// Steps over the attributes at the cursor without reading them.
    fn skip_attributes(&mut self) {
        loop {
            if self.cursor.at_keyword(Keyword::Attribute) {
                self.cursor.bump();
                self.skip_balanced(Punct::LParen, Punct::RParen);
            } else if self.at_standard_attribute() {
                // Both brackets are counted rather than one stepped over, since the run ends at
                // the `]` that closes the outer one and stopping at the inner one would leave
                // the second `]` where the token that decides the parse should be.
                self.skip_balanced(Punct::LBracket, Punct::RBracket);
            } else {
                return;
            }
        }
    }

    /// Steps over a bracketed run, from the opener at the cursor to the one that closes it.
    ///
    /// Nothing is stepped over when the cursor is not on the opener, which is what a malformed
    /// attribute looks like from here, so a file with one in it is parsed the way it was before
    /// rather than skipped to whichever closer came next.
    fn skip_balanced(&mut self, open: Punct, close: Punct) {
        let mut depth = 0usize;
        while !self.cursor.current().is_eof() {
            if depth == 0 && self.cursor.current().punct() != Some(open) {
                return;
            }
            let punct = self.cursor.bump().punct();
            if punct == Some(open) {
                depth += 1;
            } else if punct == Some(close) {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
        }
    }

    /// Puts the attributes written before a pointer onto it, which is where they appertain.
    ///
    /// They are dropped when no pointer follows, as in `int (ATTR x)`, because a declarator has
    /// nowhere to carry an attribute of its own and the ones that reach here are the function
    /// attributes that gcc ignores in this position too.
    fn attach_leading(&mut self, leading: AttrList, pointers: &mut [Derived]) {
        if self.ast[leading].is_empty() {
            return;
        }
        let Some(&Derived::Pointer { attrs: written, .. }) = pointers.first() else {
            return;
        };
        let mut merged = self.ast[leading].to_vec();
        merged.extend_from_slice(&self.ast[written]);
        let merged = self.ast.add_attr_list(&merged);
        if let Some(Derived::Pointer { attrs, .. }) = pointers.first_mut() {
            *attrs = merged;
        }
    }

    /// The `*` and the qualifiers and attributes written after each of them, in written order.
    fn pointers(&mut self) -> Vec<Derived> {
        let mut out = Vec::new();
        while self.cursor.eat_punct(Punct::Star) {
            let quals = self.pointer_quals();
            let attrs = self.attributes();
            out.push(Derived::Pointer { quals, attrs });
        }
        out
    }

    /// The qualifiers after a `*` or inside an array's brackets.
    fn pointer_quals(&mut self) -> Quals {
        let mut quals = Quals::NONE;
        loop {
            let Some(word) = self.cursor.current().keyword() else { return quals };
            let one = match word {
                Keyword::Const => Quals::CONST,
                Keyword::Volatile => Quals::VOLATILE,
                Keyword::Restrict => Quals::RESTRICT,
                Keyword::Atomic => Quals::ATOMIC,
                _ => return quals,
            };
            self.cursor.bump();
            quals = quals.with(one);
        }
    }

    /// The `[...]` and `(...)` written after a declarator at one level.
    fn declarator_suffixes(&mut self, out: &mut Vec<Derived>) {
        loop {
            if self.cursor.at_punct(Punct::LBracket) {
                if !self.enter() {
                    self.cursor.bump();
                    return;
                }
                self.cursor.bump();
                out.push(self.array_suffix());
                self.expect_punct(Punct::RBracket);
                self.leave();
            } else if self.cursor.at_punct(Punct::LParen) {
                if !self.enter() {
                    self.cursor.bump();
                    return;
                }
                self.cursor.bump();
                out.push(self.parameter_list());
                self.expect_punct(Punct::RParen);
                self.leave();
            } else {
                return;
            }
        }
    }

    /// What is between a declarator's brackets, with the `[` already consumed.
    ///
    /// The `static` and the qualifiers are only legal on a parameter, where they say something
    /// about the pointer the array decays to. They are kept rather than dropped because they
    /// license a diagnostic and can inform alias analysis, per section 6.5.
    fn array_suffix(&mut self) -> Derived {
        let mut quals = Quals::NONE;
        let mut has_static = false;
        loop {
            if self.cursor.eat_keyword(Keyword::Static) {
                has_static = true;
                continue;
            }
            let before = quals;
            quals = quals.with(self.pointer_quals());
            if quals == before {
                break;
            }
        }
        let size = if self.cursor.at_punct(Punct::RBracket) {
            ArraySize::Unspecified
        } else if self.cursor.at_punct(Punct::Star)
            && self.cursor.peek(1).punct() == Some(Punct::RBracket)
        {
            self.cursor.bump();
            ArraySize::Star
        } else {
            ArraySize::Expr(self.assign_expr())
        };
        Derived::Array { size, quals, has_static }
    }

    /// A parameter list, with the `(` already consumed.
    ///
    /// The parameters are declared in a scope of their own, which is what makes
    /// `typedef int T; void f(int T, T x);` an error on the second parameter rather than a
    /// declaration of two. A function definition needs them in the body's scope instead, and
    /// re-declares them there from the list this produced.
    fn parameter_list(&mut self) -> Derived {
        if self.cursor.at_punct(Punct::RParen) {
            return Derived::Function {
                params: ParamList::EMPTY,
                variadic: false,
                kind: ParamKind::Empty,
            };
        }
        if self.cursor.at_keyword(Keyword::Void)
            && self.cursor.peek(1).punct() == Some(Punct::RParen)
        {
            self.cursor.bump();
            return Derived::Function {
                params: ParamList::EMPTY,
                variadic: false,
                kind: ParamKind::Void,
            };
        }
        // An identifier that names no type starts an old-style list. Nothing else can: a
        // prototype's first token is a specifier, and a specifier is never an ordinary
        // identifier.
        if self.cursor.current().ident().is_some() && !self.starts_decl_specs(self.cursor.current())
        {
            return self.identifier_list();
        }
        self.scopes.push();
        let mut params = Vec::new();
        let mut variadic = false;
        loop {
            if self.cursor.at_punct(Punct::RParen) || self.cursor.is_eof() {
                break;
            }
            if self.cursor.eat_punct(Punct::Ellipsis) {
                variadic = true;
                break;
            }
            let before = self.cursor.index();
            params.push(self.parameter());
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
            if self.cursor.index() == before {
                break;
            }
        }
        self.scopes.pop();
        let params = self.ast.add_param_list(&params);
        Derived::Function { params, variadic, kind: ParamKind::Prototype }
    }

    /// One parameter of a prototype.
    fn parameter(&mut self) -> Param {
        let start = self.cursor.span();
        let specs = self.decl_specs();
        let declarator = self.any_declarator(DeclKind::Either);
        if let Some(name) = self.ast[declarator].name {
            self.scopes.declare(name, IdentKind::Ordinary);
        }
        let attrs = self.attributes();
        Param { specs: Some(specs), declarator, attrs, span: self.span_from(start) }
    }

    /// An old-style parameter list, which is bare identifiers whose types come afterwards.
    fn identifier_list(&mut self) -> Derived {
        let mut params = Vec::new();
        loop {
            let start = self.cursor.span();
            let before = self.cursor.index();
            let Some((name, name_span)) = self.expect_ident() else { break };
            let declarator = self.ast.add_declarator(Declarator {
                name: Some(name),
                name_span,
                derived: DerivedList::EMPTY,
                span: name_span,
            });
            params.push(Param {
                specs: None,
                declarator,
                attrs: AttrList::EMPTY,
                span: self.span_from(start),
            });
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
            if self.cursor.index() == before {
                break;
            }
        }
        let params = self.ast.add_param_list(&params);
        Derived::Function { params, variadic: false, kind: ParamKind::Identifiers }
    }

    /// Declares a definition's parameters in the scope its body will be parsed in.
    ///
    /// The parameters of a declaration live in a scope that closes with the declarator, and the
    /// parameters of a definition live in the body. Rather than parse the list differently in
    /// the two cases, the list is parsed once and its names are put back into the block scope
    /// here, which is also where an old-style definition's identifier list gets its names.
    pub(crate) fn declare_params(&mut self, declarator: DeclaratorId) {
        let derived = self.ast[declarator].derived;
        let Some(&first) = self.ast[derived].first() else { return };
        let Derived::Function { params, .. } = first else { return };
        for index in 0..params.len() {
            let param = self.ast[params][index];
            if let Some(name) = self.ast[param.declarator].name {
                self.scopes.declare(name, IdentKind::Ordinary);
            }
        }
    }
}
