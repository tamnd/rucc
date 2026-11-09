//! The x86 SSE builtins of gcc that NetBSD's `<crypto/arch/x86/immintrin.h>` calls.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The NetBSD kernel builds with `-nostdinc`, so it cannot use the compiler's `<immintrin.h>`. Its
//! own header writes each intrinsic it needs as a call to the gcc builtin behind it, such as
//! `__builtin_ia32_pshufb128` for `_mm_shuffle_epi8`, when the compiler says it is gcc. The AES
//! and ChaCha code under `sys/crypto` uses them. rucc's own headers write the same intrinsics in
//! C, so these names are only for a header that is not rucc's.
//!
//! Each call is answered with the nodes C would write for it, and not called: there is no
//! function behind the name. The unpacks, the moves of halves, `pshufd`, `shufps` and the byte
//! shifts are a `__builtin_shufflevector` with constant indices. A shift of each lane by a count
//! is the C shift. gcc gives zero for a count of the lane width or more, so a count that is not
//! a constant is also compared with the width. `pshufb` is a `__builtin_shuffle` of the bytes,
//! and a byte whose index has the top bit set is zero.
//!
//! # What gcc asks of the operands
//!
//! gcc's prototypes take vectors of 16 bytes with a fixed lane type, and a header casts to that
//! type before each call. A vector of 16 bytes of another lane type is read as the type the
//! builtin takes, the same bytes, as `-flax-vector-conversions` does. Anything else is refused.
//! An operand that is an immediate in the instruction is an integer constant.
//!
//! # Where an operand is read twice
//!
//! The count of a shift that is not a constant, and the index vector of `pshufb`, are each used
//! two times: once for the value and once for the test. The front end has no temporary to keep
//! one value in, so the operand is checked a second time, which is only right when that has no
//! effect. That is so for a name, a constant, or a cast of one, which is how each header writes
//! these calls, inside an inline function that takes them as parameters. Anything else is
//! refused rather than evaluated twice.

use rucc_ast::{self as ast, BinaryOp};
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{FloatKind, IntKind, TypeId, compatible, is_integer, layout};

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::tast::Const;

/// The lane type of a vector of 16 bytes, which is what each name takes and gives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    /// `__v4sf`, four `float`.
    F32,
    /// `__v4si`, four `int`.
    I32,
    /// `__v2di`, two `long long`.
    I64,
    /// `__v8hi`, eight `short`.
    I16,
    /// `__v16qi`, sixteen `char`.
    I8,
}

/// What a name does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    /// Two vectors, and the lanes of the answer picked from the two laid end to end.
    Pick(&'static [i128]),
    /// A vector and a count, each lane shifted left.
    ShiftLeft,
    /// A vector and a count, each lane shifted right with zeros in.
    ShiftRight,
    /// `pshufb`: a vector of bytes and a vector of indices into it.
    Pshufb,
    /// `pshufd`: a vector and an immediate of four 2 bit indices.
    Pshufd,
    /// `shufps`: two vectors and an immediate of four 2 bit indices, two into each.
    Shufps,
    /// `pslldq`: the whole vector shifted left by a count of bits that is a multiple of 8.
    BytesLeft,
    /// `psrldq`: the same, to the right.
    BytesRight,
    /// `palignr`: two vectors laid end to end, the second one first, shifted right by a count
    /// of bits.
    Palignr,
}

