//! `__builtin_unreachable`, which is the program promising control does not get here.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The call has no value and no arguments, and what it means is a fact about the path it stands on
//! rather than anything to compute. So it becomes [`ExprKind::Unreachable`], a node with nothing
//! under it, and the lowering writes `unreachable_hint` where it stood. Nothing reads that yet: a
//! promise pays when a pass believes it and deletes the code after it, and there is no such pass.
//! Which is why honouring this costs nothing and refusing it cost a great deal.
//!
//! # Why the block does not end here
//!
//! gcc treats the call as the end of a path and everything after it as dead. That is an
//! optimization and not the meaning, and doing it in the front end would be doing it in the one
//! place that cannot check whether it was right. The promise is undefined behaviour when it turns
//! out false, so a compiler may do anything at all with the code below, and continuing to translate
//! it is one of the things it may do. It is also the one that keeps a program built at `-O0`
//! behaving the way its author watched it behave.
//!
//! What that gives up is the one thing the builtin is usually written for: a `switch` covering
//! every value of an enumeration, where the default arm is `__builtin_unreachable()` and the
//! function has no return after it. Here that function still runs off the bottom, which the walk
//! already ends with the `unreachable` terminator, so the two arrive at the same instruction from
//! opposite directions and the program is right either way.
//!
//! # Why this is answered after the call is checked
//!
//! The same reason `check/builtin/expect.rs` is. The row carries `void(void)`, so the call has a
//! prototype, and it is the prototype that reports `__builtin_unreachable(1)` in the ordinary
//! words. Recognising the name before the callee is looked up would mean writing that message
//! again here.

use rucc_base::Symbol;
use rucc_diag::Span;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The name, which is the whole of what this recognises.
const NAME: &str = "__builtin_unreachable";

impl Checker<'_> {
    /// The node a call to `__builtin_unreachable` becomes, if the name is that one.
    ///
    /// Answers nothing for every other call in the program, so the test that costs a byte goes
    /// first, and the name decides it rather than the declaration for the reason it does next
    /// door: the reserved prefix is what says the name belongs to the implementation.
    pub(in crate::check) fn unreachable_builtin(
        &mut self,
        function: Option<Symbol>,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_") || spelled != NAME {
            return None;
        }
        let ty = self.types.void();
        Some(self.tast.expr(Expr::new(ExprKind::Unreachable, ty, Category::Rvalue), span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The name has to be a row of the table with a signature, because the signature is what the
    /// call is checked against before this replaces it, and it has to be implemented, because that
    /// is what stops the lowering refusing the call before it ever gets here.
    #[test]
    fn the_name_is_a_row_of_the_table_that_carries_a_signature() {
        let Some(feature) = rucc_gnu::lookup(Kind::Builtin, NAME) else {
            panic!("{NAME} is answered here and is not in features.toml");
        };
        assert_eq!(feature.status, Status::Implemented);
        assert_eq!(feature.signature, "void(void)");
        assert!(feature.library.is_empty(), "{NAME} is not a call to anything");
    }
}
