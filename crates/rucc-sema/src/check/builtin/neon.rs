//! The AArch64 Advanced SIMD builtins of gcc that NetBSD's `<crypto/arch/arm/arm_neon.h>` calls.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The NetBSD kernel builds with `-nostdinc`, so it cannot use the compiler's `<arm_neon.h>`. Its
//! own header writes each intrinsic it needs as a call to the gcc builtin behind it, such as
//! `__builtin_aarch64_ld1v4si` for `vld1q_u32`, when the compiler says it is gcc. The AES and
//! ChaCha code under `sys/crypto` uses them. rucc's own `<arm_neon.h>` writes the same intrinsics
//! in C, so these names are only for a header that is not rucc's. This is the AArch64 side of
//! what `sse.rs` does for x86, and it borrows that file's rules for operands.
//!
//! Each call is answered with the nodes C would write for it, and not called:
//!
//! - `ld1` is a read through the pointer, cast to a pointer to a vector that may alias anything
//!   and is aligned only to its lane, which is what the instruction asks of the address.
//! - `st1` is an assignment through the same pointer, with no value.
//! - `ashl` is the C shift to the left, for a count from 0 to one less than the lane width.
//! - `lshr` is the shift to the right with zeros in, for a count from 1 to the lane width. A count
//!   of the width gives zero, and C cannot shift that far, so it is `(x >> (n - 1)) >> 1`, which
//!   reads the count once.
//! - `sli`, shift left and insert, is `(vsh << n) | (vins & ~(ones << n))`: the shifted lanes,
//!   with the low bits the shift emptied kept from the first operand.
//! - `sri`, shift right and insert, is the same to the right, with the high bits kept.
//!
//! Each shift is done on unsigned lanes, so a bit shifted out of a signed lane is not overflow.
//!
//! # What gcc asks of the operands
//!
//! A vector operand is any vector of 16 bytes, read as the type the builtin takes, as in
//! `sse.rs`. The count is an integer, and one that is a constant out of the instruction's range
//! is refused with `E0715`. gcc refuses a count that is not a constant once the call has been
//! inlined, but the header passes the count of an inline function, which is only a constant
//! after inlining, so a count that is not a constant is accepted and computed with.
//!
//! The two inserting names use their count twice, once for the shift and once for the mask, so a
//! count that is not a constant is a name, a constant or a cast of one, as `sse.rs` asks. The
//! header passes a parameter.

use std::num::NonZeroU32;

use rucc_ast as ast;
use rucc_ast::{BinaryOp, UnaryOp};
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{IntKind, Qualifiers, TypeId, is_integer, is_pointer};

use super::sse::plain;
use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::tast::Const;

/// The lanes of a vector of 16 bytes, which is what each name takes and gives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    /// `v4si`, four lanes of 32 bits.
    W32,
    /// `v16qi`, sixteen lanes of 8 bits.
    W8,
}

impl Lane {
    /// How wide a lane is, in bits.
    fn bits(self) -> i128 {
        match self {
            Lane::W32 => 32,
            Lane::W8 => 8,
        }
    }

    /// How many lanes there are.
    fn count(self) -> u32 {
        match self {
            Lane::W32 => 4,
            Lane::W8 => 16,
        }
    }
}

/// What a name does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    /// `ld1`: a vector read from a pointer.
    Load,
    /// `st1`: a vector written through a pointer.
    Store,
    /// `ashl`: each lane shifted left by 0 to the width less one.
    ShiftLeft,
    /// `lshr`: each lane shifted right with zeros in, by 1 to the width.
    ShiftRight,
    /// `sli`: the second vector shifted left by 0 to the width less one, into the first.
    InsertLeft,
    /// `sri`: the second vector shifted right by 1 to the width, into the first.
    InsertRight,
}

