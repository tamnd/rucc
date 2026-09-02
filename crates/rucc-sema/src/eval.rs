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
//! # Addresses
//!
//! `&x`, `&s.field + 3` and a string literal are constants of a different kind: their value is
//! an object and an offset rather than a number, because nothing knows where the object is
//! until the linker puts it somewhere. [`Const::Address`] is that pair, and folding one is a
//! walk down an lvalue adding up member offsets and scaled subscripts rather than a walk over
//! values, which is why it is a second function and not another arm.
//!
//! Two of the rules are worth stating because they are not the obvious ones. A pointer with no
//! object behind it is not an address at all: `(int *)4` folds to four, and so does `(int *)4 +
//! 1` once the scaling is done, which is why an enumerator may be written that way and gcc
//! accepts it. And an address cast to an integer stays an address only where every bit of it
//! survives, which is what makes `long n = (long)&a;` a static initializer on a sixty four bit
//! target and `int n = (int)&a;` not one, exactly as gcc has it.
//!
//! An address is not an integer constant expression, whatever type it is wearing. So an array
//! bound, a `case` label and an enumerator each go through [`Eval::integer`], which asks for a
//! number and gets [`NotConstant`] for any of these.
//!
//! # What is not here
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
use rucc_types::{IntegerInfo, TypeId, TypeKind, Types, float_format, integer_info, layout, spell};

