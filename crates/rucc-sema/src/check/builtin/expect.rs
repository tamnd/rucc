//! `__builtin_expect` and `__builtin_expect_with_probability`, which are their first argument.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! These two say which way a branch is expected to go. The value of `__builtin_expect(x, c)` is
//! `x`, and everything after the first argument is a hint about how often that value will turn out
//! to be `c`. So the answer is the first argument, and the hint is dropped here because there is
//! nothing yet that could read it: branch weights arrive with the optimizer, and until then a node
//! carrying one would be a node every pass has to step over for no gain. `Opcode::Expect` is in the
//! IR waiting for that day.
//!
//! # Why this is not a link error, which is what it was
//!
//! A builtin nothing lowers reaches the assembler as a call to a name no object file defines, and
//! this is the one where that matters most. glibc's `<stdio.h>` writes `getc_unlocked` and its
//! neighbours as extern inline functions in terms of `__builtin_expect` as soon as `__OPTIMIZE__`
//! is set, so before this every program that included that header, called one of those functions
//! and asked for `-O1` failed to link on a name it never wrote. The wider defect, which is that any
//! unimplemented builtin does this rather than saying so, is tamnd/rucc#303.
//!
//! # Why the answer is taken after the call is checked and not before
//!
//! The families next door are recognised before the callee is looked up, because their type comes
//! out of the call and there is no prototype to check them against. These two have one:
//! `long(long, long)` in `features.toml`, which is what gcc gives them, and it is worth keeping.
//! It is where the argument count message comes from, it is what converts the first argument to
//! `long` so that `sizeof(__builtin_expect((char)1, 1))` is eight the way gcc has it, and it is
//! what reports a structure handed to the first parameter in the ordinary words. All of that would
//! have to be written again here to gain nothing.
//!
//! So the call is checked the whole ordinary way and the node is replaced at the end of it. What
//! that costs is that the arguments after the first are checked and then dropped, so a side effect
//! in one does not happen. That is what gcc does with them as well: `__builtin_expect(5, side())`
//! never calls `side`, measured on gcc 16.2.0.

use rucc_base::Symbol;
use rucc_diag::Span;

use crate::check::Checker;
use crate::expr::ExprId;

/// The names whose value is their first argument.
///
/// Both are rows of `features.toml` with a signature, which is what makes them ordinary calls up
/// to the point this replaces them, and the test at the bottom of this file is what keeps the two
/// lists from drifting apart.
const FAMILY: &[&str] = &["__builtin_expect", "__builtin_expect_with_probability"];

impl Checker<'_> {
    /// The value of a call to one of the hint builtins, if the name is one and it was handed
    /// something to answer with.
    ///
    /// Answers nothing for every other call in the program, which is every call, so the test that
    /// costs a byte goes first.
    ///
    /// The name decides this and not the declaration, the same way the library builtins are
    /// decided by their spelling: what `__builtin_expect` means is a fact about the name, since
    /// the prefix is what says the name belongs to the implementation, and a program that declares
    /// one itself has redeclared something it does not own rather than made a function of its own.
    pub(in crate::check) fn expect_builtin_value(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_") || !FAMILY.contains(&spelled) {
            return None;
        }
        // Nothing to answer with, which is the call with no arguments at all. The count has been
        // reported by this point, so the ordinary node is built and the program is refused for the
        // reason it was already going to be refused for.
        let &value = args.first()?;
        if self.is_poisoned(value) {
            return Some(self.poison(span));
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every name here has to be a row of the table, with a signature, because the signature is
    /// what the call is checked against before this replaces it. A row without one would never be
    /// declared and the call would be to an undeclared name.
    #[test]
    fn every_name_in_the_family_is_a_row_of_the_table_that_carries_a_signature() {
        for &name in FAMILY {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert!(!feature.signature.is_empty(), "{name} is checked against its prototype");
            assert!(feature.library.is_empty(), "{name} is not a call to anything");
        }
    }
}
