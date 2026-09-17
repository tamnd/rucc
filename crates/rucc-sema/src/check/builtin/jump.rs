//! `__builtin_setjmp` and `__builtin_longjmp`, which save a place in a function and come back to it.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The save writes down where the function is and answers zero. The restore reads that back and
//! goes there, and the save answers one the second time round. Neither is a call: there is no
//! function of either name for a call to reach, the buffer is five words rather than the `jmp_buf`
//! the C library declares, and the pair puts a requirement on the function they are written in
//! that no library function could put there.
//!
//! # Why one node for the pair
//!
//! [`ExprKind::Jump`] carries which end it is, for the reason [`ExprKind::FrameAddress`] carries
//! which of its two builtins it came from. The two ends are written against each other over the
//! same buffer and neither means anything without the other, so a reader who finds one wants the
//! other in the same place.
//!
//! # Why the second argument has to be 1
//!
//! `longjmp` in the C library takes the value the matching `setjmp` is to answer with, and zero is
//! turned into one there because a `setjmp` that answered zero twice would say it had not been
//! jumped to. These are not those: the value the save answers is decided by which way control got
//! to it and not by anything written at the restore, so the argument is a place-holder that has to
//! be the one value it could have been. gcc 16.2.0 refuses every other value with
//! `__builtin_longjmp second argument must be 1`, and so does this.
//!
//! # Why this is answered after the call is checked
//!
//! The same reason `check/builtin/alloca.rs` is. Both rows carry a signature, so both calls have a
//! prototype, and it is the prototype that converts the buffer to a `void *`, gives the save its
//! `int`, and reports a call written with the wrong number of arguments in the ordinary words.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::TypeId;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind, JumpAsk};

/// The name that saves a place, whose value says how control got to it.
const SAVE: &str = "__builtin_setjmp";

/// The name that goes back to one, which answers nothing and never comes back.
const RESTORE: &str = "__builtin_longjmp";

/// The only value the restore's second argument is allowed to have.
const VALUE: i128 = 1;

/// The code that second argument reports under.
const CODE: &str = "E0710";

impl Checker<'_> {
    /// The node a call to either name becomes, if the call is one.
    ///
    /// Answers nothing for every other call in the program, so the test that costs a byte goes
    /// first, and the name decides it rather than the declaration, because the reserved prefix is
    /// what says the name belongs to the implementation.
    pub(in crate::check) fn jump_builtin(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        ret: TypeId,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_") {
            return None;
        }
        let ask = match spelled {
            SAVE => JumpAsk::Save,
            RESTORE => JumpAsk::Restore,
            _ => return None,
        };
        // No buffer at all, which the prototype has already refused. The program is turned down
        // for the reason it was already going to be turned down for.
        let &buffer = args.first()?;
        if self.is_poisoned(buffer) {
            return Some(self.poison(span));
        }
        if ask == JumpAsk::Restore && !self.jump_value_is_one(args) {
            return Some(self.poison(span));
        }
        Some(self.tast.expr(Expr::new(ExprKind::Jump { ask, buffer }, ret, Category::Rvalue), span))
    }

    /// Whether the restore's second argument is the one value it may have, complaining if not.
    ///
    /// The folding is asked and its own complaints are dropped, for the reason
    /// `check/builtin/size.rs` drops them: what is wrong here is that the argument is not the
    /// constant 1, said in the words of the builtin it is an argument of.
    fn jump_value_is_one(&mut self, args: &[ExprId]) -> bool {
        let Some(&value) = args.get(1) else { return true };
        if self.is_poisoned(value) {
            return false;
        }
        if self.eval().integer(value).ok() == Some(VALUE) {
            return true;
        }
        let what = format!("the second argument of `{RESTORE}` is `{VALUE}`");
        let note = "this pair does not carry a value back the way the library's `longjmp` does, \
                    because what the matching `__builtin_setjmp` answers is decided by which way \
                    control reached it, so the argument is a place-holder with one allowed value";
        let span = self.tast.expr_span(value);
        self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
        false
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Both names have to be rows of the table with a signature, because the signature is what the
    /// call is checked against before this replaces it and is where the type of the result and the
    /// conversion of the buffer both come from, and both have to be implemented, because that is
    /// what stops the lowering refusing the call before it ever gets here.
    #[test]
    fn the_two_names_are_rows_of_the_table_that_carry_signatures() {
        for (name, signature) in [(SAVE, "int(void *)"), (RESTORE, "void(void *, int)")] {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented);
            assert_eq!(feature.signature, signature);
            assert!(feature.library.is_empty(), "{name} is not a call to anything");
        }
    }
}