use crate::decl::StorageDuration;
use crate::expr::{Conversion, ExprId, ExprKind};
use crate::tast::{Address, Base, Const, Tast};

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
            // The address of an lvalue, which is the one operator whose operand is not folded
            // to a value first, because an lvalue does not have one.
            ExprKind::Unary { op: UnaryOp::AddrOf, operand } => {
                Ok(Const::Address(self.place(operand)?))
            }
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
            ExprKind::Convert {
                kind: Conversion::Arithmetic | Conversion::Bool | Conversion::Pointer,
                operand,
            } => self.convert(expr, operand),
            // An array or a function becoming a pointer is the address of the thing itself,
            // which is why these two go to the lvalue walk rather than to a value.
            ExprKind::Convert {
                kind: Conversion::ArrayDecay | Conversion::FunctionDecay,
                operand,
            } => Ok(Const::Address(self.place(operand)?)),
            // A null pointer constant keeps the value it had, since the whole point of the
            // conversion is that the value was already zero.
            ExprKind::Convert { kind: Conversion::NullPointer, operand } => self.eval(operand),
            // What is left is reading an object, which is not a constant however `const` the
            // object is: `const int n = 1; int a[n];` is a variable length array in C, and it is
            // this arm that makes it one. And a value being discarded, a call, an assignment, a
            // comma, a statement expression, a label address, and an lvalue with no `&` in front
            // of it. The comma is the interesting one: it is a constant nowhere, by 6.6p3, and
            // `enum { a = (1, 2) };` is an error in gcc rather than a two.
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
                if self.pointee_size(self.tast[lhs].ty).is_some()
                    || self.pointee_size(self.tast[rhs].ty).is_some()
                {
                    return self.pointer_binary(expr, op, lhs, rhs, left, right);
                }
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

    /// The address of an lvalue, walked down rather than folded up.
    ///
    /// This is the half of the folding that does not have values to work with. A member adds its
    /// own offset to whatever holds it and a subscript adds its index scaled by the element, so
    /// what comes out is the object at the bottom and the distance travelled to reach it.
    fn place(&mut self, expr: ExprId) -> Result<Address, NotConstant> {
        match self.tast[expr].kind {
            ExprKind::Error => Err(NotConstant { at: expr, poisoned: true }),
            // An automatic object has no address until the frame holding it exists, so it is
            // not a constant, and neither is a parameter for the same reason.
            ExprKind::Decl(decl) | ExprKind::CompoundLiteral(decl)
                if self.tast[decl].duration != StorageDuration::Automatic =>
            {
                Ok(Address { base: Base::Decl(decl), offset: 0 })
            }
            ExprKind::Str(id) => Ok(Address { base: Base::Str(id), offset: 0 }),
            ExprKind::Member { base, field } => {
                let mut address = self.place(base)?;
                let TypeKind::Record(record) = bare(self.types, self.tast[base].ty) else {
                    return Err(self.stop(expr));
                };
                let Some(field) =
                    self.types.record_info(record).fields.get(field as usize).copied()
                else {
                    return Err(self.stop(expr));
                };
                address.offset += i128::from(field.offset);
                Ok(address)
            }
            ExprKind::Subscript { base, index } => {
                let base = self.eval(base)?;
                let Const::Int(index) = self.eval(index)? else { return Err(self.stop(expr)) };
                let size = i128::from(self.size_of(self.tast[expr].ty));
                let Const::Address(mut address) = base else { return Err(self.stop(expr)) };
                address.offset += index.wrapping_mul(size);
                Ok(address)
            }
            // `&*p` is `p`, which is what makes `int *q = &*a;` a constant and is not a
            // simplification: the dereference of an address constant is the object it names.
            ExprKind::Unary { op: UnaryOp::Deref, operand } => match self.eval(operand)? {
                Const::Address(address) => Ok(address),
                _ => Err(self.stop(expr)),
            },
            _ => Err(self.stop(expr)),
        }
    }

    /// An operator with a pointer on at least one side.
    ///
    /// Which side the pointer is on is read off the types rather than off the folded values,
    /// because `(int *)4` folds to a number and is still a pointer, and the scaling that
    /// `p + 1` does is decided by what `p` points at and not by what it happened to fold to.
    fn pointer_binary(
        &mut self,
        expr: ExprId,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
        left: Const,
        right: Const,
    ) -> Result<Const, NotConstant> {
        let (left_step, right_step) =
            (self.pointee_size(self.tast[lhs].ty), self.pointee_size(self.tast[rhs].ty));
        match (op, left_step, right_step) {
            (BinaryOp::Add, Some(step), None) => self.offset_by(expr, left, right, step),
            (BinaryOp::Add, None, Some(step)) => self.offset_by(expr, right, left, step),
            (BinaryOp::Sub, Some(step), None) => self.offset_by(expr, left, negate(right), step),
            // A difference of two pointers, which is a number and not an address however far
            // from home the two are. It needs the same object under both, since the distance
            // between two objects is not decided until they are placed.
            (BinaryOp::Sub, Some(step), Some(_)) if step != 0 => {
                let distance = match (left, right) {
                    (Const::Address(left), Const::Address(right)) if left.base == right.base => {
                        left.offset - right.offset
                    }
                    (Const::Int(left), Const::Int(right)) => left - right,
                    _ => return Err(self.stop(expr)),
                };
                Ok(Const::Int(distance / i128::from(step)))
            }
            (_, Some(_), _) | (_, _, Some(_)) => self.pointer_compare(expr, op, left, right),
            _ => Err(self.stop(expr)),
        }
    }

    /// A pointer moved by a number of elements, whichever kind of pointer it folded to.
    fn offset_by(
        &mut self,
        expr: ExprId,
        pointer: Const,
        count: Const,
        step: u64,
    ) -> Result<Const, NotConstant> {
        let Const::Int(count) = count else { return Err(self.stop(expr)) };
        let distance = count.wrapping_mul(i128::from(step));
        match pointer {
            Const::Address(address) => Ok(Const::Address(Address {
                base: address.base,
                offset: address.offset.wrapping_add(distance),
            })),
            Const::Int(value) => Ok(Const::Int(value.wrapping_add(distance))),
            Const::Float(_) => Err(self.stop(expr)),
        }
    }

    /// A comparison with a pointer on at least one side.
    fn pointer_compare(
        &mut self,
        expr: ExprId,
        op: BinaryOp,
        left: Const,
        right: Const,
    ) -> Result<Const, NotConstant> {
        let ordering = match (left, right) {
            (Const::Address(left), Const::Address(right)) if left.base == right.base => {
                left.offset.cmp(&right.offset)
            }
            // Two pointers that are both numbers, which compare as the unsigned values they are.
            (Const::Int(left), Const::Int(right)) => (left as u128).cmp(&(right as u128)),
            // An object has an address and a null pointer does not point at one, so the two are
            // never the same. Which of them was written first does not matter to `==` or `!=`,
            // and nothing else about the pair can be answered before the object is placed.
            (Const::Address(_), Const::Int(0)) | (Const::Int(0), Const::Address(_)) => {
                return match op {
                    BinaryOp::Eq => Ok(Const::Int(0)),
                    BinaryOp::Ne => Ok(Const::Int(1)),
                    _ => Err(self.stop(expr)),
                };
            }
            _ => return Err(self.stop(expr)),
        };
        match holds(op, ordering) {
            Some(value) => Ok(Const::Int(i128::from(value))),
            None => Err(self.stop(expr)),
        }
    }

    /// How far apart two elements of a pointer's target type are, and [`None`] for a non-pointer.
    ///
    /// A pointer to `void` or to a function steps by one byte, which is what GNU C says and what
    /// every program that does arithmetic on a `void *` is written against.
    fn pointee_size(&self, ty: TypeId) -> Option<u64> {
        match bare(self.types, ty) {
            TypeKind::Pointer(target) => Some(match bare(self.types, target) {
                TypeKind::Void | TypeKind::Function(_) => 1,
                _ => self.size_of(target),
            }),
            _ => None,
        }
    }

    /// The size of a type in bytes, and zero for one that has no size to give.
    fn size_of(&self, ty: TypeId) -> u64 {
        layout(self.types, ty, self.target).map_or(0, |layout| layout.size)
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
                    // An address written as a number is still an address, and it survives only
                    // where every bit of it does. That is the whole difference between gcc
                    // taking `long n = (long)&a;` as a static initializer and refusing
                    // `int n = (int)&a;`, and it is measured in bits and not in names.
                    Const::Address(address) => (u64::from(info.width) == self.size_of(from) * 8)
                        .then_some(Const::Address(address)),
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
                    // No cast turns an address into a floating value, so a tree with one here
                    // did not check.
                    Const::Address(_) => return None,
                };
                Some(Const::Float(value))
            }
            // A pointer keeps whatever it was, since a cast between pointer types moves nothing:
            // an address stays the same address and a number stays the same number.
            TypeKind::Pointer(_) => match value {
                Const::Int(_) | Const::Address(_) => Some(value),
                Const::Float(_) => None,
            },
            // `void`, a record, a complex type. None of them has a constant to be.
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
        // An object has an address and no object is at zero, so an address is always true.
        Const::Address(_) => true,
    }
}

