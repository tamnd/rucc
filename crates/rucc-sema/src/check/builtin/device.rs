//! `__builtin_acc_on_device`, which says whether the code runs on an OpenACC device type.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! gcc folds the call to a comparison. In a compiler that does not offload, which is every gcc a
//! BSD builds its world with and is also rucc, the code always runs on the host. So the answer is
//! whether the argument is `acc_device_none` or `acc_device_host`, which are 0 and 2 in libgomp's
//! `gomp-constants.h`. libgomp's `oacc-init.c` is the program that writes the call: its
//! `acc_on_device` is the builtin and nothing else.
//!
//! The two values differ only in the bit of 2, so the comparison is `(dev & ~2) == 0`. That reads
//! the argument once, as gcc does, and a call written with a side effect has it once.

use rucc_ast::BinaryOp;
use rucc_base::Symbol;
use rucc_diag::Span;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::tast::Const;

/// The name, which is the whole of what this recognises.
const NAME: &str = "__builtin_acc_on_device";

/// `GOMP_DEVICE_HOST` in libgomp's `gomp-constants.h`. `GOMP_DEVICE_NONE` is 0.
const HOST: i128 = 2;

impl Checker<'_> {
    /// The comparison a call to `__builtin_acc_on_device` is, if the call is one.
    ///
    /// Taken after the call has been checked, so the argument has already been counted and
    /// converted to `int` by the prototype the row in `features.toml` gives it.
    pub(in crate::check) fn acc_on_device_builtin(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        if self.text(function?) != NAME {
            return None;
        }
        let &operand = args.first()?;
        if self.is_poisoned(operand) {
            return Some(self.poison(span));
        }
        let int = self.int();
        let mask = self.constant(Const::Int(!HOST), int, span);
        let node = ExprKind::Binary { op: BinaryOp::BitAnd, lhs: operand, rhs: mask };
        let rest = self.tast.expr(Expr::new(node, int, Category::Rvalue), span);
        let zero = self.constant(Const::Int(0), int, span);
        let node = ExprKind::Binary { op: BinaryOp::Eq, lhs: rest, rhs: zero };
        Some(self.tast.expr(Expr::new(node, int, Category::Rvalue), span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The prototype the argument is converted by comes from the row, so the row has to say what
    /// gcc says the builtin takes and gives.
    #[test]
    fn acc_on_device_is_a_row_of_the_table_taking_and_giving_int() {
        let feature = rucc_gnu::lookup(Kind::Builtin, NAME).expect("a row");
        assert_eq!(feature.status, Status::Implemented);
        assert_eq!(feature.signature, "int(int)");
        assert!(feature.library.is_empty(), "{NAME} is not a call to anything");
    }
}
