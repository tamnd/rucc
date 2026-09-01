//! Folding a constant expression, which is what an array bound and a `case` label are made of.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.6.
//!
//! C has a dozen places where an expression has to have a value at translation time: the size of
//! an array, a `case` label, an enumerator, a bit-field width, `static_assert`, `alignas`, the
//! width of a `_BitInt`, and an initializer for an object with static storage duration. This is
//! the one thing that answers all of them, because a compiler with two constant folders has two
//! answers to `1 << 31` and only one of them is right.
//!
//! It folds the typed tree rather than the untyped one. That is not a detail: every conversion
//! is already a node here, so folding never has to work out that an `int` met a `long`, and the
//! width every operation happens in is on the node in front of it. The same walk over the
//! untyped tree would have to redo the conversion rules, and that is the second implementation
//! that ends up slightly wrong.
//!
//! # What it reports and what it hands back
//!
//! Two different things go wrong when a constant is wanted, and they belong to two different
//! places. A division by zero is wrong wherever it is written, so it is reported here. Not being
//! a constant at all is only wrong because of where the expression is, and gcc's messages say
//! so: `case label does not reduce to an integer constant` and `enumerator value for 'x' is not
//! an integer constant` are two sentences about one failure. So [`NotConstant`] is handed back
//! with the node that stopped it and the caller writes the sentence.
//!
//! # Arithmetic
//!
//! Integers are held the way [`Const::Int`] holds them, as the low bits of the type extended
//! into a hundred and twenty eight by its signedness, so every operation is done in [`i128`] and
//! then wrapped by the [`IntegerInfo`] of the type it happened in. Signed overflow is warned
//! about and wrapped, which is what gcc does and is the only useful thing to do: the standard
//! says the program is undefined and a person who wrote `2147483647 + 1` wants to be told.
//! Unsigned overflow is silent, because it is not overflow.
//!
//! Floating operations go to [`rucc_base::float`], which is correctly rounded and does not ask
//! the host anything. Nothing here looks at the status those return. A constant that overflows
//! to an infinity or loses a digit is still a constant and gcc says nothing about either, so the
//! flags are dropped on purpose rather than by omission.
//!
//! # What is not here
//!
//! Address constants. `&x`, a string literal, `(char *)0` and `&s.field + 3` are constants of a
//! different kind: their value is a symbol and an offset rather than a number, and the only
//! thing that can hold one is a static initializer, which is not written yet. Every one of them
//! is [`NotConstant`] for now and the caller reports it, which is the right answer everywhere
//! except in an initializer.
//!
//! Folding happens where a constant is wanted, so an expression nothing asks about is not
//! folded and the warnings below are not produced for it. `1/0;` as a statement is silent here
//! and gcc warns, and that closes as more of the compiler asks this for values.

use std::cmp::Ordering;

use rucc_ast::{BinaryOp, UnaryOp};
use rucc_base::Interner;
use rucc_base::float::{Float, Format, Status};
use rucc_diag::Diagnostic;
use rucc_target::TargetInfo;
use rucc_types::{IntegerInfo, TypeId, TypeKind, Types, float_format, integer_info, spell};

use crate::expr::{Conversion, ExprId, ExprKind};
use crate::tast::{Const, Tast};

/// Why an expression is not a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotConstant {
    /// The node the folding stopped at, which is what a diagnostic should point at. It is the
    /// subexpression and not the whole thing, so that `case 1 + f():` underlines the call.
    pub at: ExprId,
    /// Whether that node had already been diagnosed before the folding reached it.
    ///
    /// The poisoning rule of `spec/06-lexer-and-parser.md` section 6.8: a caller says nothing
    /// about one of these, because something has already been said about the same source. It is
    /// not the same as the folding having warned, which it does about a division by zero and
    /// which gcc still follows with the caller's message.
    pub poisoned: bool,
}