/// A folded integer negated, for the `p - n` that is written as an offset of minus `n`.
fn negate(value: Const) -> Const {
    match value {
        Const::Int(value) => Const::Int(value.wrapping_neg()),
        other => other,
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
        // Nothing narrows an address, since the caller asked for a number and got one of these
        // instead. Zero is a value it will not use.
        Const::Address(_) => 0,
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
        Const::Address(address) => {
            let base = match address.base {
                Base::Decl(decl) => decl.index(),
                Base::Str(id) => id.index(),
            };
            format!("&#{base} + {}", address.offset)
        }
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
        // An address is as wide as a pointer or it would not have got this far, so nothing about
        // it is lost.
        Const::Address(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use rucc_ast as ast;
    use rucc_ast::{
        ArraySize, AttrList, Builtin, BuiltinSet, DeclSpecs, Declarator, Derived, Quals,
        StorageClass, TypeSpec,
    };
    use rucc_base::Symbol;
    use rucc_base::float::Format;
    use rucc_diag::Span;
    use rucc_lex::{
        Encoding, FloatConstant, FloatConstantType, IntConstant, IntConstantType, Remarks,
        StringLiteral,
    };
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

        fn name(&mut self, text: &str) -> Symbol {
            self.names.intern(text)
        }

        fn use_name(&mut self, text: &str) -> ast::ExprId {
            let name = self.name(text);
            self.expr(ast::Expr::Name(name))
        }

        fn string(&mut self, text: &str) -> ast::ExprId {
            let elements = text.chars().map(|c| c as u32).collect();
            let id = self.ast.add_string(StringLiteral {
                elements,
                encoding: Encoding::Plain,
                remarks: Remarks::default(),
            });
            self.expr(ast::Expr::Str(id))
        }

        fn subscript(&mut self, base: ast::ExprId, index: ast::ExprId) -> ast::ExprId {
            self.expr(ast::Expr::Index { base, index })
        }

        fn member(&mut self, base: ast::ExprId, field: &str) -> ast::ExprId {
            let name = self.name(field);
            self.expr(ast::Expr::Member { base, name, arrow: false })
        }

        /// One member of a record.
        fn field(&mut self, specs: DeclSpecs, name: &str) -> ast::Member {
            let declarator = Some(self.declarator(Some(name), &[]));
            let specs = self.ast.add_specs(specs);
            ast::Member::Field(ast::Field {
                specs,
                declarator,
                bits: None,
                attrs: AttrList::EMPTY,
                span: Span::DUMMY,
            })
        }

        /// `struct S { ... }`, as a specifier list.
        fn record(&mut self, tag: &str, members: &[ast::Member]) -> DeclSpecs {
            let tag = Some(self.name(tag));
            let fields = Some(self.ast.add_member_list(members));
            let mut specs = DeclSpecs::empty(Span::DUMMY);
            specs.ty = TypeSpec::Record {
                kind: ast::RecordKind::Struct,
                tag,
                fields,
                attrs: AttrList::EMPTY,
                pack: None,
            };
            specs
        }

        fn cast(
            &mut self,
            specs: DeclSpecs,
            derived: &[Derived],
            operand: ast::ExprId,
        ) -> ast::ExprId {
            let ty = self.type_name(specs, derived);
            self.expr(ast::Expr::Cast { ty, operand })
        }

        /// `int`, as a specifier list a test can add words to.
        fn int_specs(&self) -> DeclSpecs {
            self.builtin(BuiltinSet::INT)
        }

        fn builtin(&self, keyword: BuiltinSet) -> DeclSpecs {
            let mut specs = DeclSpecs::empty(Span::DUMMY);
            let builtin = Builtin::NONE.add(keyword).expect("a keyword written once");
            specs.ty = TypeSpec::Builtin(builtin);
            specs
        }

        fn type_name(&mut self, specs: DeclSpecs, derived: &[Derived]) -> ast::TypeNameId {
            let declarator = self.declarator(None, derived);
            let specs = self.ast.add_specs(specs);
            self.ast.add_type_name(ast::TypeName { specs, declarator, span: Span::DUMMY })
        }

        fn declarator(&mut self, name: Option<&str>, derived: &[Derived]) -> ast::DeclaratorId {
            let name = name.map(|name| self.name(name));
            let derived = self.ast.add_derived_list(derived);
            self.ast.add_declarator(Declarator {
                name,
                name_span: Span::DUMMY,
                derived,
                span: Span::DUMMY,
            })
        }

        /// A declaration of one name, which is what an address needs an object to be.
        fn var(&mut self, specs: DeclSpecs, name: &str, derived: &[Derived]) -> ast::DeclId {
            let declarator = self.declarator(Some(name), derived);
            let item = ast::InitDeclarator {
                declarator,
                init: None,
                asm_label: None,
                attrs: AttrList::EMPTY,
                span: Span::DUMMY,
            };
            let declarators = self.ast.add_init_declarator_list(&[item]);
            let specs = self.ast.add_specs(specs);
            self.ast.decl(ast::Decl::Var { specs, declarators }, Span::DUMMY)
        }

        fn checker(&self) -> Checker<'_> {
            Checker::new(&self.ast, Context::new(&self.names, &self.target, Std::C23))
        }
    }

    /// `[n]`, with a fixed bound.
    fn array(size: ast::ExprId) -> Derived {
        Derived::Array { size: ArraySize::Expr(size), quals: Quals::NONE, has_static: false }
    }

    /// `*`.
    fn pointer() -> Derived {
        Derived::Pointer { quals: Quals::NONE, attrs: AttrList::EMPTY }
    }

    /// What an expression folds to, whatever kind of constant that is.
    fn value(checker: &mut Checker<'_>, expr: ast::ExprId) -> Result<Const, NotConstant> {
        let id = checker.check_expr(expr);
        checker.eval_constant(id)
    }

    /// The object an address constant is into, and how far.
    fn address(value: Result<Const, NotConstant>) -> Option<(usize, i128)> {
        match value {
            Ok(Const::Address(address)) => {
                let base = match address.base {
                    Base::Decl(decl) => decl.index(),
                    Base::Str(id) => id.index(),
                };
                Some((base, address.offset))
            }
            _ => None,
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
    fn the_address_of_a_static_object_is_that_object_and_no_distance() {
        let mut f = Fixture::new();
        let object = f.var(f.int_specs(), "a", &[]);
        let a = f.use_name("a");
        let taken = f.unary(UnaryOp::AddrOf, a);

        let mut c = f.checker();
        c.check_decl(object);
        assert_eq!(address(value(&mut c, taken)), Some((0, 0)));
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_subscript_and_a_member_add_up_into_one_distance() {
        let mut f = Fixture::new();
        let x = f.int(4, IntKind::Int);
        let object = f.var(f.int_specs(), "a", &[array(x)]);
        let a = f.use_name("a");
        let two = f.int(2, IntKind::Int);
        let element = f.subscript(a, two);
        let taken = f.unary(UnaryOp::AddrOf, element);

        let mut c = f.checker();
        c.check_decl(object);
        assert_eq!(
            address(value(&mut c, taken)),
            Some((0, 8)),
            "two elements of four bytes each into the object it started at"
        );
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_member_adds_its_own_offset_to_the_object_that_holds_it() {
        let mut f = Fixture::new();
        let x = f.field(f.int_specs(), "x");
        let y = f.field(f.int_specs(), "y");
        let specs = f.record("S", &[x, y]);
        let object = f.var(specs, "s", &[]);
        let s = f.use_name("s");
        let member = f.member(s, "y");
        let taken = f.unary(UnaryOp::AddrOf, member);

        let mut c = f.checker();
        c.check_decl(object);
        assert_eq!(address(value(&mut c, taken)), Some((0, 4)));
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_pointer_moves_by_what_it_points_at_and_not_by_bytes() {
        let mut f = Fixture::new();
        let four = f.int(4, IntKind::Int);
        let object = f.var(f.int_specs(), "a", &[array(four)]);
        let a = f.use_name("a");
        let three = f.int(3, IntKind::Int);
        let moved = f.binary(BinaryOp::Add, a, three);
        let a = f.use_name("a");
        let one = f.int(1, IntKind::Int);
        let back = f.binary(BinaryOp::Sub, a, one);

        let mut c = f.checker();
        c.check_decl(object);
        assert_eq!(address(value(&mut c, moved)), Some((0, 12)));
        assert_eq!(address(value(&mut c, back)), Some((0, -4)), "and it may go the other way");
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn two_pointers_into_one_object_subtract_to_the_elements_between_them() {
        let mut f = Fixture::new();
        let ten = f.int(10, IntKind::Int);
        let object = f.var(f.int_specs(), "a", &[array(ten)]);
        let a = f.use_name("a");
        let three = f.int(3, IntKind::Int);
        let high = f.subscript(a, three);
        let high = f.unary(UnaryOp::AddrOf, high);
        let a = f.use_name("a");
        let one = f.int(1, IntKind::Int);
        let low = f.subscript(a, one);
        let low = f.unary(UnaryOp::AddrOf, low);
        let distance = f.binary(BinaryOp::Sub, high, low);

        let mut c = f.checker();
        c.check_decl(object);
        assert_eq!(
            value(&mut c, distance),
            Ok(Const::Int(2)),
            "a difference is a number, since the two cancel whatever the linker does with them"
        );
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn two_pointers_into_different_objects_have_no_distance_between_them() {
        let mut f = Fixture::new();
        let first = f.var(f.int_specs(), "a", &[]);
        let second = f.var(f.int_specs(), "b", &[]);
        let a = f.use_name("a");
        let a = f.unary(UnaryOp::AddrOf, a);
        let b = f.use_name("b");
        let b = f.unary(UnaryOp::AddrOf, b);
        let distance = f.binary(BinaryOp::Sub, a, b);

        let mut c = f.checker();
        c.check_decl(first);
        c.check_decl(second);
        assert!(value(&mut c, distance).is_err(), "nothing decides that until the two are placed");
    }

    #[test]
    fn the_address_of_an_automatic_object_is_not_a_constant() {
        let mut f = Fixture::new();
        let object = f.var(f.int_specs(), "a", &[]);
        let a = f.use_name("a");
        let taken = f.unary(UnaryOp::AddrOf, a);

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(object);
        assert!(
            value(&mut c, taken).is_err(),
            "a local has no address until the frame holding it exists"
        );
    }

    #[test]
    fn a_static_local_does_have_one_since_it_is_laid_out_once() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Static);
        let object = f.var(specs, "a", &[]);
        let a = f.use_name("a");
        let taken = f.unary(UnaryOp::AddrOf, a);

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(object);
        assert_eq!(address(value(&mut c, taken)), Some((0, 0)));
    }

    #[test]
    fn a_string_literal_is_an_object_and_its_decay_is_the_address_of_it() {
        let mut f = Fixture::new();
        let literal = f.string("hi");
        let one = f.int(1, IntKind::Int);
        let moved = f.binary(BinaryOp::Add, literal, one);

        let mut c = f.checker();
        assert_eq!(address(value(&mut c, moved)), Some((0, 1)));
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn an_address_written_as_an_integer_survives_only_where_all_of_it_does() {
        let mut f = Fixture::new();
        let object = f.var(f.int_specs(), "a", &[]);
        let a = f.use_name("a");
        let taken = f.unary(UnaryOp::AddrOf, a);
        let wide = f.cast(f.builtin(BuiltinSet::LONG), &[], taken);
        let a = f.use_name("a");
        let taken = f.unary(UnaryOp::AddrOf, a);
        let narrow = f.cast(f.int_specs(), &[], taken);

        let mut c = f.checker();
        c.check_decl(object);
        assert_eq!(
            address(value(&mut c, wide)),
            Some((0, 0)),
            "a `long` holds every bit of a pointer here, so the value is still the object"
        );
        assert!(
            value(&mut c, narrow).is_err(),
            "an `int` does not, and half an address is not an address"
        );
    }

    #[test]
    fn a_pointer_with_no_object_behind_it_is_a_number_and_stays_one() {
        let mut f = Fixture::new();
        let four = f.int(4, IntKind::Int);
        let pointer = f.cast(f.int_specs(), &[pointer()], four);
        let one = f.int(1, IntKind::Int);
        let moved = f.binary(BinaryOp::Add, pointer, one);
        let back = f.cast(f.builtin(BuiltinSet::LONG), &[], moved);

        let mut c = f.checker();
        assert_eq!(
            value(&mut c, back),
            Ok(Const::Int(8)),
            "the scaling happens and nothing has to be relocated, so it is an integer throughout"
        );
    }

    #[test]
    fn an_address_is_never_null_and_says_so() {
        let mut f = Fixture::new();
        let object = f.var(f.int_specs(), "a", &[]);
        let a = f.use_name("a");
        let taken = f.unary(UnaryOp::AddrOf, a);
        let zero = f.int(0, IntKind::Int);
        let compared = f.binary(BinaryOp::Ne, taken, zero);

        let mut c = f.checker();
        c.check_decl(object);
        assert_eq!(fold(&mut c, compared), Ok(1));
    }

    #[test]
    fn an_address_is_not_an_integer_constant_expression_whatever_type_it_wears() {
        let mut f = Fixture::new();
        let object = f.var(f.int_specs(), "a", &[]);
        let a = f.use_name("a");
        let taken = f.unary(UnaryOp::AddrOf, a);
        let wide = f.cast(f.builtin(BuiltinSet::LONG), &[], taken);

        let mut c = f.checker();
        c.check_decl(object);
        assert!(
            fold(&mut c, wide).is_err(),
            "an array bound and a case label want a number, and this is a relocation"
        );
    }

    #[test]
    fn reading_an_object_is_not_a_constant_however_const_the_object_is() {
        let mut f = Fixture::new();
        let mut specs = f.int_specs();
        specs.quals = Quals::CONST;
        let object = f.var(specs, "n", &[]);
        let n = f.use_name("n");

        let mut c = f.checker();
        c.check_decl(object);
        assert!(
            value(&mut c, n).is_err(),
            "which is the whole reason `const int n = 1; int a[n];` is a variable length array"
        );
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
