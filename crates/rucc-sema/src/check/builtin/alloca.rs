//! `__builtin_alloca`, which takes bytes off the frame and answers where they are.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! One argument, how many bytes, and the answer is a pointer to that many bytes of this function's
//! own frame. It is a node rather than a call for the reason its neighbours are: there is no
//! function of the name for a call to reach, and a program that wrote one used to fail to link on a
//! name it never wrote itself.
//!
//! # What makes it different from a local
//!
//! Not the size, which is allowed to be a constant and often is. What makes it different is when
//! the storage goes away: an ordinary local lives as long as the block it was declared in, and this
//! lives until the function returns however deep inside the function it was written. That is what
//! `alloca` in a loop means, which is a frame that grows every time round, and it is why a program
//! that wants one call's worth of scratch writes this rather than a variable length array.
//!
//! The lowering is where that costs something. A block holding a variable length array gives the
//! stack back at the end of itself, and a block holding one of these must not, so an alloca puts
//! every scope that is open where it was written out of the business of giving anything back. That
//! is measured rather than reasoned about: gcc 16.2.0 at -O0 writes no stack restore at all at the
//! end of a block holding an alloca.
//!
//! # The plain name as well as the prefixed one
//!
//! `alloca` is not a name C reserves, so the rule is the one `check/builtin/abs.rs` gives for its
//! family rather than the one the prefix gives: the plain name is this only where the callee is a
//! function with external linkage declared with exactly the type the C library gives `alloca`, and
//! a program that means something of its own by the name has not got one. `-fno-builtin` and
//! `-ffreestanding` turn the plain name back into an ordinary call, and the prefixed spelling goes
//! on meaning this whatever either of them says.
//!
//! gcc expands the plain name inline too, and `gcc.c-torture/execute/20010122-1.c` is a program
//! that depends on it: it declares `extern void *alloca (__SIZE_TYPE__)`, calls it, and would fail
//! to link against a C library that has no such function.
//!
//! # Why this is answered after the call is checked
//!
//! The same reason `check/builtin/thread.rs` is. The row carries `void *(size_t)`, so the call has
//! a prototype, and it is the prototype that converts the argument to a `size_t`, reports a call
//! that wrote two of them in the ordinary words, and gives the expression its type.

use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_types::TypeId;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The prefixed spelling, which is a row of `features.toml` and means this whatever the program
/// has done with the plain name.
const NAME: &str = "__builtin_alloca";

/// The plain spelling, which means this only where the program has left it to the library.
const PLAIN: &str = "alloca";

impl Checker<'_> {
    /// The node a call to either spelling becomes, if the call is one.
    ///
    /// Answers nothing for every other call in the program, so the tests that cost a byte go first.
    /// The prefixed spelling is decided by its name, because the prefix is what says the name
    /// belongs to the implementation, and the plain one has to look at the declaration as well for
    /// the reason written above.
    ///
    /// The type is taken from the call rather than built here, so that the one place saying what
    /// this answers with is the row in the table the call was checked against, or for the plain
    /// spelling the declaration the program wrote.
    pub(in crate::check) fn alloca_builtin(
        &mut self,
        callee: ExprId,
        function: Option<Symbol>,
        args: &[ExprId],
        ret: TypeId,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if spelled != NAME {
            if spelled != PLAIN || !self.cx.means_the_library(spelled) {
                return None;
            }
            let size = self.size_type();
            let answers = self.types.pointer(self.types.void());
            if !self.callee_is_the_library_one(callee, answers, &[size]) {
                return None;
            }
        }
        // No size at all, which the prototype has already refused. The ordinary node is built and
        // the program is refused for the reason it was already going to be refused for.
        let &size = args.first()?;
        if self.is_poisoned(size) {
            return Some(self.poison(span));
        }
        Some(self.tast.expr(Expr::new(ExprKind::Alloca { size }, ret, Category::Rvalue), span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The name has to be a row of the table with a signature, because the signature is what the
    /// call is checked against before this replaces it and is where the type of the result and the
    /// conversion of the argument both come from, and it has to be implemented, because that is
    /// what stops the lowering refusing the call before it ever gets here.
    #[test]
    fn the_prefixed_spelling_is_the_plain_name_with_the_prefix_on_it() {
        assert_eq!(NAME.strip_prefix("__builtin_"), Some(PLAIN));
    }

    #[test]
    fn the_name_is_a_row_of_the_table_that_carries_a_signature() {
        let Some(feature) = rucc_gnu::lookup(Kind::Builtin, NAME) else {
            panic!("{NAME} is answered here and is not in features.toml");
        };
        assert_eq!(feature.status, Status::Implemented);
        assert_eq!(feature.signature, "void *(size_t)");
        assert!(feature.library.is_empty(), "{NAME} is not a call to anything");
    }
}
