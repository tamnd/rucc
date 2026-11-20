//! `__builtin_thread_pointer`, which is where the running thread's own storage starts.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! A thread-local variable is at a fixed offset inside a block of storage, and every thread has a
//! block of its own. Nothing in the source ever says where a block is, because the code the
//! compiler writes for a thread-local reads the machine register holding it. This builtin is a
//! program asking for that address on its own account, and the answer is the same register read.
//!
//! What is behind the pointer is not the compiler's to describe. The layout of a thread's block is
//! an agreement between the C library and the loader, so a program that reads through this pointer
//! is reading the library's data structure and is on its own. What it is usually for is the
//! opposite: an allocator that keeps a cache per thread wants a number that is different in every
//! thread and is cheap to come by, and this is that number without the declaration, the relocation
//! and the offset a thread-local of its own would cost. rpmalloc is the program that made this
//! worth doing.
//!
//! # Why this is answered after the call is checked
//!
//! The same reason `check/builtin/unreachable.rs` is. The row carries `void *(void)`, so the call
//! has a prototype, and it is the prototype that reports `__builtin_thread_pointer(1)` in the
//! ordinary words and gives the expression its type. Recognising the name before the callee is
//! looked up would mean writing both of those again here.

use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_types::TypeId;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The name, which is the whole of what this recognises.
const NAME: &str = "__builtin_thread_pointer";

impl Checker<'_> {
    /// The node a call to `__builtin_thread_pointer` becomes, if the name is that one.
    ///
    /// Answers nothing for every other call in the program, so the test that costs a byte goes
    /// first, and the name decides it rather than the declaration for the reason it does next
    /// door: the reserved prefix is what says the name belongs to the implementation.
    ///
    /// The type is taken from the call rather than built here, so that the one place saying what
    /// this answers with is the row in the table the call was checked against.
    pub(in crate::check) fn thread_pointer_builtin(
        &mut self,
        function: Option<Symbol>,
        ret: TypeId,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_") || spelled != NAME {
            return None;
        }
        Some(self.tast.expr(Expr::new(ExprKind::ThreadPointer, ret, Category::Rvalue), span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The name has to be a row of the table with a signature, because the signature is what the
    /// call is checked against before this replaces it and is where the type of the result comes
    /// from, and it has to be implemented, because that is what stops the lowering refusing the
    /// call before it ever gets here.
    #[test]
    fn the_name_is_a_row_of_the_table_that_carries_a_signature() {
        let Some(feature) = rucc_gnu::lookup(Kind::Builtin, NAME) else {
            panic!("{NAME} is answered here and is not in features.toml");
        };
        assert_eq!(feature.status, Status::Implemented);
        assert_eq!(feature.signature, "void *(void)");
        assert!(feature.library.is_empty(), "{NAME} is not a call to anything");
    }
}