/// Every name, with the lane type it takes and gives and what it does.
const FAMILY: &[(&str, Lane, Op)] = &[
    ("__builtin_ia32_movhlps", Lane::F32, Op::Pick(&[6, 7, 2, 3])),
    ("__builtin_ia32_movlhps", Lane::F32, Op::Pick(&[0, 1, 4, 5])),
    ("__builtin_ia32_unpcklps", Lane::F32, Op::Pick(&[0, 4, 1, 5])),
    ("__builtin_ia32_unpckhps", Lane::F32, Op::Pick(&[2, 6, 3, 7])),
    ("__builtin_ia32_punpcklbw128", Lane::I8, Op::Pick(&LOW_BYTES)),
    ("__builtin_ia32_punpckhbw128", Lane::I8, Op::Pick(&HIGH_BYTES)),
    ("__builtin_ia32_punpcklwd128", Lane::I16, Op::Pick(&[0, 8, 1, 9, 2, 10, 3, 11])),
    ("__builtin_ia32_punpckhwd128", Lane::I16, Op::Pick(&[4, 12, 5, 13, 6, 14, 7, 15])),
    ("__builtin_ia32_punpckldq128", Lane::I32, Op::Pick(&[0, 4, 1, 5])),
    ("__builtin_ia32_punpckhdq128", Lane::I32, Op::Pick(&[2, 6, 3, 7])),
    ("__builtin_ia32_punpcklqdq128", Lane::I64, Op::Pick(&[0, 2])),
    ("__builtin_ia32_punpckhqdq128", Lane::I64, Op::Pick(&[1, 3])),
    ("__builtin_ia32_psllwi128", Lane::I16, Op::ShiftLeft),
    ("__builtin_ia32_pslldi128", Lane::I32, Op::ShiftLeft),
    ("__builtin_ia32_psllqi128", Lane::I64, Op::ShiftLeft),
    ("__builtin_ia32_psrlwi128", Lane::I16, Op::ShiftRight),
    ("__builtin_ia32_psrldi128", Lane::I32, Op::ShiftRight),
    ("__builtin_ia32_psrlqi128", Lane::I64, Op::ShiftRight),
    ("__builtin_ia32_pshufb128", Lane::I8, Op::Pshufb),
    ("__builtin_ia32_pshufd", Lane::I32, Op::Pshufd),
    ("__builtin_ia32_shufps", Lane::F32, Op::Shufps),
    ("__builtin_ia32_pslldqi128", Lane::I64, Op::BytesLeft),
    ("__builtin_ia32_psrldqi128", Lane::I64, Op::BytesRight),
    ("__builtin_ia32_palignr128", Lane::I64, Op::Palignr),
];

/// The picks of `punpcklbw`, the low eight bytes of the two vectors in turn.
const LOW_BYTES: [i128; 16] = [0, 16, 1, 17, 2, 18, 3, 19, 4, 20, 5, 21, 6, 22, 7, 23];

/// The picks of `punpckhbw`, the high eight bytes of the two vectors in turn.
const HIGH_BYTES: [i128; 16] = [8, 24, 9, 25, 10, 26, 11, 27, 12, 28, 13, 29, 14, 30, 15, 31];

/// Whether the roster has a row for this name, which is what the test in `generic.rs` asks.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    FAMILY.iter().any(|&(spelled, ..)| spelled == name)
}

impl Op {
    /// How many operands the name takes.
    fn operands(self) -> usize {
        match self {
            Op::Shufps | Op::Palignr => 3,
            _ => 2,
        }
    }

    /// Whether the last operand is an integer rather than a vector.
    fn takes_integer(self) -> bool {
        !matches!(self, Op::Pick(_) | Op::Pshufb)
    }
}

/// Whether reading this operand a second time reads the same value and does nothing else: a
/// name, a constant, or a cast of one.
fn plain(ast: &ast::Ast, expr: ast::ExprId) -> bool {
    match ast[expr] {
        ast::Expr::Name(_) | ast::Expr::Int(_) | ast::Expr::Char(_) => true,
        ast::Expr::Cast { operand, .. } => plain(ast, operand),
        _ => false,
    }
}

