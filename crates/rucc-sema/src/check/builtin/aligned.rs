//! `__builtin_assume_aligned`, which is its first argument and a promise about the low bits.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The value of `__builtin_assume_aligned(p, n)` is `p`, and what the rest of the call says is
//! that the address is a multiple of `n`, or a multiple of `n` once a third argument has been
//! taken off it. Nothing in this compiler reads an alignment fact about a value yet, so the
//! promise is dropped and the pointer is the whole of the answer, which is the same shape
//! `__builtin_expect` took before there was a pass that read its hint.
//!
//! Dropping it is correct rather than merely tolerable. The promise cannot make a program mean
//! something different, since a program whose promise is false is undefined either way and a
//! program whose promise is true gets the same answer from an address the compiler knows nothing
//! about. What it costs is the wider load the promise would have allowed, which is a pass that
//! does not exist here yet.
//!
//! # Why this is not a link error, which is what it was
//!
//! The same reason `check/builtin/expect.rs` gives. A builtin nothing lowers reaches the
//! assembler as a call to a name no object file defines, and glibc's string headers and every
//! hand vectorised loop in ffmpeg write this one, so a program that included the wrong header and
//! asked for `-O2` failed to link on a name it never wrote.
//!
//! # What happens to the arguments that are not the answer
//!
//! They are evaluated. gcc evaluates them at every optimization level, which was measured on
//! gcc 16.2.0 with a counter in a function called from the alignment argument rather than
//! reasoned about, and it evaluates them even though it has folded the call away. So each one
//! that is not already a constant is kept in a comma in front of the answer, which is what
//! `check/builtin/prefetch.rs` does with the arguments it has no use for.
//!
//! That leaves one difference from gcc, and it is worth writing down rather than hiding: a comma
//! runs its left side first, so an argument kept this way runs before the pointer expression
//! rather than after it. It shows only where the pointer and the alignment both have side
//! effects, which is a call nobody writes, and the alternative is a temporary for a value that is
//! about to be thrown away.

use rucc_base::Symbol;
use rucc_diag::Span;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The name, which is the whole of what this recognises.
const NAME: &str = "__builtin_assume_aligned";

impl Checker<'_> {
    /// The value of a call to `__builtin_assume_aligned`, if the name is that one.
    ///
    /// Answers nothing for every other call in the program, so the test that costs a byte goes
    /// first, and the name decides it rather than the declaration for the reason it does next
    /// door: the reserved prefix is what says the name belongs to the implementation.
    ///
    /// The call is checked against its prototype before this replaces it, which is where the
    /// argument count is reported and where the pointer argument is converted, so the answer here
    /// is `const void *` and the call gcc declares answers `void *`. The cast back is the one
    /// piece of work this does, and it is what makes `__builtin_assume_aligned(p, 16)` assignable
    /// to a `char *` without a diagnostic about dropping a qualifier.
    pub(in crate::check) fn assume_aligned_value(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_") || spelled != NAME {
            return None;
        }
        // Nothing to answer with, which is the call with no arguments at all. The count has been
        // reported by this point, so the ordinary node is built and the program is refused for
        // the reason it was already going to be refused for.
        let &value = args.first()?;
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return Some(self.poison(span));
        }
        let void = self.types.void();
        let ty = self.types.pointer(void);
        let mut answer = self.conv().to_type(value, ty);

        // Backwards, so that what is kept runs in the order it was written: a comma runs its left
        // side first, so wrapping from the inside out puts the first of them outermost. An
        // argument that folds to a constant is dropped, since a constant is the case where there
        // is nothing to run and the whole point of keeping the others is that there is.
        for &rest in args[1..].iter().rev() {
            if self.eval().constant(rest).is_ok() {
                continue;
            }
            let node = ExprKind::Comma { lhs: rest, rhs: answer };
            answer = self.tast.expr(Expr::new(node, ty, Category::Rvalue), span);
        }
        Some(answer)
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The name has to be a row of the table with a signature, because the signature is what the
    /// call is checked against before this replaces it, and it has to be implemented, because
    /// that is what stops the lowering refusing the call before it ever gets here.
    #[test]
    fn the_name_is_a_row_of_the_table_that_carries_a_signature() {
        let Some(feature) = rucc_gnu::lookup(Kind::Builtin, NAME) else {
            panic!("{NAME} is answered here and is not in features.toml");
        };
        assert_eq!(feature.status, Status::Implemented);
        assert_eq!(feature.signature, "void *(const void *, size_t, ...)");
        assert!(feature.library.is_empty(), "{NAME} is not a call to anything");
    }
}
