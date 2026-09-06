//! The overflow checking builtins: `__builtin_add_overflow` and its two neighbours.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5, and tamnd/rucc#309.
//!
//! Each takes two integers and a pointer to an integer, does the arithmetic as if the operands
//! were mathematical integers rather than values of a C type, writes the low bits of the exact
//! answer through the pointer, and gives back whether the exact answer was the one it wrote. That
//! is the whole of it, and it is the only portable way to write the check: `a + b < a` is a test
//! on the wrapped answer, and for signed operands the wrap it is testing already had undefined
//! behaviour before the test ran.
//!
//! SQLite is why this is here now. Its `sqlite3AddInt64`, `sqlite3SubInt64` and `sqlite3MulInt64`
//! are one line each and the line is one of these, so an amalgamation build reaches all three
//! within a few lines of each other. The kernel's `check_add_overflow` and glibc's allocation size
//! checks are the same shape.
//!
//! # The type the arithmetic happens at
//!
//! The three types in the call need not agree, and gcc does not require them to. This picks one
//! type wide enough to represent every value of all three, does the arithmetic there, and then
//! asks whether the answer fit in the type being written to. That works because a type that
//! represents every value of the destination also represents every value that the destination
//! cannot hold but is adjacent to, so narrowing and widening back and comparing is an exact test.
//!
//! The rule is the obvious one. If all three types are unsigned, the widest of them will do. If
//! any is signed, the answer must be signed, and it must be wide enough for every signed type as
//! written and for every unsigned type with a bit to spare for the sign. The result is rounded up
//! to `int` or `long long`, because those are the widths the IR does arithmetic at and the extra
//! bits cost nothing.
//!
//! # What is refused
//!
//! A call needing more than sixty four bits, which is E0694. Two ways to get there: an operand of
//! `__int128`, or a mix of a sixty four bit unsigned type with a signed one, which needs sixty
//! five bits to represent both. gcc handles the second by being cleverer about the mixed case
//! rather than by widening. That cleverness is worth having and is not worth blocking the common
//! case on, so for now the call gets a message that says what it needed rather than a wrong
//! answer. Nothing in SQLite, or in anything else measured, is written that way: the three types
//! agree in almost every real call.
//!
//! # Why the arguments are not promoted
//!
//! `check/builtin/generic.rs` gives `Param::Integer` the type the argument already had, so a
//! `short` operand arrives here as a `short`. That is deliberate and it is gcc's rule too, but it
//! makes no difference to the answer: the type below picks something at least as wide as `int`
//! anyway, so a promotion would have been undone by the widening that follows it.

use rucc_diag::{Diagnostic, Span};
use rucc_types::{IntKind, IntegerInfo, TypeId, integer_info, pointee};

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind, OverflowOp};

/// The three names and the arithmetic each asks for.
const FAMILY: &[(&str, OverflowOp)] = &[
    ("__builtin_add_overflow", OverflowOp::Add),
    ("__builtin_sub_overflow", OverflowOp::Sub),
    ("__builtin_mul_overflow", OverflowOp::Mul),
];

/// The widest type the arithmetic can be done at, because it is the widest the IR legalizes.
const LIMIT: u32 = 64;

/// Which of the three a name is, if it is one of them.
pub(in crate::check) fn operation(spelled: &str) -> Option<OverflowOp> {
    FAMILY.iter().find(|&&(name, _)| name == spelled).map(|&(_, op)| op)
}

/// The shape of the type the arithmetic has to happen at, given the three types in the call.
///
/// Separate from the checker so that the rule can be tested on shapes rather than on programs,
/// which is the only way to cover the corners of it without writing a hundred calls.
fn common(shapes: [IntegerInfo; 3]) -> Option<IntegerInfo> {
    let signed = shapes.iter().any(|shape| shape.signed);
    let width = shapes
        .iter()
        .map(|shape| if signed && !shape.signed { shape.width + 1 } else { shape.width })
        .max()
        .unwrap_or(0);
    // Rounded up to a width the IR does arithmetic at. Below thirty two there is nothing to gain
    // by being narrow, since every operand was going to be widened into a register anyway.
    let width = if width <= 32 {
        32
    } else if width <= LIMIT {
        LIMIT
    } else {
        return None;
    };
    Some(IntegerInfo::new(signed, width))
}