impl Checker<'_> {
    /// Answers a call to one of the SSE builtins, if the name is one.
    ///
    /// The caller has already made sure the program did not declare the name itself, in which
    /// case what it declared is what the call is checked against. On a target that is not x86
    /// the names are not builtins, as in gcc, and the call is an implicit declaration.
    pub(super) fn sse_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        if !matches!(self.cx.target.tuple.arch().as_str(), "x86_64" | "i686") {
            return None;
        }
        let spelled = self.text(name);
        let &(spelled, lane, op) = FAMILY.iter().find(|&&(row, ..)| row == spelled)?;
        Some(self.sse_call(spelled, lane, op, args, span))
    }

    /// The call itself, once the name has been recognised.
    fn sse_call(
        &mut self,
        name: &'static str,
        lane: Lane,
        op: Op,
        args: ast::ExprList,
        span: Span,
    ) -> ExprId {
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        // Every argument is checked whatever else is wrong with the call, because a mistake
        // inside one of them is worth hearing about even when there is one too many of them.
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
        let ty = self.sse_vector(lane);
        let vectors = if op.takes_integer() { wanted - 1 } else { wanted };
        let mut operands = Vec::with_capacity(wanted);
        for (at, &value) in values.iter().enumerate().take(vectors) {
            match self.sse_operand(value, ty, name, at) {
                Some(operand) => operands.push(operand),
                None => return self.poison(span),
            }
        }
        let last = values[wanted - 1];
        if op.takes_integer() && !is_integer(&self.types, self.tast[last].ty) {
            let what = format!("incompatible type for argument {wanted} of '{name}'");
            self.report(Diagnostic::error(what, self.tast.expr_span(last)).with_code("E0715"));
            return self.poison(span);
        }
        match op {
            Op::Pick(picks) => self.sse_pick(operands[0], operands[1], picks, ty, span),
            Op::ShiftLeft | Op::ShiftRight => {
                let left = op == Op::ShiftLeft;
                self.sse_shift(name, lane, left, operands[0], last, written[1], ty, span)
            }
            Op::Pshufb => self.sse_pshufb(name, operands[0], operands[1], written[1], ty, span),
            Op::Pshufd | Op::Shufps | Op::BytesLeft | Op::BytesRight | Op::Palignr => {
                let Some(imm) = self.sse_immediate(name, last, span) else {
                    return self.poison(span);
                };
                self.sse_constant_pick(op, &operands, imm, ty, span)
            }
        }
    }

    /// The vector of 16 bytes with this lane type.
    fn sse_vector(&mut self, lane: Lane) -> TypeId {
        let (element, count) = match lane {
            Lane::F32 => (self.types.float(FloatKind::Float), 4),
            Lane::I32 => (self.types.int(IntKind::Int), 4),
            Lane::I64 => (self.types.int(IntKind::LongLong), 2),
            Lane::I16 => (self.types.int(IntKind::Short), 8),
            Lane::I8 => (self.types.int(IntKind::Char), 16),
        };
        self.types.vector(element, count)
    }

    /// A vector operand read as the type the builtin takes.
    fn sse_operand(&mut self, value: ExprId, ty: TypeId, name: &str, at: usize) -> Option<ExprId> {
        let from = self.types.unqualified(self.tast[value].ty);
        let size = layout(&self.types, from, self.cx.target).map_or(0, |layout| layout.size);
        if !rucc_types::is_vector(&self.types, from) || size != 16 {
            let what = format!("incompatible type for argument {} of '{name}'", at + 1);
            self.report(Diagnostic::error(what, self.tast.expr_span(value)).with_code("E0715"));
            return None;
        }
        Some(self.sse_as(value, ty))
    }

    /// The same bytes read as another vector type, which is a cast in C.
    fn sse_as(&mut self, value: ExprId, ty: TypeId) -> ExprId {
        let from = self.types.unqualified(self.tast[value].ty);
        if compatible(&self.types, from, ty) {
            return value;
        }
        let span = self.tast.expr_span(value);
        self.tast.expr(Expr::new(ExprKind::Cast(value), ty, Category::Rvalue), span)
    }

    /// A vector of this type with every lane zero: two `long long` zeros, read as the type. All
    /// bits zero is zero in a lane of each type here.
    fn sse_zero(&mut self, ty: TypeId, span: Span) -> ExprId {
        let wide = self.types.int(IntKind::LongLong);
        let zero = self.constant(Const::Int(0), wide, span);
        let two = self.types.vector(wide, 2);
        let zero = self.conv().to_type(zero, two);
        self.sse_as(zero, ty)
    }

    /// `__builtin_shufflevector(lhs, rhs, picks...)`, with an answer of `ty`.
    fn sse_pick(
        &mut self,
        lhs: ExprId,
        rhs: ExprId,
        picks: &[i128],
        ty: TypeId,
        span: Span,
    ) -> ExprId {
        let int = self.types.int(IntKind::Int);
        let picks: Vec<ExprId> =
            picks.iter().map(|&pick| self.constant(Const::Int(pick), int, span)).collect();
        let operands = self.tast.add_expr_refs(&[&[lhs, rhs][..], &picks].concat());
        let node = ExprKind::ShuffleVector { operands };
        self.tast.expr(Expr::new(node, ty, Category::Rvalue), span)
    }

    /// The immediate operand, which has to be an integer constant.
    fn sse_immediate(&mut self, name: &str, value: ExprId, span: Span) -> Option<i128> {
        if let Ok(value) = self.eval_integer(value) {
            return Some(value);
        }
        let what = format!("the last argument of '{name}' must be an 8-bit immediate");
        self.report(Diagnostic::error(what, span).with_code("E0715"));
        None
    }

    /// The names whose lanes come from an immediate: `pshufd`, `shufps`, and the byte shifts.
    fn sse_constant_pick(
        &mut self,
        op: Op,
        operands: &[ExprId],
        imm: i128,
        ty: TypeId,
        span: Span,
    ) -> ExprId {
        let field = |at: u32| (imm >> (2 * at)) & 3;
        match op {
            Op::Pshufd => {
                // The second operand is never picked from. It is a zero and not the first
                // operand again, so the first is read once.
                let zero = self.sse_zero(ty, span);
                let picks = [field(0), field(1), field(2), field(3)];
                self.sse_pick(operands[0], zero, &picks, ty, span)
            }
            Op::Shufps => {
                let picks = [field(0), field(1), 4 + field(2), 4 + field(3)];
                self.sse_pick(operands[0], operands[1], &picks, ty, span)
            }
            Op::BytesLeft | Op::BytesRight | Op::Palignr => {
                let bytes = self.sse_vector(Lane::I8);
                let shift = imm.clamp(0, 8 * 32) / 8;
                let mut sources: Vec<ExprId> =
                    operands.iter().map(|&operand| self.sse_as(operand, bytes)).collect();
                let picks: Vec<i128> = match op {
                    // Byte `i` is byte `i - shift` of the operand, or zero below that.
                    Op::BytesLeft => {
                        (0..16).map(|i| if i < shift { 16 } else { i - shift }).collect()
                    }
                    // Byte `i` is byte `i + shift` of the operand, or zero past its end.
                    Op::BytesRight => {
                        (0..16).map(|i| if i + shift < 16 { i + shift } else { 16 }).collect()
                    }
                    // The second operand is the low half and the first the high half, and the
                    // answer is 16 bytes of the two from byte `shift` on, with zero past the end.
                    _ if shift <= 16 => {
                        sources.reverse();
                        (0..16).map(|i| i + shift).collect()
                    }
                    _ => {
                        sources.truncate(1);
                        (0..16).map(|i| if i + shift < 32 { i + shift - 16 } else { 16 }).collect()
                    }
                };
                if sources.len() == 1 {
                    let zero = self.sse_zero(bytes, span);
                    sources.push(zero);
                }
                let answer = self.sse_pick(sources[0], sources[1], &picks, bytes, span);
                self.sse_as(answer, ty)
            }
            _ => unreachable!("not a name with an immediate"),
        }
    }

    /// A shift of each lane by a count, with zero where the count is the lane width or more, and
    /// with zeros in from the left for a shift to the right.
    #[expect(clippy::too_many_arguments, reason = "the call and both forms of the count")]
    fn sse_shift(
        &mut self,
        name: &str,
        lane: Lane,
        left: bool,
        value: ExprId,
        count: ExprId,
        written: ast::ExprId,
        ty: TypeId,
        span: Span,
    ) -> ExprId {
        let (unsigned, width) = match lane {
            Lane::I16 => (IntKind::UShort, 16),
            Lane::I32 => (IntKind::UInt, 32),
            _ => (IntKind::ULongLong, 64),
        };
        let element = self.types.int(unsigned);
        let lanes = 128 / width;
        let wide = self.types.vector(element, u32::try_from(lanes).unwrap_or(0));
        let value = self.sse_as(value, wide);
        let op = if left { BinaryOp::Shl } else { BinaryOp::Shr };
        let answer = match self.eval_integer(count) {
            Ok(shift) if (0..width).contains(&shift) => {
                let int = self.types.int(IntKind::Int);
                let shift = self.constant(Const::Int(shift), int, span);
                self.shift(op, value, shift, span)
            }
            Ok(_) => self.sse_zero(wide, span),
            Err(_) => {
                if !plain(self.ast, written) {
                    return self.sse_read_twice(name, 2, span);
                }
                let shifted = self.shift(op, value, count, span);
                // The count again, for the test. It is a name or a constant, so this reads the
                // same value and does nothing else.
                let again = self.expr(written);
                let again = self.value(again);
                let uint = self.types.int(IntKind::UInt);
                let again = self.conv().to_type(again, uint);
                let limit = self.constant(Const::Int(width), uint, span);
                let test = self.comparison(BinaryOp::Lt, again, limit, span);
                let cond = self.condition(test, span);
                let zero = self.sse_zero(wide, span);
                let node = ExprKind::Cond { cond, then: shifted, otherwise: zero };
                self.tast.expr(Expr::new(node, wide, Category::Rvalue), span)
            }
        };
        self.sse_as(answer, ty)
    }

    /// `pshufb`: byte `i` of the answer is the byte of `table` that the low four bits of byte `i`
    /// of `index` name, or zero when the top bit of that byte is set.
    fn sse_pshufb(
        &mut self,
        name: &str,
        table: ExprId,
        index: ExprId,
        written: ast::ExprId,
        ty: TypeId,
        span: Span,
    ) -> ExprId {
        if !plain(self.ast, written) {
            return self.sse_read_twice(name, 2, span);
        }
        // `__builtin_shuffle` reads only the low four bits of each index for 16 lanes.
        let node = ExprKind::Shuffle { lhs: table, rhs: None, mask: index };
        let picked = self.tast.expr(Expr::new(node, ty, Category::Rvalue), span);
        // The indices again, for the test of the top bit. They are a name or a constant, so this
        // reads the same value and does nothing else.
        let again = self.expr(written);
        let again = self.value(again);
        let signed = self.types.int(IntKind::SChar);
        let signed = self.types.vector(signed, 16);
        let again = self.sse_as(again, signed);
        let int = self.types.int(IntKind::Int);
        let zero = self.constant(Const::Int(0), int, span);
        let keep = self.comparison(BinaryOp::Ge, again, zero, span);
        let keep = self.sse_as(keep, ty);
        self.arithmetic_binary(BinaryOp::BitAnd, picked, keep, true, span)
    }

    /// The refusal of an operand that would have to be read twice and is not plain.
    fn sse_read_twice(&mut self, name: &str, at: usize, span: Span) -> ExprId {
        let what = format!(
            "argument {at} of '{name}' must be a name, a constant, or a cast of one when it is \
             not a constant"
        );
        let note = "rucc reads this argument twice, and only those forms read the same value \
                    again and do nothing else";
        self.report(Diagnostic::error(what, span).with_code("E0715").note(note, span));
        self.poison(span)
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Each name is a row of the table with no signature, since it is answered and not called,
    /// and it is on x86 only.
    #[test]
    fn every_name_is_a_row_of_the_table_and_is_answered_not_called() {
        for &(name, ..) in FAMILY {
            let row = rucc_gnu::lookup(Kind::Builtin, name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(row.status, Status::Implemented, "{name}");
            assert!(row.signature.is_empty(), "{name}");
            assert!(row.library.is_empty(), "{name}");
            assert!(!row.targets.is_empty(), "{name}");
        }
    }

    /// Each pick list picks lanes that are there: one list for each lane of the answer, and
    /// each index less than the lanes of the two operands together.
    #[test]
    fn every_pick_is_a_lane_of_the_two_operands() {
        for &(name, lane, op) in FAMILY {
            let Op::Pick(picks) = op else { continue };
            let lanes = match lane {
                Lane::F32 | Lane::I32 => 4,
                Lane::I64 => 2,
                Lane::I16 => 8,
                Lane::I8 => 16,
            };
            assert_eq!(picks.len(), lanes, "{name}");
            assert!(picks.iter().all(|&pick| (0..2 * lanes as i128).contains(&pick)), "{name}");
        }
    }
}
