//! `__builtin_trap`, which stops the program where it stands.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The call has no value and no arguments, and what it asks for is one instruction the machine has
//! no meaning for. So it becomes [`ExprKind::Trap`], a node with nothing under it, and the back end
//! writes `ud2` where it stood. A processor reaching that raises the fault for an instruction it
//! does not know, and on Linux the program is ended with `SIGILL`.
//!
//! # Why an instruction and not a call to `abort`
//!
//! It needs no library, which is the whole of the difference where this is written most. A kernel
//! and a freestanding program have no `abort` to call, and both of them write this. It is also two
//! bytes against a call and a relocation, and it leaves the address of the fault in the core file
//! rather than the address inside whatever `abort` does. gcc 16.2.0 writes `ud2` here too.
//!
//! # Why the block does not end here
//!
//! The same answer `check/builtin/unreachable.rs` gives, for a plainer reason. What ends a block in
//! the IR is control going somewhere, and this goes nowhere at all, so there is no terminator to
//! write. The statements after it are lowered the way they would have been without it and the
//! instructions they become are never run, which costs a few bytes nothing reaches and keeps every
//! pass that walks a block from needing a second shape for the block.
//!
//! # Why this is answered after the call is checked
//!
//! The same reason `check/builtin/unreachable.rs` is. The row carries `void(void)`, so the call has
//! a prototype, and it is the prototype that reports `__builtin_trap(1)` in the ordinary words.

use rucc_base::Symbol;
use rucc_diag::Span;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The name, which is the whole of what this recognises.
const NAME: &str = "__builtin_trap";

impl Checker<'_> {
    /// The node a call to `__builtin_trap` becomes, if the name is that one.
    ///
    /// Answers nothing for every other call in the program, so the test that costs a byte goes
    /// first, and the name decides it rather than the declaration for the reason it does next
    /// door: the reserved prefix is what says the name belongs to the implementation.
    pub(in crate::check) fn trap_builtin(
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
        Some(self.tast.expr(Expr::new(ExprKind::Trap, ty, Category::Rvalue), span))
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
