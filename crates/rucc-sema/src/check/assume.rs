//! `__attribute__((assume(expression)));`, gcc 13's statement that tells the optimizer something
//! is true without asking for it to be computed.
//!
//! The expression is never evaluated, which is the difference between it and an `assert` that
//! has been compiled out: a call in it is not made and an increment in it does not happen. When
//! the optimizer runs and nothing in the expression would do anything, the statement becomes
//! `if (expression) ; else __builtin_unreachable();`, which is the spelling a program wrote for
//! the same promise before gcc had one, and what the optimizer makes of that is what it makes of
//! this. An expression that would do something is checked and dropped, as gcc drops it, and so is
//! every one at `-O0`, where gcc writes nothing for the statement either.
//!
//! What is here besides is gcc 13's checking, in its words: the argument count, the scalar the
//! expression has to be, and the places the attribute is written where it is not a statement.

use rucc_ast::{AttrArg, AttrList, AttrSyntax};
use rucc_diag::{Diagnostic, Span};
use rucc_types::{Qualifiers, is_array};

use crate::check::Checker;
use crate::expr::{Category, Conversion, Expr, ExprId, ExprKind};
use crate::stmt::Stmt;

impl Checker<'_> {
    /// The statement an attribute declaration in a body becomes, which is the promise each
    /// `assume` in it makes when the optimizer is there to read it, and nothing otherwise.
    pub(in crate::check) fn assumed(&mut self, attrs: AttrList) -> Stmt {
        let ast = self.ast;
        let mut promises = Vec::new();
        for &attr in &ast[attrs] {
            if self.gnu_name(&attr) != "assume" {
                continue;
            }
            // `[[assume(...)]]` is C++'s spelling and not C's, and gcc reads it as a name it
            // does not know.
            if attr.namespace.is_none() && attr.syntax != AttrSyntax::Gnu {
                let what = "'assume' attribute ignored";
                self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                continue;
            }
            let args = self.ast[attr.args].to_vec();
            let [AttrArg::Expr(written)] = args[..] else {
                let what = "wrong number of arguments specified for 'assume' attribute";
                let note = format!("expected 1, found {}", args.len());
                self.report(
                    Diagnostic::error(what, attr.span).with_code("E0835").note(note, attr.span),
                );
                continue;
            };
            let value = self.expr(written);
            // An array is a scalar to an `if`, through the pointer it decays to, and not to gcc's
            // `assume`, which asks before the decay.
            if is_array(&self.types, self.types.canonical(self.tast[value].ty)) {
                let what =
                    "used array that cannot be converted to pointer where scalar is required";
                self.report(Diagnostic::error(what, attr.span).with_code("E0510"));
                continue;
            }
            let cond = self.condition(value, attr.span);
            if self.is_poisoned(cond) || !self.cx.optimizing || !self.effectless(cond) {
                continue;
            }
            let at = self.ast.expr_span(written);
            let then = self.tast.stmt(Stmt::Empty, at);
            let void = self.types.void();
            let unreachable =
                self.tast.expr(Expr::new(ExprKind::Unreachable, void, Category::Rvalue), at);
            let otherwise = self.tast.stmt(Stmt::Expr(unreachable), at);
            promises.push((Stmt::If { cond, then, otherwise: Some(otherwise) }, at));
        }
        match promises[..] {
            [] => Stmt::Empty,
            [(one, _)] => one,
            _ => {
                let each: Vec<_> =
                    promises.into_iter().map(|(promise, at)| self.tast.stmt(promise, at)).collect();
                Stmt::Block(self.tast.add_stmt_refs(&each))
            }
        }
    }

    /// gcc's warning for an `assume` on its own outside any function.
    pub(in crate::check) fn assumed_at_top(&mut self, attrs: AttrList) {
        let ast = self.ast;
        for &attr in &ast[attrs] {
            if self.gnu_name(&attr) == "assume" {
                let what = "'assume' attribute at top level";
                self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
            }
        }
    }

    /// gcc's warnings for an `assume` written on a declaration, which is not where it goes. In
    /// front of the declaration it is one that should have had a `;` after it, and anywhere it
    /// is ignored, which gcc says at the start of the declaration.
    pub(in crate::check) fn assume_misplaced(
        &mut self,
        lists: &[AttrList],
        leading: bool,
        at: Span,
    ) {
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if self.gnu_name(&attr) != "assume" {
                    continue;
                }
                if leading {
                    let what = "'assume' attribute not followed by ';'";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
                let what = "'assume' attribute ignored";
                self.report(Diagnostic::warning(what, at).with_code("E0703"));
            }
        }
    }

    /// Whether working out the value does nothing but work it out: no call, no store, no
    /// `volatile` read, nothing that is not an operator on what is already there. Anything this
    /// does not know is taken to do something, which only costs a promise.
    fn effectless(&self, expr: ExprId) -> bool {
        use rucc_ast::UnaryOp;
        match self.tast[expr].kind {
            ExprKind::Const(_) | ExprKind::Str(_) | ExprKind::Decl(_) | ExprKind::LabelAddr(_) => {
                true
            }
            ExprKind::Member { base, .. } => self.effectless(base),
            ExprKind::Subscript { base, index } => self.effectless(base) && self.effectless(index),
            ExprKind::Unary { op, operand } => {
                !matches!(
                    op,
                    UnaryOp::PreInc | UnaryOp::PreDec | UnaryOp::PostInc | UnaryOp::PostDec
                ) && self.effectless(operand)
            }
            ExprKind::Binary { lhs, rhs, .. } | ExprKind::Comma { lhs, rhs } => {
                self.effectless(lhs) && self.effectless(rhs)
            }
            ExprKind::Cond { cond, then, otherwise } => {
                self.effectless(cond) && self.effectless(then) && self.effectless(otherwise)
            }
            ExprKind::Cast(operand) => self.effectless(operand),
            ExprKind::Convert { kind, operand } => {
                let read = self.types.quals(self.types.canonical(self.tast[operand].ty));
                !(kind == Conversion::Lvalue && read.has(Qualifiers::VOLATILE))
                    && self.effectless(operand)
            }
            _ => false,
        }
    }
}
