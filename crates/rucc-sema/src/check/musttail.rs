//! `[[gnu::musttail]] return f(x);`, gcc 15's tail call that is not an optimization but a promise:
//! the call is made as a jump once this function's frame is given back, at every level of
//! optimization, and where it cannot be the compile stops and says why. An interpreter written
//! as one function per instruction, each ending in a call to the next, is what it is for. It runs
//! in the stack of one call however long the program, and without the jump it runs out of stack.
//!
//! Here is the front end's part, in gcc's words: the attribute goes on a `return` and nowhere
//! else, it takes no arguments, and what is returned has to be a call. The call is then kept in
//! [`crate::tast::Tast::is_must_tail`], and the walk to the IR marks it for the back end, which
//! makes the jump or says in gcc's words why it could not.

use rucc_ast::{AttrList, Attribute};
use rucc_diag::{Diagnostic, Span};

use crate::check::Checker;
use crate::expr::{ExprId, ExprKind};

impl Checker<'_> {
    /// Whether the attributes in front of a `return` ask for a tail call, with what gcc says about
    /// the ones that are wrong.
    ///
    /// gcc takes `musttail` out of the list and says nothing more of anything left in it than that
    /// the two were mixed. A list without it is one this compiler has nowhere to keep, as on any
    /// other statement.
    pub(in crate::check) fn musttail(&mut self, attrs: AttrList, span: Span) -> bool {
        let ast = self.ast;
        let mut asked = false;
        let mut others = false;
        for &attr in &ast[attrs] {
            if !self.is_musttail(&attr) {
                others = true;
                continue;
            }
            asked = true;
            if !ast[attr.args].is_empty() {
                let what = "'musttail' attribute does not take any arguments";
                self.report(Diagnostic::error(what, attr.span).with_code("E0847"));
            }
        }
        if asked && others {
            let what = "attribute 'musttail' mixed with other attributes on 'return' statement";
            self.report(Diagnostic::warning(what, span).with_code("E0703"));
        } else if others {
            self.report(
                Diagnostic::warning("attributes on this statement are ignored", span)
                    .with_code("E0411"),
            );
        }
        asked
    }

    /// Keeps the call a `musttail` return gives back, or says it is not one.
    ///
    /// gcc asks before the value is converted to the return type, so a call whose answer then has
    /// to change is taken here and refused by the back end, which says the value changed after
    /// the call.
    pub(in crate::check) fn must_tail(&mut self, value: Option<ExprId>, span: Span) {
        let call = value.filter(|&value| matches!(self.tast[value].kind, ExprKind::Call { .. }));
        match call {
            Some(call) => self.tast.add_must_tail(call),
            None if value.is_some_and(|value| self.is_poisoned(value)) => {}
            None => {
                let what = "cannot tail-call: return value must be a call";
                self.report(Diagnostic::error(what, span).with_code("E0847"));
            }
        }
    }

    /// gcc's warning for a `musttail` anywhere but in front of a `return`, which is that it is
    /// ignored.
    pub(in crate::check) fn musttail_misplaced(&mut self, lists: &[AttrList]) {
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if self.is_musttail(&attr) {
                    let what = "'musttail' attribute ignored";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
            }
        }
    }

    /// Whether that is `musttail`, in any of gcc's spellings, `clang::musttail` among them.
    fn is_musttail(&self, attr: &Attribute) -> bool {
        if attr.namespace.is_some_and(|ns| self.text(ns) == "clang") {
            return self.gnu_name(&Attribute { namespace: None, ..*attr }) == "musttail";
        }
        self.gnu_name(attr) == "musttail"
    }
}