/// The constant folder, over one typed tree.
#[derive(Debug)]
pub struct Eval<'a> {
    tast: &'a Tast,
    types: &'a Types,
    target: &'a TargetInfo,
    names: &'a Interner,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Eval<'a> {
    /// A folder over a tree, the types it points into, and the target it is being compiled for.
    #[must_use]
    pub fn new(
        tast: &'a Tast,
        types: &'a Types,
        target: &'a TargetInfo,
        names: &'a Interner,
    ) -> Eval<'a> {
        Eval { tast, types, target, names, diagnostics: Vec::new() }
    }

    /// The value of an expression.
    ///
    /// # Errors
    ///
    /// [`NotConstant`] when the expression is not one, which is an ordinary answer rather than a
    /// failure: whether it is a diagnostic depends on where the expression was.
    pub fn constant(&mut self, expr: ExprId) -> Result<Const, NotConstant> {
        self.eval(expr)
    }

    /// The value of an expression that has to be an integer constant expression, 6.6p6.
    ///
    /// The type has to be an integer type as well as the value being one, which is what rejects
    /// `enum { a = nullptr };`: the value folds to zero and the expression is still not an
    /// integer constant expression.
    ///
    /// # Errors
    ///
    /// [`NotConstant`] when the expression is not one, or is a constant of some other type.
    pub fn integer(&mut self, expr: ExprId) -> Result<i128, NotConstant> {
        let value = self.eval(expr)?;
        let ty = self.tast[expr].ty;
        match value {
            Const::Int(value) if self.int_shape(ty).is_some() => Ok(value),
            _ => Err(self.stop(expr)),
        }
    }

    /// What the folding reported, in the order it was found.
    #[must_use]
    pub fn finish(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    /// The value of one node.
    fn eval(&mut self, expr: ExprId) -> Result<Const, NotConstant> {
        match self.tast[expr].kind {
            ExprKind::Error => Err(NotConstant { at: expr, poisoned: true }),
            ExprKind::Const(value) => Ok(self.tast[value]),
            ExprKind::Unary { op, operand } => self.unary(expr, op, operand),
            ExprKind::Binary { op, lhs, rhs } => self.binary(expr, op, lhs, rhs),
            ExprKind::Cond { cond, then, otherwise } => {
                // Only the arm that is taken is folded. `1 ? 2 : f()` is a constant and so is
                // `0 && f()` below, which is 6.6p3 saying the operands of an unevaluated
                // subexpression do not have to be constants and is what both compilers do.
                let cond = self.eval(cond)?;
                let taken = if truth(cond) { then } else { otherwise };
                self.eval(taken)
            }
            ExprKind::Cast(operand) => self.convert(expr, operand),
            ExprKind::Convert { kind: Conversion::Arithmetic | Conversion::Bool, operand } => {
                self.convert(expr, operand)
            }
            // Every other conversion is reading an object, an array or a function decaying, a
            // pointer, or a value being discarded. Each is either an address constant, which
            // waits on the static initializers, or not a constant at all: `const int n = 1; int
            // a[n];` is a variable length array in C and an error at file scope, and it is this
            // arm that makes it one.
            //
            // And with them a name, a call, a member, a subscript, an assignment, a comma, a
            // compound literal, a statement expression, a label address, a string. The comma is
            // the interesting one: it is a constant nowhere, by 6.6p3, and `enum { a = (1, 2) };`
            // is an error in gcc rather than a two.
            _ => Err(self.stop(expr)),
        }
    }

    /// A prefix operator applied to a folded operand.
    fn unary(&mut self, expr: ExprId, op: UnaryOp, operand: ExprId) -> Result<Const, NotConstant> {
        let value = self.eval(operand)?;
        match (op, value) {
            (UnaryOp::Plus, value) => Ok(value),
            (UnaryOp::Not, value) => Ok(Const::Int(i128::from(!truth(value)))),
            // `__real__` of a real operand is the operand, and `__imag__` of one is a zero of
            // the same type. The complex cases cannot arrive: there is no complex constant for
            // the operand to have folded to, so it fails above.
            (UnaryOp::Real, value) => Ok(value),
            (UnaryOp::Imag, _) => self.zero(expr),
            (UnaryOp::Minus, Const::Float(value)) => Ok(Const::Float(value.negated())),
            (UnaryOp::Minus | UnaryOp::BitNot, Const::Int(value)) => {
                let Some(info) = self.int_shape(self.tast[operand].ty) else {
                    return Err(self.stop(expr));
                };
                if matches!(op, UnaryOp::BitNot) {
                    return Ok(Const::Int(info.wrap(!value)));
                }
                // The only negation that overflows is of the least value, whose negative is one
                // past the greatest. gcc warns and wraps, and wrapping is what the hardware
                // does with the same bits.
                let negated = info.wrap(value.wrapping_neg());
                if info.signed && value == least(info) {
                    self.overflow(expr, negated);
                }
                Ok(Const::Int(negated))
            }
            // A dereference, an address, an increment or a decrement. None of them is a
            // constant, and the last two are not even allowed to appear in one.
            _ => Err(self.stop(expr)),
        }
    }

    /// A binary operator applied to folded operands.
    fn binary(
        &mut self,
        expr: ExprId,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<Const, NotConstant> {
        match op {
            BinaryOp::LogAnd | BinaryOp::LogOr => {
                let wanted = matches!(op, BinaryOp::LogOr);
                let left = self.eval(lhs)?;
                if truth(left) == wanted {
                    return Ok(Const::Int(i128::from(wanted)));
                }
                let right = self.eval(rhs)?;
                Ok(Const::Int(i128::from(truth(right))))
            }
            BinaryOp::Shl | BinaryOp::Shr => self.shift(expr, op, lhs, rhs),
            _ => {
                let left = self.eval(lhs)?;
                let right = self.eval(rhs)?;
                match (left, right) {
                    (Const::Int(left), Const::Int(right)) => {
                        // The signedness and the width come from an operand and not from the
                        // node, because a comparison has type `int` however wide the things it
                        // compared were.
                        let Some(info) = self.int_shape(self.tast[lhs].ty) else {
                            return Err(self.stop(expr));
                        };
                        self.int_binary(expr, op, left, right, info)
                    }
                    (Const::Float(left), Const::Float(right)) => {
                        self.float_binary(expr, op, left, right)
                    }
                    // The two operands of an arithmetic operator have one type by the time they
                    // are here, so a mismatched pair is pointer arithmetic or a tree that did
                    // not check. Neither has a value to give.
                    _ => Err(self.stop(expr)),
                }
            }
        }
    }

    /// A binary operator on two integers of the same type.
    fn int_binary(
        &mut self,
        expr: ExprId,
        op: BinaryOp,
        left: i128,
        right: i128,
        info: IntegerInfo,
    ) -> Result<Const, NotConstant> {
        if let Some(ordering) = compare_int(op, left, right, info) {
            return Ok(Const::Int(i128::from(ordering)));
        }
        let value = match op {
            BinaryOp::BitAnd => left & right,
            BinaryOp::BitOr => left | right,
            BinaryOp::BitXor => left ^ right,
            BinaryOp::Div | BinaryOp::Rem if right == 0 => {
                // A warning and not an error, because that is what gcc calls it, and then no
                // value, because there is not one. The caller adds what the context calls it.
                self.warn(expr, "division by zero", "E0521");
                return Err(NotConstant { at: expr, poisoned: false });
            }
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
                return self.arithmetic(expr, op, left, right, info);
            }
            // The shifts and the logical operators went elsewhere, and the comparisons were
            // answered above, so what is left is an operator with no meaning on two integers.
            _ => return Err(self.stop(expr)),
        };
        Ok(Const::Int(info.wrap(value)))
    }

    /// The four operations that can leave the range of the type they happened in, and `%`.
    fn arithmetic(
        &mut self,
        expr: ExprId,
        op: BinaryOp,
        left: i128,
        right: i128,
        info: IntegerInfo,
    ) -> Result<Const, NotConstant> {
        if !info.signed {
            let (left, right) = (left as u128, right as u128);
            let value = match op {
                BinaryOp::Add => left.wrapping_add(right),
                BinaryOp::Sub => left.wrapping_sub(right),
                BinaryOp::Mul => left.wrapping_mul(right),
                BinaryOp::Div => left / right,
                _ => left % right,
            };
            return Ok(Const::Int(info.wrap(value as i128)));
        }
        let (exact, wrapped) = match op {
            BinaryOp::Add => (left.checked_add(right), left.wrapping_add(right)),
            BinaryOp::Sub => (left.checked_sub(right), left.wrapping_sub(right)),
            BinaryOp::Mul => (left.checked_mul(right), left.wrapping_mul(right)),
            BinaryOp::Div => (left.checked_div(right), left.wrapping_div(right)),
            _ => (left.checked_rem(right), left.wrapping_rem(right)),
        };
        let value = info.wrap(wrapped);
        // The least value divided by minus one is the one signed division that leaves the range,
        // and gcc calls the remainder of the same pair an overflow too. It is right to: the
        // remainder is zero and the instruction that computes it traps exactly as the quotient
        // does, so a program that reaches either has the same problem.
        let extreme =
            matches!(op, BinaryOp::Div | BinaryOp::Rem) && right == -1 && left == least(info);
        if extreme || exact.is_none_or(|exact| !info.holds(exact)) {
            self.overflow(expr, value);
        }
        Ok(Const::Int(value))
    }

    /// A binary operator on two floating values of the same format.
    fn float_binary(
        &mut self,
        expr: ExprId,
        op: BinaryOp,
        left: Float,
        right: Float,
    ) -> Result<Const, NotConstant> {
        if let Some(ordering) = compare_float(op, left, right) {
            return Ok(Const::Int(i128::from(ordering)));
        }
        // The status is dropped on purpose. Overflowing to an infinity and dropping a digit are
        // both things a constant is allowed to do and neither compiler says a word about either.
        let (value, _) = match op {
            BinaryOp::Add => left.sum(right),
            BinaryOp::Sub => left.difference(right),
            BinaryOp::Mul => left.product(right),
            BinaryOp::Div => left.quotient(right),
            // `%` and the bitwise operators have no floating operands, so a tree with one here
            // did not check.
            _ => return Err(self.stop(expr)),
        };
        Ok(Const::Float(value))
    }

    /// `<<` or `>>`, whose operands have their own types and whose result has the left one's.
    fn shift(
        &mut self,
        expr: ExprId,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<Const, NotConstant> {
        let left = self.eval(lhs)?;
        let right = self.eval(rhs)?;
        let (Const::Int(value), Const::Int(count)) = (left, right) else {
            return Err(self.stop(expr));
        };
        let (Some(info), Some(counts)) =
            (self.int_shape(self.tast[lhs].ty), self.int_shape(self.tast[rhs].ty))
        else {
            return Err(self.stop(expr));
        };
        let side = if matches!(op, BinaryOp::Shl) { "left" } else { "right" };
        if counts.signed && count < 0 {
            self.warn(expr, format!("{side} shift count is negative"), "E0522");
            return Err(NotConstant { at: expr, poisoned: false });
        }
        // Negative counts are gone, so the bits are the magnitude whichever type they came from,
        // which is what makes a hundred and twenty eight bit unsigned count compare correctly.
        let count = count as u128;
        if count >= u128::from(info.width) {
            self.warn(expr, format!("{side} shift count >= width of type"), "E0523");
            // gcc still gives it a value, and the value is what shifting the whole width away
            // leaves: nothing, or the sign repeated when the shift was an arithmetic right one.
            let sign = matches!(op, BinaryOp::Shr) && info.signed && value < 0;
            return Ok(Const::Int(if sign { -1 } else { 0 }));
        }
        let count = count as u32;
        let value = match (op, info.signed) {
            (BinaryOp::Shr, true) => value >> count,
            (BinaryOp::Shr, false) => ((value as u128) >> count) as i128,
            // A left shift out of the range of a signed type is undefined in C and gcc folds it
            // without a word, which is the sensible answer: `1 << 31` is how a person writes the
            // sign bit and warning about it would be noise in every real program.
            _ => value.wrapping_shl(count),
        };
        Ok(Const::Int(info.wrap(value)))
    }

    /// A folded value converted to the type of the node it is under.
    fn convert(&mut self, expr: ExprId, operand: ExprId) -> Result<Const, NotConstant> {
        let value = self.eval(operand)?;
        let (from, to) = (self.tast[operand].ty, self.tast[expr].ty);
        match self.converted(value, from, to) {
            Some(value) => Ok(value),
            None => Err(self.stop(expr)),
        }
    }

    /// A value converted to `ty`, and [`None`] when `ty` is not one a number converts to.
    ///
    /// The conversion that changes the value is the caller's to warn about, not this one's:
    /// `(char)300` is silent in gcc and `char c = 300;` is not, and both of them come through
    /// here.
    fn converted(&self, value: Const, from: TypeId, to: TypeId) -> Option<Const> {
        match bare(self.types, to) {
            // Not a truncation to one bit. `(bool)2` is one and `(bool)0.5` is one, which is
            // why this is a comparison against zero and not the integer case below.
            TypeKind::Bool => Some(Const::Int(i128::from(truth(value)))),
            TypeKind::Int(_) | TypeKind::BitInt { .. } | TypeKind::Enum(_) => {
                let info = self.int_shape(to)?;
                match value {
                    Const::Int(value) => Some(Const::Int(info.wrap(value))),
                    // Out of range is undefined behaviour rather than a value, and what comes
                    // back is the nearest end of the range with a flag on it. The flag is the
                    // caller's to warn about and is why this drops it rather than reads it.
                    Const::Float(value) => {
                        Some(Const::Int(value.to_integer(info.width, info.signed).0))
                    }
                }
            }
            TypeKind::Float(kind) => {
                let format = float_format(kind, self.target);
                let (value, _) = match value {
                    Const::Float(value) => value.to_format(format),
                    Const::Int(value) => match self.int_shape(from) {
                        Some(info) if !info.signed => Float::from_unsigned(value as u128, format),
                        _ => Float::from_signed(value, format),
                    },
                };
                Some(Const::Float(value))
            }
            // A pointer, `void`, a record, a complex type. The first is an address constant and
            // the rest have no constant to be.
            _ => None,
        }
    }

    /// A zero of the type of a node, for the `__imag__` of something real.
    fn zero(&mut self, expr: ExprId) -> Result<Const, NotConstant> {
        let ty = self.tast[expr].ty;
        if self.int_shape(ty).is_some() {
            return Ok(Const::Int(0));
        }
        match self.float_shape(ty) {
            Some(format) => Ok(Const::Float(Float::zero(format, false))),
            None => Err(self.stop(expr)),
        }
    }

    /// The shape of an integer type, over the tree's own types and target.
    fn int_shape(&self, ty: TypeId) -> Option<IntegerInfo> {
        int_shape(self.types, ty, self.target)
    }

    /// The format of a real floating type, and [`None`] for anything else.
    fn float_shape(&self, ty: TypeId) -> Option<Format> {
        match bare(self.types, ty) {
            TypeKind::Float(kind) => Some(float_format(kind, self.target)),
            _ => None,
        }
    }

    /// The answer for a node that is not a constant and that nothing has been said about.
    fn stop(&self, expr: ExprId) -> NotConstant {
        NotConstant { at: expr, poisoned: false }
    }

    /// Warns that an operation left the range of the type it happened in.
    fn overflow(&mut self, expr: ExprId, value: i128) {
        let ty = spell(self.types, self.names, self.tast[expr].ty);
        let message = format!("integer overflow in expression of type '{ty}' results in '{value}'");
        self.warn(expr, message, "E0524");
    }

    /// Reports a warning about a node.
    fn warn(&mut self, expr: ExprId, message: impl Into<String>, code: &'static str) {
        let span = self.tast.expr_span(expr);
        self.diagnostics.push(Diagnostic::warning(message.into(), span).with_code(code));
    }
}

/// The shape of an integer type, and [`None`] for anything a folded constant cannot hold.
///
/// A `_BitInt` wider than a hundred and twenty eight bits is the one integer type in that second
/// group. It is refused where it is written rather than folded to a wrong answer here.
pub(crate) fn int_shape(types: &Types, ty: TypeId, target: &TargetInfo) -> Option<IntegerInfo> {
    let info = integer_info(types, ty, target)?;
    (info.width > 0 && info.width <= 128).then_some(info)
}

/// Whether a constant is true, which is a comparison against zero and not a look at the bits.
///
/// A nan is true, because it is not equal to zero, and so is a negative zero's negation of
/// itself: the test is `!= 0` and `-0.0 == 0.0`.
fn truth(value: Const) -> bool {
    match value {
        Const::Int(value) => value != 0,
        Const::Float(value) => !value.is_zero(),
    }
}

/// The result of a comparison of two integers, and [`None`] when `op` is not a comparison.
fn compare_int(op: BinaryOp, left: i128, right: i128, info: IntegerInfo) -> Option<bool> {
    let ordering = if info.signed {
        left.cmp(&right)
    } else {
        // The bits are the value for an unsigned type of any width, including the hundred and
        // twenty eight bit one whose top bit is sitting in the sign of the `i128`.
        (left as u128).cmp(&(right as u128))
    };
    holds(op, ordering)
}

/// The result of a comparison of two floating values, and [`None`] when `op` is not one.
fn compare_float(op: BinaryOp, left: Float, right: Float) -> Option<bool> {
    match left.compare(right) {
        Some(ordering) => holds(op, ordering),
        // Unordered, so one of them is a nan. Every comparison against one is false except the
        // inequality, which is the whole of why `x != x` is the test for a nan. The ordering
        // asked about first is only there to answer whether `op` is a comparison at all.
        None if holds(op, Ordering::Equal).is_some() => Some(matches!(op, BinaryOp::Ne)),
        None => None,
    }
}

/// Whether an ordering satisfies a comparison operator, and [`None`] for anything else.
fn holds(op: BinaryOp, ordering: Ordering) -> Option<bool> {
    Some(match op {
        BinaryOp::Lt => ordering.is_lt(),
        BinaryOp::Gt => ordering.is_gt(),
        BinaryOp::Le => ordering.is_le(),
        BinaryOp::Ge => ordering.is_ge(),
        BinaryOp::Eq => ordering.is_eq(),
        BinaryOp::Ne => ordering.is_ne(),
        _ => return None,
    })
}

/// The least value a signed type of this shape holds, which is meaningless for an unsigned one.
fn least(info: IntegerInfo) -> i128 {
    info.wrap(1i128 << info.width.saturating_sub(1))
}

/// What a type is once the sugar, the qualifiers and `_Atomic` are off it.
///
/// The same peel `rucc_types` does behind each of its own predicates, spelled out here because
/// this needs the kind itself rather than an answer about it.
pub(crate) fn bare(types: &Types, ty: TypeId) -> TypeKind {
    match types.kind(types.canonical(ty)) {
        TypeKind::Atomic(inner) => types.kind(types.canonical(inner)),
        other => other,
    }
}

/// A folded integer as a diagnostic writes it, which needs the type to know whether the top bit
/// is a sign or a digit.
pub(crate) fn spell_int(value: i128, info: IntegerInfo) -> String {
    if info.signed { format!("{value}") } else { format!("{}", value as u128) }
}

/// A folded value stored in an integer type of this shape, which is what the conversion leaves.
pub(crate) fn narrowed(value: Const, info: IntegerInfo) -> i128 {
    match value {
        Const::Int(value) => info.wrap(value),
        Const::Float(value) => value.to_integer(info.width, info.signed).0,
    }
}

/// A folded constant as a diagnostic writes it.
///
/// A floating value is written in hexadecimal, which is the one place the wording here is not
/// gcc's. gcc prints `1.0e+40` and printing that needs a binary to decimal conversion that this
/// compiler does not have yet, and `0x1.d6329f1c35ca5p+132` is at least the same number.
pub(crate) fn spell_const(value: Const, info: Option<IntegerInfo>) -> String {
    match value {
        Const::Int(value) => match info {
            Some(info) => spell_int(value, info),
            None => format!("{value}"),
        },
        Const::Float(value) => value.to_hex(),
    }
}

/// Whether converting a folded value to a type of this shape changes it, gcc's `-Woverflow`.
///
/// The rule is not the obvious one and is worth stating. `signed char c = 200;` and `unsigned
/// char u = -1;` both change the value and gcc warns about neither, because in each the bits are
/// all there and it is only the sign that moved, which is a different option's business.
/// `unsigned char u = 300;` is warned about, because three hundred does not fit in eight bits
/// whichever way round they are read. So the question is whether the value fits in neither
/// signedness of the target's width.
pub(crate) fn overflows(value: Const, info: IntegerInfo) -> bool {
    match value {
        Const::Int(value) => {
            !IntegerInfo::new(true, info.width).holds(value)
                && !IntegerInfo::new(false, info.width).holds(value)
        }
        // A conversion that had to saturate, which is the flag the float arithmetic raises for
        // a value out of range and for a nan. Dropping a fraction is not overflow and gcc does
        // not warn about `char c = 3.5;` either.
        Const::Float(value) => value.to_integer(info.width, info.signed).1.has(Status::INVALID),
    }
}

#[cfg(test)]
mod tests {
    use rucc_ast as ast;
    use rucc_base::float::Format;
    use rucc_diag::Span;
    use rucc_lex::{FloatConstant, FloatConstantType, IntConstant, IntConstantType, Remarks};
    use rucc_session::Std;
    use rucc_target::{TargetInfo, Triple};
    use rucc_types::IntKind;

    use super::*;
    use crate::check::{Checker, Context};

    /// The untyped tree a test folds, built by hand.
    ///
    /// The same shape as the one the checking tests use and for the same reason: the checker
    /// borrows the interner for as long as it lives, so everything a test needs to name is
    /// named before the checker exists.
    struct Fixture {
        ast: ast::Ast,
        names: Interner,
        target: TargetInfo,
    }

    impl Fixture {
        fn new() -> Fixture {
            let target =
                TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a triple"));
            Fixture { ast: ast::Ast::new(), names: Interner::new(), target }
        }

        fn expr(&mut self, expr: ast::Expr) -> ast::ExprId {
            self.ast.expr(expr, Span::DUMMY)
        }

        fn int(&mut self, value: u128, kind: IntKind) -> ast::ExprId {
            let ty = IntConstantType::Standard(kind);
            let id = self.ast.add_int(IntConstant { value, ty, remarks: Remarks::default() });
            self.expr(ast::Expr::Int(id))
        }

        /// A constant of a bit precise type, which is the one integer type that does not promote.
        fn bit_int(&mut self, value: u128, signed: bool, width: u32) -> ast::ExprId {
            let ty = IntConstantType::BitInt { signed, width };
            let id = self.ast.add_int(IntConstant { value, ty, remarks: Remarks::default() });
            self.expr(ast::Expr::Int(id))
        }

        fn double(&mut self, text: &str) -> ast::ExprId {
            let (value, _) = Float::parse(text, Format::Double).expect("a float");
            let constant = FloatConstant {
                value,
                ty: FloatConstantType::Double,
                imaginary: false,
                remarks: Remarks::default(),
            };
            let id = self.ast.add_float(constant);
            self.expr(ast::Expr::Float(id))
        }

        fn binary(&mut self, op: BinaryOp, lhs: ast::ExprId, rhs: ast::ExprId) -> ast::ExprId {
            self.expr(ast::Expr::Binary { op, lhs, rhs })
        }

        fn unary(&mut self, op: UnaryOp, operand: ast::ExprId) -> ast::ExprId {
            self.expr(ast::Expr::Unary { op, operand })
        }

        fn checker(&self) -> Checker<'_> {
            Checker::new(&self.ast, Context::new(&self.names, &self.target, Std::C23))
        }
    }

    /// The integer one expression folds to, checking it first the way the compiler would.
    fn fold(checker: &mut Checker<'_>, expr: ast::ExprId) -> Result<i128, NotConstant> {
        let id = checker.check_expr(expr);
        checker.eval_integer(id)
    }

    /// What was reported, as the messages alone.
    fn messages(checker: &Checker<'_>) -> Vec<String> {
        checker.errors.diagnostics().iter().map(|d| d.message.clone()).collect()
    }

    #[test]
    fn arithmetic_folds_to_the_value_the_program_wrote() {
        let mut f = Fixture::new();
        let (one, two, three) =
            (f.int(1, IntKind::Int), f.int(2, IntKind::Int), f.int(3, IntKind::Int));
        let sum = f.binary(BinaryOp::Add, one, two);
        let product = f.binary(BinaryOp::Mul, sum, three);

        let mut c = f.checker();
        assert_eq!(fold(&mut c, product), Ok(9));
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn signed_overflow_is_warned_about_and_wrapped() {
        let mut f = Fixture::new();
        let (big, one) = (f.int(2_147_483_647, IntKind::Int), f.int(1, IntKind::Int));
        let sum = f.binary(BinaryOp::Add, big, one);

        let mut c = f.checker();
        assert_eq!(fold(&mut c, sum), Ok(-2_147_483_648));
        assert_eq!(
            messages(&c),
            ["integer overflow in expression of type 'int' results in '-2147483648'"]
        );
    }

    #[test]
    fn unsigned_arithmetic_wraps_without_a_word_because_it_is_not_overflow() {
        let mut f = Fixture::new();
        let (big, one) = (f.int(4_294_967_295, IntKind::UInt), f.int(1, IntKind::UInt));
        let sum = f.binary(BinaryOp::Add, big, one);

        let mut c = f.checker();
        assert_eq!(fold(&mut c, sum), Ok(0));
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_bit_precise_type_overflows_in_its_own_width_and_not_in_an_int() {
        let mut f = Fixture::new();
        let (a, b) = (f.bit_int(100, true, 8), f.bit_int(100, true, 8));
        let sum = f.binary(BinaryOp::Add, a, b);

        let mut c = f.checker();
        // Two hundred is an ordinary `int` and is not a `_BitInt(8)`, and the whole point of the
        // type is that it does what it says rather than promoting out of the question.
        assert_eq!(fold(&mut c, sum), Ok(-56));
        assert_eq!(messages(&c).len(), 1, "{:?}", messages(&c));
    }

    #[test]
    fn division_by_zero_is_warned_about_and_has_no_value() {
        let mut f = Fixture::new();
        let (one, zero) = (f.int(1, IntKind::Int), f.int(0, IntKind::Int));
        let quotient = f.binary(BinaryOp::Div, one, zero);

        let mut c = f.checker();
        let folded = fold(&mut c, quotient);
        assert!(folded.is_err());
        assert!(!folded.expect_err("no value").poisoned, "the caller still names the context");
        assert_eq!(messages(&c), ["division by zero"]);
    }

    #[test]
    fn the_least_value_over_minus_one_overflows_and_so_does_its_remainder() {
        for op in [BinaryOp::Div, BinaryOp::Rem] {
            let mut f = Fixture::new();
            let (big, one) = (f.int(2_147_483_647, IntKind::Int), f.int(1, IntKind::Int));
            let negated = f.unary(UnaryOp::Minus, big);
            let least = f.binary(BinaryOp::Sub, negated, one);
            let minus_one = f.unary(UnaryOp::Minus, one);
            let divided = f.binary(op, least, minus_one);

            let mut c = f.checker();
            let expected = if matches!(op, BinaryOp::Div) { -2_147_483_648 } else { 0 };
            assert_eq!(fold(&mut c, divided), Ok(expected));
            assert_eq!(messages(&c).len(), 1, "{:?}", messages(&c));
        }
    }

    #[test]
    fn negating_the_least_value_overflows_onto_itself() {
        let mut f = Fixture::new();
        let (big, one) = (f.int(2_147_483_647, IntKind::Int), f.int(1, IntKind::Int));
        let flipped = f.unary(UnaryOp::Minus, big);
        let least = f.binary(BinaryOp::Sub, flipped, one);
        let negated = f.unary(UnaryOp::Minus, least);

        let mut c = f.checker();
        assert_eq!(fold(&mut c, negated), Ok(-2_147_483_648));
        assert_eq!(
            messages(&c),
            ["integer overflow in expression of type 'int' results in '-2147483648'"]
        );
    }

    #[test]
    fn a_shift_past_the_width_is_warned_about_and_folded_the_way_gcc_folds_it() {
        let mut f = Fixture::new();
        let (one, thirty_two) = (f.int(1, IntKind::Int), f.int(32, IntKind::Int));
        let shifted = f.binary(BinaryOp::Shl, one, thirty_two);

        let mut c = f.checker();
        assert_eq!(fold(&mut c, shifted), Ok(0));
        assert_eq!(messages(&c), ["left shift count >= width of type"]);
    }

    #[test]
    fn an_arithmetic_right_shift_past_the_width_keeps_the_sign() {
        let mut f = Fixture::new();
        let (one, forty) = (f.int(1, IntKind::Int), f.int(40, IntKind::Int));
        let minus_one = f.unary(UnaryOp::Minus, one);
        let shifted = f.binary(BinaryOp::Shr, minus_one, forty);

        let mut c = f.checker();
        // Measured: gcc 13.3 folds `-1 >> 40` to minus one and `1 >> 40` to zero, which is the
        // shift having gone as far as it can rather than the count having wrapped.
        assert_eq!(fold(&mut c, shifted), Ok(-1));
        assert_eq!(messages(&c), ["right shift count >= width of type"]);
    }

    #[test]
    fn a_negative_shift_count_is_warned_about_and_has_no_value() {
        let mut f = Fixture::new();
        let (one, two) = (f.int(1, IntKind::Int), f.int(2, IntKind::Int));
        let count = f.unary(UnaryOp::Minus, two);
        let shifted = f.binary(BinaryOp::Shl, one, count);

        let mut c = f.checker();
        assert!(fold(&mut c, shifted).is_err());
        assert_eq!(messages(&c), ["left shift count is negative"]);
    }

    #[test]
    fn a_shift_folds_in_the_width_of_its_left_operand_alone() {
        let mut f = Fixture::new();
        let (one, forty) = (f.int(1, IntKind::LongLong), f.int(40, IntKind::Int));
        let shifted = f.binary(BinaryOp::Shl, one, forty);

        let mut c = f.checker();
        // The usual arithmetic conversions do not apply to a shift, so this is a sixty four bit
        // one shifted forty places and not an `int` shifted out of existence.
        assert_eq!(fold(&mut c, shifted), Ok(1 << 40));
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn an_unsigned_comparison_reads_the_top_bit_as_a_digit() {
        let mut f = Fixture::new();
        let one = f.int(1, IntKind::UInt);
        let big = f.unary(UnaryOp::Minus, one);
        let other = f.int(1, IntKind::UInt);
        let greater = f.binary(BinaryOp::Gt, big, other);

        let mut c = f.checker();
        // `-1u` is four billion and something. Compared as a signed value it would be less than
        // one, and a compiler that folds it that way gets every unsigned bound check wrong.
        assert_eq!(fold(&mut c, greater), Ok(1));
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn short_circuiting_does_not_fold_what_the_language_did_not_evaluate() {
        let mut f = Fixture::new();
        let zero = f.int(0, IntKind::Int);
        let name = f.names.intern("x");
        let x = f.expr(ast::Expr::Name(name));
        let and = f.binary(BinaryOp::LogAnd, zero, x);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        c.declare_object(name, int, Span::DUMMY);
        assert_eq!(fold(&mut c, and), Ok(0));
        assert!(messages(&c).is_empty(), "{:?}", messages(&c));
    }

    #[test]
    fn only_the_arm_the_condition_takes_is_folded() {
        let mut f = Fixture::new();
        let (one, two) = (f.int(1, IntKind::Int), f.int(2, IntKind::Int));
        let name = f.names.intern("x");
        let x = f.expr(ast::Expr::Name(name));
        let conditional = f.expr(ast::Expr::Cond { cond: one, then: Some(two), otherwise: x });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        c.declare_object(name, int, Span::DUMMY);
        assert_eq!(fold(&mut c, conditional), Ok(2));
        assert!(messages(&c).is_empty(), "{:?}", messages(&c));
    }

    #[test]
    fn reading_an_object_is_not_a_constant_however_const_it_is() {
        let mut f = Fixture::new();
        let name = f.names.intern("n");
        let x = f.expr(ast::Expr::Name(name));

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let constant = c.types.qualified(int, rucc_types::Qualifiers::CONST);
        c.declare_object(name, constant, Span::DUMMY);
        // C says `const int n = 1; int a[n];` is a variable length array and C++ says it is not.
        // This is the arm that decides which language is being compiled.
        assert!(fold(&mut c, x).is_err());
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_comma_is_a_constant_nowhere() {
        let mut f = Fixture::new();
        let (one, two) = (f.int(1, IntKind::Int), f.int(2, IntKind::Int));
        let comma = f.expr(ast::Expr::Comma { lhs: one, rhs: two });

        let mut c = f.checker();
        // 6.6p3 lists the comma operator among the things a constant expression shall not
        // contain, and gcc refuses `enum { a = (1, 2) };` accordingly.
        assert!(fold(&mut c, comma).is_err());
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn nothing_is_said_about_an_expression_that_was_already_diagnosed() {
        let mut f = Fixture::new();
        let name = f.names.intern("undeclared");
        let x = f.expr(ast::Expr::Name(name));
        let one = f.int(1, IntKind::Int);
        let sum = f.binary(BinaryOp::Add, x, one);

        let mut c = f.checker();
        let folded = fold(&mut c, sum);
        assert!(folded.expect_err("no value").poisoned);
        assert_eq!(messages(&c).len(), 1, "the undeclared name, and nothing about the addition");
    }

    #[test]
    fn a_floating_constant_is_not_an_integer_constant_expression() {
        let mut f = Fixture::new();
        let three = f.double("3.0");

        let mut c = f.checker();
        // Exactly three and still not an integer constant expression, which is 6.6p6 being
        // about the type and not about the value. gcc refuses `enum { a = 3.0 };` too.
        let id = c.check_expr(three);
        assert!(c.eval_integer(id).is_err());
        let (three, _) = Float::parse("3.0", Format::Double).expect("a float");
        assert_eq!(c.eval_constant(id), Ok(Const::Float(three)));
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn floating_arithmetic_is_folded_in_the_target_format() {
        let mut f = Fixture::new();
        let (one, three) = (f.double("1.0"), f.double("3.0"));
        let third = f.binary(BinaryOp::Div, one, three);

        let mut c = f.checker();
        let id = c.check_expr(third);
        let Ok(Const::Float(value)) = c.eval_constant(id) else { panic!("a folded float") };
        assert_eq!(value.to_bits(), 0x3fd5_5555_5555_5555, "the correctly rounded double third");
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_comparison_against_a_nan_is_false_except_for_the_inequality() {
        for (op, expected) in [(BinaryOp::Eq, 0), (BinaryOp::Ne, 1), (BinaryOp::Lt, 0)] {
            let mut f = Fixture::new();
            let (a, b) = (f.double("0.0"), f.double("0.0"));
            let nan = f.binary(BinaryOp::Div, a, b);
            let (c1, c2) = (f.double("0.0"), f.double("0.0"));
            let other = f.binary(BinaryOp::Div, c1, c2);
            let compared = f.binary(op, nan, other);

            let mut c = f.checker();
            assert_eq!(fold(&mut c, compared), Ok(expected));
            // A floating division by zero is a nan and not a diagnostic, which is what makes
            // `0.0/0.0` a way to write one and what both compilers accept in a constant.
            assert!(messages(&c).is_empty());
        }
    }

    #[test]
    fn a_conversion_between_arithmetic_types_folds_through_the_node_the_checking_wrote() {
        let mut f = Fixture::new();
        let (half, one) = (f.double("0.5"), f.int(1, IntKind::Int));
        let sum = f.binary(BinaryOp::Add, half, one);

        let mut c = f.checker();
        let id = c.check_expr(sum);
        let Ok(Const::Float(value)) = c.eval_constant(id) else { panic!("a folded float") };
        // The `1` became a `1.0` in a conversion node, which is the whole reason the folding
        // reads the typed tree: nothing here had to work out that an int met a double.
        assert_eq!(value.to_bits(), 0x3ff8_0000_0000_0000, "one and a half, in a double");
        assert!(messages(&c).is_empty());
    }
}
