//! `__assume`, `__noop` and `__debugbreak`, three of the intrinsics MSVC has without anybody
//! declaring them.
//!
//! Design: `platform/windows/11-headers-and-dialect.md` section 11.3.
//!
//! All three are names on the MSVC rows only. On a mingw row they are whatever the program or the
//! mingw headers say they are, which for `__debugbreak` is an inline function in `intrin.h` and for
//! the other two is nothing, and that has to keep working the way it does today.
//!
//! # Why the arguments are checked and then dropped
//!
//! `__assume(x)` and `__noop(...)` both promise not to evaluate what they are given, and both are
//! still written in the program: a name in there that nothing declares is a mistake MSVC reports.
//! So the arguments are checked like any other expression and the nodes that come back are left
//! out of the answer, which is what `__builtin_constant_p` next door does for the same reason.
//!
//! # `__assume(0)`
//!
//! The one argument that says something the code generator can use without an optimizer that
//! reads assumptions. It is how MSVC code marks a `default:` that cannot happen, and it means what
//! `__builtin_unreachable()` means, so that is what it becomes. Any other argument is dropped,
//! which is what an optimizer that ignores the hint would do with it anyway.
//!
//! # `__debugbreak`
//!
//! The breakpoint instruction, which on x86-64 is `int3` and on arm64 is `brk #0xf000`, the
//! immediate being the one Windows' debuggers look for. It is written as an `asm volatile` rather
//! than an instruction of its own, since an `asm` statement is already a thing every stage after
//! this one carries through untouched. The x86-64 one is spelled as its byte, `0xcc`, because the
//! template reader takes `.byte` and has no row for `int3`.

use rucc_ast::{self as ast, AsmQuals};
use rucc_base::{IdxRange, Symbol};
use rucc_diag::{Diagnostic, Span};
use rucc_lex::{Encoding, Remarks, StringLiteral};

use crate::asm::Asm;
use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::stmt::Stmt;
use crate::tast::Const;

impl Checker<'_> {
    /// The node a call to one of the three becomes, if the name is one of them and the target is
    /// an MSVC row.
    pub(in crate::check) fn msvc_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        if self.cx.target.tuple.env().as_str() != "msvc" {
            return None;
        }
        let spelled = self.text(name);
        match spelled {
            "__noop" => {
                self.unevaluated_args(args);
                let int = self.int();
                Some(self.constant(Const::Int(0), int, span))
            }
            "__assume" => Some(self.msvc_assume(args, span)),
            "__debugbreak" => Some(self.msvc_debugbreak(args, span)),
            _ => None,
        }
    }

    /// Checks each argument and keeps none of them.
    fn unevaluated_args(&mut self, args: ast::ExprList) {
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        for arg in written {
            let arg = self.expr(arg);
            let _ = self.value(arg);
        }
    }

    /// `__assume(x)`, which is `__builtin_unreachable()` when `x` is zero and nothing otherwise.
    fn msvc_assume(&mut self, args: ast::ExprList, span: Span) -> ExprId {
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        if written.len() != 1 {
            return self.wrong_arity("__assume", written.is_empty(), span);
        }
        let arg = self.expr(written[0]);
        let arg = self.condition(arg, span);
        if self.is_poisoned(arg) {
            return arg;
        }
        let void = self.types.void();
        let mut eval = self.eval();
        let folded = eval.constant(arg);
        let _ = eval.finish();
        if matches!(folded, Ok(Const::Int(0))) {
            return self.tast.expr(Expr::new(ExprKind::Unreachable, void, Category::Rvalue), span);
        }
        self.no_op(span)
    }

    /// `__debugbreak()`, the breakpoint instruction as an `asm volatile`.
    fn msvc_debugbreak(&mut self, args: ast::ExprList, span: Span) -> ExprId {
        if !self.ast[args].is_empty() {
            self.unevaluated_args(args);
            return self.wrong_arity("__debugbreak", false, span);
        }
        let aarch64 = matches!(self.cx.target.tuple.arch().as_str(), "aarch64" | "arm64ec");
        let text = if aarch64 { "brk #0xf000" } else { ".byte 0xcc" };
        let elements = text.chars().map(u32::from).collect();
        let literal =
            StringLiteral { elements, encoding: Encoding::Plain, remarks: Remarks::default() };
        let template = self.tast.add_string(literal);
        let asm = Asm {
            template,
            outputs: IdxRange::EMPTY,
            inputs: IdxRange::EMPTY,
            clobbers: IdxRange::EMPTY,
            labels: IdxRange::EMPTY,
            quals: AsmQuals::VOLATILE,
        };
        let asm = self.tast.add_asm(asm);
        let stmt = self.tast.stmt(Stmt::Asm(asm), span);
        let body = self.tast.add_stmt_refs(&[stmt]);
        let block = self.tast.stmt(Stmt::Block(body), span);
        let void = self.types.void();
        self.tast.expr(Expr::new(ExprKind::StmtExpr(block), void, Category::Rvalue), span)
    }

    /// A `void` expression that does nothing, which is an empty statement expression.
    fn no_op(&mut self, span: Span) -> ExprId {
        let body = self.tast.add_stmt_refs(&[]);
        let block = self.tast.stmt(Stmt::Block(body), span);
        let void = self.types.void();
        self.tast.expr(Expr::new(ExprKind::StmtExpr(block), void, Category::Rvalue), span)
    }

    /// The message for a call with the wrong number of arguments, in the words the rest of the
    /// builtins use.
    fn wrong_arity(&mut self, name: &str, few: bool, span: Span) -> ExprId {
        let how = if few { "few" } else { "many" };
        self.report(
            Diagnostic::error(format!("too {how} arguments to function '{name}'"), span)
                .with_code("E0511"),
        );
        self.poison(span)
    }
}
