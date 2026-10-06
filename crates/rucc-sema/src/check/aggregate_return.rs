//! `__attribute__((callee_pop_aggregate_return(n)))`, gcc's word on 32-bit x86 for who takes the
//! address a returned structure goes back through off the stack: the callee, with a `ret $4`,
//! for one, and the caller for zero.
//!
//! The two 32-bit conventions disagree. i386 System V has the callee pop it and 32-bit Windows has
//! the caller, and a function built against the other one's headers says which it follows with
//! this. gcc reads it off the function type, so it lands where a calling convention does, and it
//! is [`FunctionType::return_pointer_popped`] here.

use rucc_ast::{AttrArg, AttrList, Attribute};
use rucc_diag::Diagnostic;
use rucc_types::{FunctionType, TypeId, TypeKind};

use crate::check::Checker;
use crate::scope::Binding;

impl Checker<'_> {
    /// The type with its function, or the function it points at, given whom the list says pops
    /// the address, and the type as written where the list says nothing gcc keeps.
    ///
    /// Each one written is held to gcc's rules in turn: one argument, which is an error otherwise,
    /// then a function type, then a 32-bit target, then an integer constant that is zero or one,
    /// each of the last four a warning that drops it. The last one kept is the answer, since gcc
    /// puts each in front of the type's list and reads the first.
    ///
    /// `pointee` is a type a pointer's own list landed on, which is already the function and goes
    /// no further down.
    pub(in crate::check) fn popping(
        &mut self,
        ty: TypeId,
        attrs: AttrList,
        pointee: bool,
    ) -> TypeId {
        if !matches!(self.cx.target.tuple.arch().as_str(), "x86_64" | "i686") {
            return ty;
        }
        let ast = self.ast;
        let canonical = self.types.canonical(ty);
        let function = match self.types.kind(canonical) {
            TypeKind::Function(function) => Some((function, false)),
            TypeKind::Pointer(to) if !pointee => match self.types.kind(self.types.canonical(to)) {
                TypeKind::Function(function) => Some((function, true)),
                _ => None,
            },
            _ => None,
        };
        let mut asked = None;
        for &attr in &ast[attrs] {
            if self.gnu_name(&attr) != "callee_pop_aggregate_return" {
                continue;
            }
            let args = &ast[attr.args];
            if args.len() != 1 {
                let what = "wrong number of arguments specified for \
                            'callee_pop_aggregate_return' attribute";
                let refused = Diagnostic::error(what, attr.span).with_code("E0840");
                let note = format!("expected 1, found {}", args.len());
                self.report(refused.note(note, attr.span));
                continue;
            }
            if function.is_none() {
                self.not_a_function("callee_pop_aggregate_return", attr.span);
                continue;
            }
            if self.cx.target.tuple.arch().as_str() == "x86_64" {
                let what = "'callee_pop_aggregate_return' attribute only available for 32-bit";
                self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                continue;
            }
            if let Some(pops) = self.pops_argument(attr, args[0]) {
                asked = Some(pops);
            }
        }
        let (Some(pops), Some((function, through))) = (asked, function) else { return ty };
        let current = self.types.signature(function);
        if current.return_pointer_popped == Some(pops) {
            return ty;
        }
        let signature = FunctionType { return_pointer_popped: Some(pops), ..current.clone() };
        let popping = self.types.function(signature);
        if !through {
            return popping;
        }
        let quals = self.types.quals(canonical);
        let pointer = self.types.pointer(popping);
        self.types.qualified(pointer, quals)
    }

    /// Whether the argument says the callee pops, or nothing where gcc warns and drops the
    /// attribute: an argument that is not an integer constant, and one that is neither zero nor
    /// one.
    ///
    /// A lone name stays a name in the parser, so it is looked up here. An enumerator is its
    /// value, and a name nothing declared is gcc's error about it before the warning.
    fn pops_argument(&mut self, attr: Attribute, arg: AttrArg) -> Option<bool> {
        let number = match arg {
            AttrArg::Expr(expr) => {
                let value = self.expr(expr);
                let ty = self.types.canonical(self.tast[value].ty);
                if rucc_types::is_integer(&self.types, ty) {
                    self.eval_integer(value).ok()
                } else {
                    None
                }
            }
            AttrArg::Ident(name) => match self.scopes.lookup(name) {
                Some(Binding::Enumerator { value, .. }) => Some(value),
                Some(_) => None,
                None => {
                    let spelled = self.text(name);
                    let what = if self.scopes.at_file_scope() {
                        format!("'{spelled}' undeclared here (not in a function)")
                    } else {
                        format!("'{spelled}' undeclared (first use in this function)")
                    };
                    self.report(Diagnostic::error(what, attr.span).with_code("E0500"));
                    None
                }
            },
        };
        let what = match number {
            Some(0) => return Some(false),
            Some(1) => return Some(true),
            Some(_) => {
                "argument to 'callee_pop_aggregate_return' attribute is neither zero, nor one"
            }
            None => "'callee_pop_aggregate_return' attribute requires an integer constant argument",
        };
        self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
        None
    }
}
