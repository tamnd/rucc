//! Initializers, and the designators that say which part of an object they initialize.
//!
//! Design: `spec/06-lexer-and-parser.md` sections 6.6 and 6.7.
//!
//! Nothing here works out which member an item lands on. A braced initializer is recorded as the
//! list that was written, with the designations attached to the items that carried them, and the
//! walk over the object being initialized happens in semantic analysis where the types are
//! known. That is the only place it can happen: whether `{1, 2}` initializes one member or two
//! depends on what the first member's type turns out to be.

use rucc_ast::{Designator, DesignatorList, Init, InitId, InitItem, InitItemList};
use rucc_lex::Punct;

use crate::parser::Parser;

impl Parser<'_> {
    /// An `initializer`, braced or not.
    pub(crate) fn initializer(&mut self) -> InitId {
        if self.cursor.at_punct(Punct::LBrace) {
            return self.braced_init();
        }
        self.assign_init()
    }

    /// An initializer that has to be an expression, which is what a deduced type is deduced from.
    pub(crate) fn assign_init(&mut self) -> InitId {
        let value = self.assign_expr();
        self.ast.add_init(Init::Expr(value))
    }

    /// A `{ ... }` initializer, which C23 also allows to be empty.
    pub(crate) fn braced_init(&mut self) -> InitId {
        if !self.enter() {
            self.cursor.bump();
            return self.ast.add_init(Init::List(InitItemList::EMPTY));
        }
        self.cursor.bump();
        let mut items = Vec::new();
        while !self.cursor.at_punct(Punct::RBrace) && !self.cursor.is_eof() {
            let at = self.cursor.span();
            let before = self.cursor.index();
            let designators = self.designation();
            let init = self.initializer();
            items.push(InitItem { designators, init, span: self.span_from(at) });
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
            if self.cursor.index() == before {
                break;
            }
        }
        self.expect_punct(Punct::RBrace);
        self.leave();
        let items = self.ast.add_init_item_list(&items);
        self.ast.add_init(Init::List(items))
    }

    /// The designation on one item of a braced initializer, if it has one.
    ///
    /// Three spellings. The standard one is a run of designators and an `=`. GNU's range
    /// `[0 ... 9] = x` is one designator covering several elements. GNU's obsolete `field: x`
    /// has no `=` at all and is still in real code, so it is recognised and kept as its own
    /// designator rather than rewritten, which is what lets the printer put it back and a
    /// diagnostic name it.
    fn designation(&mut self) -> DesignatorList {
        if let Some(name) = self.cursor.current().ident() {
            if self.cursor.peek(1).punct() == Some(Punct::Colon) {
                let at = self.cursor.span();
                self.pedantic("E0413", "obsolete designator, write `.field =` instead", at);
                self.cursor.bump();
                self.cursor.bump();
                return self.ast.add_designator_list(&[Designator::ObsoleteField(name)]);
            }
        }
        let mut out = Vec::new();
        loop {
            if self.cursor.eat_punct(Punct::Dot) {
                match self.expect_ident() {
                    Some((name, _)) => out.push(Designator::Field(name)),
                    None => break,
                }
            } else if self.cursor.at_punct(Punct::LBracket) {
                if !self.enter() {
                    self.cursor.bump();
                    break;
                }
                self.cursor.bump();
                let lo = self.const_expr();
                let designator = if self.cursor.eat_punct(Punct::Ellipsis) {
                    Designator::Range { lo, hi: self.const_expr() }
                } else {
                    Designator::Index(lo)
                };
                self.expect_punct(Punct::RBracket);
                self.leave();
                out.push(designator);
            } else {
                break;
            }
        }
        if out.is_empty() {
            return DesignatorList::EMPTY;
        }
        self.expect_punct(Punct::Eq);
        self.ast.add_designator_list(&out)
    }
}