impl Op {
    /// How many operands the name takes.
    fn operands(self) -> usize {
        match self {
            Op::Load => 1,
            Op::Store | Op::ShiftLeft | Op::ShiftRight => 2,
            Op::InsertLeft | Op::InsertRight => 3,
        }
    }

    /// The counts the instruction can encode.
    fn range(self, lane: Lane) -> (i128, i128) {
        match self {
            Op::ShiftLeft | Op::InsertLeft => (0, lane.bits() - 1),
            _ => (1, lane.bits()),
        }
    }
}

/// Every name, with its lanes, whether the lanes it takes and gives are unsigned, and what it
/// does. The `_uus` and `_uuus` names are gcc 12's unsigned forms, which the header calls from
/// gcc 12 on.
const FAMILY: &[(&str, Lane, bool, Op)] = &[
    ("__builtin_aarch64_ld1v4si", Lane::W32, false, Op::Load),
    ("__builtin_aarch64_ld1v16qi", Lane::W8, false, Op::Load),
    ("__builtin_aarch64_st1v4si", Lane::W32, false, Op::Store),
    ("__builtin_aarch64_st1v16qi", Lane::W8, false, Op::Store),
    ("__builtin_aarch64_ashlv4si", Lane::W32, false, Op::ShiftLeft),
    ("__builtin_aarch64_lshrv4si", Lane::W32, false, Op::ShiftRight),
    ("__builtin_aarch64_lshrv16qi", Lane::W8, false, Op::ShiftRight),
    ("__builtin_aarch64_lshrv4si_uus", Lane::W32, true, Op::ShiftRight),
    ("__builtin_aarch64_lshrv16qi_uus", Lane::W8, true, Op::ShiftRight),
    ("__builtin_aarch64_ssli_nv4si", Lane::W32, false, Op::InsertLeft),
    ("__builtin_aarch64_usri_nv4si_uuus", Lane::W32, true, Op::InsertRight),
];

/// Whether the roster has a row for this name, which is what the test in `generic.rs` asks.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    FAMILY.iter().any(|&(spelled, ..)| spelled == name)
}

