//! `__builtin_convertvector`, which converts each lane of a vector to another lane type.
//!
//! Design: `Spec/2131/platform/wasm/19-simd.md`, the section about the header.
//!
//! The call is `__builtin_convertvector(v, T)`, where `v` is a vector and `T` is a vector type
//! with the same number of lanes. Lane `i` of the answer is lane `i` of `v` converted to the lane
//! type of `T`, as a cast does it, and the answer has the type `T`. clang made it first, and gcc 9
//! took it with the same rules. clang's `<wasm_simd128.h>` writes the conversions, the extends and
//! the extending loads with it, and SIMD libraries use it when `__has_builtin` says yes.
//!
//! It is syntax and not a call, because the second argument is a type name. The parser reads it
//! and this checks it.
//!
//! # What clang asks of the operands
//!
//! The rules and the words are those of clang 23, and each error is at the start of the call, as
//! in clang. The first argument is a vector. The second is a vector type. The two have the same
//! number of lanes. The lanes can have different sizes, so the answer can be wider or narrower
//! than the operand.

use rucc_ast as ast;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{is_vector, lanes};

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

impl Checker<'_> {
    /// `__builtin_convertvector(operand, ty)`.
    pub(in crate::check) fn convert_vector(
        &mut self,
        operand: ast::ExprId,
        ty: ast::TypeNameId,
        span: Span,
    ) -> ExprId {
        let operand = self.expr(operand);
        let operand = self.value(operand);
        let ty = self.type_name(ty);
        if self.is_poisoned(operand) {
            return self.poison(span);
        }
        let from = self.tast[operand].ty;
        let wrong = if !is_vector(&self.types, from) {
            Some("first argument to __builtin_convertvector must be a vector")
        } else if !is_vector(&self.types, ty) {
            Some("second argument to __builtin_convertvector must be of vector type")
        } else if lanes(&self.types, from) != lanes(&self.types, ty) {
            Some(
                "first two arguments to __builtin_convertvector must have the same number of \
                 elements",
            )
        } else {
            None
        };
        if let Some(wrong) = wrong {
            self.report(Diagnostic::error(wrong, span).with_code("E0685"));
            return self.poison(span);
        }
        let ty = self.types.unqualified(ty);
        self.tast.expr(Expr::new(ExprKind::ConvertVector { operand }, ty, Category::Rvalue), span)
    }
}
