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
//!
//! The jump gives the frame back before the callee runs, so a pointer into it that the call is
//! handed points at nothing by then. gcc 15 warns about one passed as an argument, which is
//! `-Wmusttail-local-addr` and on by default, and so does this.

use rucc_ast::{AttrList, Attribute, UnaryOp};
use rucc_diag::{Diagnostic, Span};
use rucc_types::is_pointer;

use crate::check::Checker;
use crate::decl::{DeclId, DeclKind, StorageDuration};
use crate::expr::{Conversion, ExprId, ExprKind};

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
            Some(call) => {
                self.tast.add_must_tail(call);
                self.musttail_local_addr(call);
            }
            None if value.is_some_and(|value| self.is_poisoned(value)) => {}
            None => {
                let what = "cannot tail-call: return value must be a call";
                self.report(Diagnostic::error(what, span).with_code("E0847"));
            }
        }
    }

    /// gcc's `-Wmusttail-local-addr`, once for each argument that is the address of something in
    /// this function's frame.
    ///
    /// gcc looks for it once the call is in its middle end, where an argument is a value or an
    /// address it can work out without running anything, so `&a`, `&s.m`, `&b[1]`, `b` decayed
    /// and any of them cast to another pointer are each one and `&b[i]` is not. Nothing is said
    /// in a function with variable arguments, which gcc refuses the call in before it looks.
    fn musttail_local_addr(&mut self, call: ExprId) {
        if self.in_variadic_function() {
            return;
        }
        let ExprKind::Call { args, .. } = self.tast[call].kind else { return };
        let span = self.tast.expr_span(call);
        let passed: Vec<String> =
            self.tast[args].iter().filter_map(|&arg| self.frame_address(arg)).collect();
        for what in passed {
            let what = format!("address of {what} passed to 'musttail' call argument");
            self.report(Diagnostic::warning(what, span).with_code("E0848"));
        }
    }

    /// What in this function's frame a pointer argument is the address of, in gcc's words.
    fn frame_address(&self, arg: ExprId) -> Option<String> {
        let mut at = arg;
        loop {
            let node = &self.tast[at];
            if !is_pointer(&self.types, node.ty) {
                return None;
            }
            match node.kind {
                ExprKind::Cast(operand)
                | ExprKind::Convert { kind: Conversion::Pointer, operand } => at = operand,
                ExprKind::LabelAddr(_) => return Some("label".to_owned()),
                ExprKind::Unary { op: UnaryOp::AddrOf, operand }
                | ExprKind::Convert { kind: Conversion::ArrayDecay, operand } => {
                    return self.frame_object(operand);
                }
                _ => return None,
            }
        }
    }

    /// The object in this function's frame an lvalue is part of, in gcc's words, where it is one
    /// at an offset known without running anything.
    fn frame_object(&self, lvalue: ExprId) -> Option<String> {
        let mut at = lvalue;
        loop {
            match self.tast[at].kind {
                ExprKind::Member { base, .. } => at = base,
                ExprKind::Subscript { base, index } => {
                    if !matches!(self.tast[index].kind, ExprKind::Const(_)) {
                        return None;
                    }
                    let ExprKind::Convert { kind: Conversion::ArrayDecay, operand } =
                        self.tast[base].kind
                    else {
                        return None;
                    };
                    at = operand;
                }
                ExprKind::Decl(decl) | ExprKind::CompoundLiteral(decl) => {
                    return self.automatic(decl);
                }
                _ => return None,
            }
        }
    }

    /// How gcc names an object of this function's: a parameter, a variable, or a compound
    /// literal, which has no name and is a local variable.
    fn automatic(&self, decl: DeclId) -> Option<String> {
        let object = &self.tast[decl];
        if object.kind != DeclKind::Object || object.duration != StorageDuration::Automatic {
            return None;
        }
        let name = object.name.map(|name| self.text(name));
        Some(match name {
            Some(name) if self.is_parameter(decl) => format!("parameter '{name}'"),
            Some(name) => format!("automatic variable '{name}'"),
            None => "local variable".to_owned(),
        })
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
