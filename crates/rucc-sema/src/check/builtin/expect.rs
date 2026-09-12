//! `__builtin_expect` and `__builtin_expect_with_probability`, which are their first argument.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! These two say which way a branch is expected to go. The value of `__builtin_expect(x, c)` is
//! `x`, and everything after the first argument is a hint about how often that value will turn out
//! to be `c`. So the answer is the first argument, with the hint kept beside it in an
//! [`ExprKind::Expect`], which is the node `Opcode::Expect` was in the IR waiting for.
//!
//! The hint used to be dropped here, and the reason it was is worth keeping: a node every pass has
//! to step over is a cost, and it is only worth paying where something reads what the node carries.
//! Something does now. `rucc_opt::expect` runs first at every level, writes what the node says onto
//! the arms of the branch the value controls, and takes the node away in the same walk, so no pass
//! after the first one sees it and nothing downstream of the optimizer has a case for it.
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
//! So the call is checked the whole ordinary way and the node is replaced at the end of it.
//!
//! # What happens to a side effect in the hint
//!
//! Whether the hint runs depends on the first argument, which is not a rule anybody would design
//! and is what gcc 16.2.0 does. A first argument that is a constant folds the whole call where it
//! is written and the hint goes with it, so `__builtin_expect(5, side())` never calls `side`. A
//! first argument that is not a constant leaves a call standing until well after the arguments
//! have been evaluated, so `__builtin_expect(x, side())` does call it.
//!
//! That was measured across five shapes at three optimization levels rather than reasoned about:
//! the hint dropped and the hint kept, the value used and the value thrown away, and a constant
//! first argument against a variable one. gcc gives the same answer at `-O0`, `-O1` and `-O2`.
//! This compiler agreed on the first two shapes and dropped the hint on the other three, which is
//! tamnd/rucc#584, and `execute/pr85156.c` in the GCC torture suite is a program that notices:
//! the value it returns is a `z++` written inside a hint.
//!
//! It is matched rather than tidied up because the whole reason a builtin exists is that a program
//! written against gcc gets gcc's answer, and there is no reading of this one that both keeps the
//! side effect and folds `sizeof(__builtin_expect((char)1, 1))` to eight.

use rucc_base::Symbol;
use rucc_base::float::Float;
use rucc_diag::Span;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::tast::Const;

/// What a probability is out of, which is what `rucc_ir::Hint` is out of.
const SCALE: u32 = 10_000;

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
        for &hint in &args[1..] {
            if self.is_poisoned(hint) {
                return Some(self.poison(span));
            }
        }
        // The folding is asked and its diagnostics are dropped, because the question here is
        // whether the first argument is a constant and not whether the program was allowed to
        // write one. An argument that is not a constant is not a mistake anywhere in this call.
        if self.eval().integer(value).is_ok() {
            return Some(value);
        }
        // A call with one argument, which the prototype has already refused. The node needs two, so
        // what is left is the value, which is what the call answers with either way.
        let Some(&hint) = args.get(1) else { return Some(value) };
        let parts = args.get(2).and_then(|&arg| self.expect_probability(arg));
        let ty = self.tast[value].ty;
        let node = ExprKind::Expect { value, hint, parts };
        let mut answer = self.tast.expr(Expr::new(node, ty, Category::Rvalue), span);

        // Whatever the node did not take, which is the probability where it would not fold and any
        // argument past the third that the prototype has already complained about. Backwards, so
        // that they are evaluated in the order they were written: a comma runs its left side first,
        // so wrapping from the inside out puts the first of them outermost. The type of each is the
        // type of the value, since a comma is its right side and nothing here changes the answer.
        let left = if parts.is_some() { 3 } else { 2 };
        for &rest in args.iter().skip(left).rev() {
            let node = ExprKind::Comma { lhs: rest, rhs: answer };
            answer = self.tast.expr(Expr::new(node, ty, Category::Rvalue), span);
        }
        Some(answer)
    }

    /// The third argument of `__builtin_expect_with_probability`, in ten thousandths.
    ///
    /// Nothing where it is not a floating constant between zero and one, which gcc refuses outright
    /// and this treats as a call that said nothing about how often the expectation holds. Refusing
    /// it is the better answer and it is a diagnostic rather than a lowering, so it belongs to
    /// tamnd/rucc#303's sweep over what the builtins say about their arguments.
    ///
    /// The scale is the one [`rucc_ir::Hint`] is in, and the multiplication happens here rather than
    /// downstream because this is the only place that still has the target's floating format.
    fn expect_probability(&mut self, expr: ExprId) -> Option<u16> {
        let Const::Float(value) = self.eval().constant(expr).ok()? else { return None };
        if !value.is_finite() || value.is_negative() {
            return None;
        }
        let (scale, _) = Float::from_unsigned(u128::from(SCALE), value.format());
        let (scaled, _) = value.product(scale);
        let (parts, _) = scaled.to_integer(32, false);
        u16::try_from(parts).ok().filter(|&parts| u32::from(parts) <= SCALE)
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