impl Checker<'_> {
    /// Builds the node for a call to one of the three, once the arguments have been checked.
    ///
    /// The arguments arrive converted to values and in the types they were written with, which is
    /// what the rule above needs. Nothing here converts them, because the walk to the IR is where
    /// the widening belongs: it is the one place that knows the signedness of each operand and so
    /// knows whether to sign extend or zero extend it.
    pub(in crate::check) fn overflow_builtin(
        &mut self,
        op: OverflowOp,
        spelled: &str,
        args: &[ExprId],
        span: Span,
    ) -> ExprId {
        let [lhs, rhs, out] = args[..] else { return self.poison(span) };
        let written = pointee(&self.types, self.tast[out].ty);
        // Each of the three has already been checked to be an integer, or a pointer to one, by
        // `argument_fits`. The only way to be here without a shape is an enumeration whose
        // definition has not been seen, which has been complained about already.
        let shapes = [self.tast[lhs].ty, self.tast[rhs].ty, written.unwrap_or(self.tast[out].ty)];
        let mut found = [IntegerInfo::new(false, 0); 3];
        for (slot, ty) in found.iter_mut().zip(shapes) {
            let Some(shape) = integer_info(&self.types, ty, self.cx.target) else {
                return self.poison(span);
            };
            *slot = shape;
        }
        let Some(shape) = common(found) else {
            self.report(
                Diagnostic::error(
                    format!(
                        "'{spelled}' needs an integer type wider than {LIMIT} bits for these \
                         arguments, which is not supported yet"
                    ),
                    span,
                )
                .with_code("E0694"),
            );
            return self.poison(span);
        };
        let at = self.widest(shape);
        let ty = self.types.boolean();
        // Left, right, destination, in that order, which is the order they were written in and the
        // order every walk over the node reads them back in.
        let args = self.tast.add_expr_refs(&[lhs, rhs, out]);
        self.tast.expr(Expr::new(ExprKind::Overflow { op, at, args }, ty, Category::Rvalue), span)
    }

    /// A standard integer type of the given shape.
    ///
    /// Only two widths ever reach this, and both have a standard type on every target this
    /// compiler has, so there is no case where a `_BitInt` would have to be made up.
    fn widest(&mut self, shape: IntegerInfo) -> TypeId {
        let kind = match (shape.signed, shape.width) {
            (true, 32) => IntKind::Int,
            (false, 32) => IntKind::UInt,
            (true, _) => IntKind::LongLong,
            (false, _) => IntKind::ULongLong,
        };
        self.types.int(kind)
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::Kind;

    use super::*;

    /// A shape, written short because the tests below are all lists of them.
    fn shape(signed: bool, width: u32) -> IntegerInfo {
        IntegerInfo::new(signed, width)
    }

    /// The names have to be rows of the roster, or `__has_builtin` has never heard of them, and
    /// they have to carry no signature, or the ordinary call checking would answer for them first.
    #[test]
    fn all_three_names_are_rows_of_the_table_that_carry_no_signature() {
        for &(name, _) in FAMILY {
            let feature = rucc_gnu::lookup(Kind::Builtin, name).unwrap_or_else(|| {
                panic!("{name} is not a row of features.toml");
            });
            assert!(feature.signature.is_empty(), "{name} has a signature and is type generic");
        }
    }

    /// Three unsigned types need nothing signed, and the widest of them holds the other two.
    #[test]
    fn three_unsigned_types_are_done_unsigned_at_the_widest_of_them() {
        let all = [shape(false, 32), shape(false, 32), shape(false, 32)];
        assert_eq!(common(all), Some(shape(false, 32)));
        let mixed = [shape(false, 8), shape(false, 64), shape(false, 16)];
        assert_eq!(common(mixed), Some(shape(false, 64)));
    }

    /// One signed type anywhere in the call makes the arithmetic signed, including when it is the
    /// destination rather than an operand, because the question is whether the answer fits there.
    #[test]
    fn one_signed_type_anywhere_makes_the_arithmetic_signed() {
        let operand = [shape(true, 32), shape(false, 32), shape(false, 32)];
        assert_eq!(common(operand), Some(shape(true, LIMIT)));
        let destination = [shape(false, 16), shape(false, 16), shape(true, 16)];
        assert_eq!(common(destination), Some(shape(true, 32)));
    }

    /// An unsigned type in signed arithmetic costs a bit, which is what pushes a call written with
    /// `unsigned int` and `int` up to sixty four rather than leaving it at thirty two.
    #[test]
    fn an_unsigned_type_costs_a_bit_once_the_arithmetic_is_signed() {
        let just_over = [shape(false, 32), shape(true, 8), shape(true, 8)];
        assert_eq!(common(just_over), Some(shape(true, LIMIT)));
        let still_under = [shape(false, 31), shape(true, 8), shape(true, 8)];
        assert_eq!(common(still_under), Some(shape(true, 32)));
    }

    /// Nothing narrow is done narrowly. Three `char` operands are added at thirty two bits, which
    /// costs nothing and means the walk to the IR never asks for a width the back end has to
    /// legalize.
    #[test]
    fn a_narrow_call_is_still_done_at_thirty_two_bits() {
        let narrow = [shape(true, 8), shape(true, 8), shape(true, 8)];
        assert_eq!(common(narrow), Some(shape(true, 32)));
    }

    /// The two ways past sixty four bits, both of which are refused rather than got wrong.
    #[test]
    fn a_call_needing_more_than_sixty_four_bits_has_no_common_type() {
        let wide = [shape(true, 128), shape(true, 32), shape(true, 32)];
        assert_eq!(common(wide), None);
        let mixed = [shape(false, LIMIT), shape(true, 32), shape(true, LIMIT)];
        assert_eq!(common(mixed), None);
    }

    /// The same three unsigned sixty four bit types, which is the case above without the signed
    /// operand, and which is fine. Written next to it because the difference between the two is
    /// the whole of the rule.
    #[test]
    fn the_same_widths_unsigned_throughout_are_not_refused() {
        let wide = [shape(false, LIMIT), shape(false, LIMIT), shape(false, LIMIT)];
        assert_eq!(common(wide), Some(shape(false, LIMIT)));
    }

    /// A name outside the family asks for nothing, including the neighbouring builtins that are
    /// spelled almost the same way.
    #[test]
    fn a_name_outside_the_family_asks_for_nothing() {
        assert_eq!(operation("__builtin_add_overflow"), Some(OverflowOp::Add));
        assert_eq!(operation("__builtin_sub_overflow"), Some(OverflowOp::Sub));
        assert_eq!(operation("__builtin_mul_overflow"), Some(OverflowOp::Mul));
        assert_eq!(operation("__builtin_add_overflow_p"), None);
        assert_eq!(operation("add_overflow"), None);
        assert_eq!(operation("__builtin_popcount"), None);
    }
}
