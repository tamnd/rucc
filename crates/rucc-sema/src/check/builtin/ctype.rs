//! `__builtin_isdigit`, which is a comparison and not a call.
//!
//! C says `isdigit` is true of the ten decimal digits and of nothing else, whatever the locale, and
//! C requires those ten to be consecutive in the execution character set. So unlike the rest of
//! `ctype.h` the answer needs no table: it is whether the argument less `'0'`, taken unsigned, is
//! at most nine. gcc writes exactly that for the prefixed spelling at every optimization level.
//!
//! Linux is why this is here. `include/linux/ctype.h` asks `__has_builtin(__builtin_isdigit)` and
//! defines `isdigit` to it when the answer is yes, and the kernel has no `isdigit` function to call
//! when the answer would have been a call. Saying no instead keeps the kernel building, but every
//! unit that includes the header then preprocesses to different text than it does under gcc, which
//! is the whole of the difference the `kernel-pp` corpus found on x86-64 `defconfig`.
//!
//! Only the prefixed spelling is answered here. The plain `isdigit` stays a call, because a hosted
//! `ctype.h` has a macro of its own for it and a program that reaches the function past that macro
//! has asked for the function.

use rucc_ast::BinaryOp;
use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_types::IntKind;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::tast::Const;

impl Checker<'_> {
    /// The comparison a call to `__builtin_isdigit` is, if the call is one.
    ///
    /// Taken after the call has been checked, so the argument has already been counted and
    /// converted to `int` by the prototype the row in `features.toml` gives it.
    pub(in crate::check) fn ctype_builtin_value(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        if self.text(function?) != "__builtin_isdigit" {
            return None;
        }
        let &operand = args.first()?;
        if self.is_poisoned(operand) {
            return Some(self.poison(span));
        }
        let unsigned = self.types.int(IntKind::UInt);
        let operand = self.conv().to_type(operand, unsigned);
        let zero = self.constant(Const::Int(i128::from(b'0')), unsigned, span);
        let node = ExprKind::Binary { op: BinaryOp::Sub, lhs: operand, rhs: zero };
        let offset = self.tast.expr(Expr::new(node, unsigned, Category::Rvalue), span);
        let nine = self.constant(Const::Int(9), unsigned, span);
        let int = self.int();
        let node = ExprKind::Binary { op: BinaryOp::Le, lhs: offset, rhs: nine };
        Some(self.tast.expr(Expr::new(node, int, Category::Rvalue), span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    /// The prototype the argument is converted by comes from the row, so the row has to say what
    /// the library says `isdigit` takes and gives.
    #[test]
    fn isdigit_is_a_row_of_the_table_taking_and_giving_int() {
        let feature = rucc_gnu::lookup(Kind::Builtin, "__builtin_isdigit").expect("a row");
        assert_eq!(feature.status, Status::Implemented);
        assert_eq!(feature.signature, "int(int)");
    }
}