impl Checker<'_> {
    /// Answers a call to one of the Advanced SIMD builtins, if the name is one.
    ///
    /// The caller has already made sure the program did not declare the name itself. On a
    /// target that is not AArch64 the names are not builtins, as in gcc, and the call is an
    /// implicit declaration.
    pub(super) fn neon_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        if self.cx.target.tuple.arch().as_str() != "aarch64" {
            return None;
        }
        let spelled = self.text(name);
        let &(spelled, lane, unsigned, op) = FAMILY.iter().find(|&&(row, ..)| row == spelled)?;
        Some(self.neon_call(name, spelled, lane, unsigned, op, args, span))
    }

    /// The call itself, once the name has been recognised.
    #[expect(clippy::too_many_arguments, reason = "the call and the row of the table")]
    fn neon_call(
        &mut self,
        symbol: Symbol,
        name: &'static str,
        lane: Lane,
        unsigned: bool,
        op: Op,
        args: ast::ExprList,
        span: Span,
    ) -> ExprId {
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        // Every argument is checked whatever else is wrong with the call, as in `sse.rs`.
        let values: Vec<ExprId> = written
            .iter()
            .map(|&arg| {
                let arg = self.expr(arg);
                self.value(arg)
            })
            .collect();
        let wanted = op.operands();
        if values.len() != wanted {
            let how = if values.len() < wanted { "few" } else { "many" };
            self.report(
                Diagnostic::error(format!("too {how} arguments to function '{name}'"), span)
                    .with_code("E0511"),
            );
            return self.poison(span);
        }
        if values.iter().any(|&arg| self.is_poisoned(arg)) {
            return self.poison(span);
        }
        let ty = self.neon_vector(lane, unsigned);
        match op {
            Op::Load | Op::Store => self.neon_memory(symbol, name, lane, op, &values, ty, span),
            Op::ShiftLeft | Op::ShiftRight | Op::InsertLeft | Op::InsertRight => {
                let vectors = wanted - 1;
                let mut operands = Vec::with_capacity(vectors);
                for (at, &value) in values.iter().enumerate().take(vectors) {
                    match self.sse_operand(value, ty, name, at) {
                        Some(operand) => operands.push(operand),
                        None => return self.poison(span),
                    }
                }
                let count = values[vectors];
                if !is_integer(&self.types, self.tast[count].ty) {
                    let what = format!("incompatible type for argument {wanted} of '{name}'");
                    let at = self.tast.expr_span(count);
                    self.report(Diagnostic::error(what, at).with_code("E0715"));
                    return self.poison(span);
                }
                let (low, high) = op.range(lane);
                let constant = self.eval_integer(count).ok();
                if let Some(shift) = constant
                    && !(low..=high).contains(&shift)
                {
                    let what = format!(
                        "the shift count of '{name}' is {shift}, and it must be from {low} to \
                         {high}"
                    );
                    let at = self.tast.expr_span(count);
                    self.report(Diagnostic::error(what, at).with_code("E0715"));
                    return self.poison(span);
                }
                let insert = matches!(op, Op::InsertLeft | Op::InsertRight);
                if insert && constant.is_none() && !plain(self.ast, written[vectors]) {
                    return self.sse_read_twice(name, wanted, span);
                }
                let left = matches!(op, Op::ShiftLeft | Op::InsertLeft);
                let wide = self.neon_vector(lane, true);
                let shifted = self.sse_as(operands[vectors - 1], wide);
                let shifted = self.neon_shift(left, shifted, count, span);
                let answer = if insert {
                    // The mask of the bits the shift filled from the second operand, which is
                    // the same shift of every bit set. The count again, which is a constant, a
                    // name or a cast of one, so this reads the same value and does nothing else.
                    let count = match constant {
                        Some(shift) => {
                            let int = self.types.int(IntKind::Int);
                            self.constant(Const::Int(shift), int, span)
                        }
                        None => {
                            let again = self.expr(written[vectors]);
                            self.value(again)
                        }
                    };
                    let zero = self.sse_zero(wide, span);
                    let ones = self.neon_not(zero, wide, span);
                    let mask = self.neon_shift(left, ones, count, span);
                    let keep = self.neon_not(mask, wide, span);
                    let kept = self.sse_as(operands[0], wide);
                    let kept = self.arithmetic_binary(BinaryOp::BitAnd, kept, keep, true, span);
                    self.arithmetic_binary(BinaryOp::BitOr, shifted, kept, true, span)
                } else {
                    shifted
                };
                self.sse_as(answer, ty)
            }
        }
    }

    /// The vector of 16 bytes with these lanes: `int` or `signed char`, or their unsigned types.
    fn neon_vector(&mut self, lane: Lane, unsigned: bool) -> TypeId {
        let element = match (lane, unsigned) {
            (Lane::W32, false) => IntKind::Int,
            (Lane::W32, true) => IntKind::UInt,
            (Lane::W8, false) => IntKind::SChar,
            (Lane::W8, true) => IntKind::UChar,
        };
        let element = self.types.int(element);
        self.types.vector(element, lane.count())
    }

    /// `~value`, on a vector of integers.
    fn neon_not(&mut self, value: ExprId, ty: TypeId, span: Span) -> ExprId {
        let node = ExprKind::Unary { op: UnaryOp::BitNot, operand: value };
        self.tast.expr(Expr::new(node, ty, Category::Rvalue), span)
    }

    /// Each unsigned lane shifted by a count in the instruction's range. To the left that is the
    /// C shift. To the right it is `(value >> (count - 1)) >> 1`, since the count can be the
    /// lane width, which C cannot shift by and where the instruction gives zero.
    fn neon_shift(&mut self, left: bool, value: ExprId, count: ExprId, span: Span) -> ExprId {
        if left {
            return self.shift(BinaryOp::Shl, value, count, span);
        }
        let int = self.types.int(IntKind::Int);
        let one = self.constant(Const::Int(1), int, span);
        let less = self.arithmetic_binary(BinaryOp::Sub, count, one, false, span);
        let most = self.shift(BinaryOp::Shr, value, less, span);
        let one = self.constant(Const::Int(1), int, span);
        self.shift(BinaryOp::Shr, most, one, span)
    }

    /// `ld1` and `st1`: a read or a write of a whole vector through a pointer, which may point
    /// at anything and is aligned to one lane.
    #[expect(clippy::too_many_arguments, reason = "the call and the row of the table")]
    fn neon_memory(
        &mut self,
        symbol: Symbol,
        name: &str,
        lane: Lane,
        op: Op,
        values: &[ExprId],
        ty: TypeId,
        span: Span,
    ) -> ExprId {
        let pointer = values[0];
        if !is_pointer(&self.types, self.tast[pointer].ty) {
            let what = format!("incompatible type for argument 1 of '{name}'");
            let at = self.tast.expr_span(pointer);
            self.report(Diagnostic::error(what, at).with_code("E0715"));
            return self.poison(span);
        }
        let stored = match op {
            Op::Store => match self.sse_operand(values[1], ty, name, 1) {
                Some(stored) => Some(stored),
                None => return self.poison(span),
            },
            _ => None,
        };
        // The typedef gcc's builtin reads through: the vector, aligned to its lane and allowed
        // to alias anything. It goes by the builtin's name, which no program can declare a type
        // under while the builtin is the one being called.
        let align = NonZeroU32::new(u32::try_from(lane.bits() / 8).unwrap_or(1));
        let place = self.types.may_alias_typedef(symbol, ty, align);
        let place =
            if stored.is_none() { self.types.qualified(place, Qualifiers::CONST) } else { place };
        let to = self.types.pointer(place);
        let pointer =
            self.tast.expr(Expr::new(ExprKind::Cast(pointer), to, Category::Rvalue), span);
        let node = ExprKind::Unary { op: UnaryOp::Deref, operand: pointer };
        let lhs = self.tast.expr(Expr::new(node, place, Category::Lvalue), span);
        match stored {
            None => {
                let read = self.value(lhs);
                self.tast.expr(Expr::new(ExprKind::Cast(read), ty, Category::Rvalue), span)
            }
            Some(rhs) => {
                let node = ExprKind::Assign { op: None, computation: place, lhs, rhs };
                let assign = self.tast.expr(Expr::new(node, place, Category::Rvalue), span);
                let void = self.types.void();
                self.tast.expr(Expr::new(ExprKind::Cast(assign), void, Category::Rvalue), span)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Each name is a row of the table with no signature, since it is answered and not called,
    /// and it is on AArch64 only.
    #[test]
    fn every_neon_name_is_a_row_of_the_table_and_is_answered_not_called() {
        for &(name, ..) in FAMILY {
            let row = rucc_gnu::lookup(Kind::Builtin, name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(row.status, Status::Implemented, "{name}");
            assert!(row.signature.is_empty(), "{name}");
            assert!(row.library.is_empty(), "{name}");
            assert_eq!(row.targets, [rucc_gnu::Place::Aarch64], "{name}");
        }
    }

    /// The counts each shift takes are the instruction's: from 0 for a shift to the left and to
    /// the width for one to the right.
    #[test]
    fn each_shift_takes_the_counts_its_instruction_encodes() {
        assert_eq!(Op::ShiftLeft.range(Lane::W32), (0, 31));
        assert_eq!(Op::InsertLeft.range(Lane::W32), (0, 31));
        assert_eq!(Op::ShiftRight.range(Lane::W8), (1, 8));
        assert_eq!(Op::InsertRight.range(Lane::W32), (1, 32));
    }
}
