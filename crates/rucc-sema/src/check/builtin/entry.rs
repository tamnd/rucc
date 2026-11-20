//! `__builtin_sponentry`, which is where the stack pointer was when the running function was
//! entered.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! Clang has it on AArch64 and on nothing else, and the program that writes it is mingw-w64. Its
//! `setjmp` on ARM64 Windows is a macro that passes this as the frame to `_setjmp`, so that a
//! `longjmp` through the Windows unwinder knows which call it is going back to. Without it none of
//! the mingw-w64 headers that include `setjmp.h` compile for that target.
//!
//! The answer is one instruction. The caller's outgoing arguments start at the stack pointer it
//! called with, so the address is the one the first argument passed on the stack would have, and
//! the backend already knows how to finish that address once the frame is laid out.
//!
//! # `__builtin_stack_address`
//!
//! gcc 14 added it, and libgcc's `strub.c` writes it. It answers where the stack pointer is at the
//! call, on every target. That is the move out of the stack pointer a variable length array saves
//! before it takes its bytes, so the node lowers to the same instruction.
//!
//! # Why this is answered after the call is checked
//!
//! The same reason `check/builtin/thread.rs` is: the rows in the table carry `void *(void)`, and
//! the prototype is what reports a call with arguments and gives the expression its type.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::TypeId;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The name, which is the whole of what this recognises.
const NAME: &str = "__builtin_sponentry";

/// The code for asking for it on a target that does not have it.
const CODE: &str = "E0748";

/// gcc's name for the stack pointer where the call is.
const STACK: &str = "__builtin_stack_address";

impl Checker<'_> {
    /// The node a call to `__builtin_sponentry` becomes, if the name is that one.
    ///
    /// Anywhere but AArch64 the call is refused, as clang refuses it, since nothing written for
    /// another machine has a reason to ask.
    pub(in crate::check) fn sponentry_builtin(
        &mut self,
        function: Option<Symbol>,
        ret: TypeId,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        if self.text(name) != NAME {
            return None;
        }
        if self.cx.target.tuple.arch().as_str() != "aarch64" {
            let what = format!("`{NAME}` is only available on AArch64");
            let note = "it is there for the ARM64 Windows `setjmp`, and no other target has it";
            self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
            return Some(self.poison(span));
        }
        Some(self.tast.expr(Expr::new(ExprKind::SpEntry, ret, Category::Rvalue), span))
    }

    /// The node a call to `__builtin_stack_address` becomes, if the name is that one.
    pub(in crate::check) fn stack_address_builtin(
        &mut self,
        function: Option<Symbol>,
        ret: TypeId,
        span: Span,
    ) -> Option<ExprId> {
        if self.text(function?) != STACK {
            return None;
        }
        Some(self.tast.expr(Expr::new(ExprKind::StackAddress, ret, Category::Rvalue), span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The name has to be an implemented row of the table with a signature, for the reasons given
    /// next door in `thread.rs`.
    #[test]
    fn the_name_is_a_row_of_the_table_that_carries_a_signature() {
        let Some(feature) = rucc_gnu::lookup(Kind::Builtin, NAME) else {
            panic!("{NAME} is answered here and is not in features.toml");
        };
        assert_eq!(feature.status, Status::Implemented);
        assert_eq!(feature.signature, "void *(void)");
        assert!(feature.library.is_empty(), "{NAME} is not a call to anything");
    }

    /// The same for gcc's name for the stack pointer, which every target has.
    #[test]
    fn the_stack_address_is_a_row_of_the_table_that_carries_a_signature() {
        let Some(feature) = rucc_gnu::lookup(Kind::Builtin, STACK) else {
            panic!("{STACK} is answered here and is not in features.toml");
        };
        assert_eq!(feature.status, Status::Implemented);
        assert_eq!(feature.signature, "void *(void)");
        assert!(feature.library.is_empty(), "{STACK} is not a call to anything");
        assert!(feature.targets.is_empty(), "{STACK} is on every target");
    }
}
