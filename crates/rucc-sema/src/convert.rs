//! The conversions the language performs without being asked, as nodes in the tree.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.2.
//!
//! Every one of these writes a [`Conversion`] node. Nothing downstream is allowed to work out
//! for itself that an `int` met a `long` somewhere, because a second place that knows the
//! conversion rules is a second place that is slightly wrong about them, and that is where the
//! sign extension bugs live.
//!
//! # The order the standard puts them in
//!
//! An expression used for its value goes through at most three steps, in this order, and the
//! order is not a convenience:
//!
//! First the lvalue conversion of 6.3.2.1, which reads the object and drops the qualifiers and
//! the atomicity, since neither is part of a value. An array and a function do not take part in
//! it at all: they decay instead, which is why `sizeof a` on an array is the array's size and
//! not a pointer's, and why the decay has to be a separate step rather than a special case of
//! reading.
//!
//! Then the integer promotions of 6.3.1.1, which are about one operand.
//!
//! Then the usual arithmetic conversions of 6.3.1.8, which are about two.
//!
//! [`Conv::value`] is the first step and is what almost every caller wants, because an operand
//! that is still an lvalue is an operand somebody forgot to read.
//!
//! # Bit-fields
//!
//! A bit-field's lvalue conversion gives the type it was declared with and its promotion is
//! decided by its width rather than by that type, which is why [`Conv::promote_bits`] exists
//! next to [`Conv::promote`]. `unsigned b:3` promotes to `int` because every three bit value
//! fits in one, and `unsigned b:32` promotes to `unsigned int` because they no longer do.
//!
//! No caller has to know that, because [`Conv::promote`] and [`Conv::usual_arithmetic`] look for
//! the width themselves. A caller that had to remember would be a caller that forgot, and the
//! symptom is a whole expression coming out unsigned on the strength of one member's declared
//! type.

use rucc_target::TargetInfo;
use rucc_types::{TypeId, TypeKind, Types, is_arithmetic, is_pointer, is_void};

use crate::expr::{Category, Conversion, Expr, ExprId, ExprKind};
use crate::tast::{Const, Tast};

/// Everything a conversion needs: the tree to write the node into and the table to ask.
///
/// Three references rather than a pass-wide context, because the conversions are the part of
/// semantic analysis with no state of its own and nothing else here should be able to reach
/// the scopes or the diagnostics through them.
#[derive(Debug)]
pub struct Conv<'a> {
    /// The tree the nodes are written into.
    pub tast: &'a mut Tast,
    /// The types, which conversions extend.
    pub types: &'a mut Types,
    /// What the target's integers are, which is what the promotions are decided by.
    pub target: &'a TargetInfo,
}

impl Conv<'_> {
    /// The value of an expression: 6.3.2.1, with the decays that replace it.
    ///
    /// An array becomes a pointer to its first element, a function becomes a pointer to itself,
    /// and everything else that is an lvalue is read. An expression that is already a value is
    /// its own answer, so this can be called on any operand without asking what it is first.
    pub fn value(&mut self, expr: ExprId) -> ExprId {
        let ty = self.tast[expr].ty;
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Array { elem, .. } => {
                let ty = self.types.pointer(elem);
                self.write(Conversion::ArrayDecay, expr, ty)
            }
            TypeKind::Function(_) => {
                let ty = self.types.pointer(ty);
                self.write(Conversion::FunctionDecay, expr, ty)
            }
            _ if self.tast[expr].category == Category::Rvalue => expr,
            _ => {
                let ty = self.read_as(ty);
                self.write(Conversion::Lvalue, expr, ty)
            }
        }
    }

    /// The value of an expression with the integer promotions applied, 6.3.1.1.
    ///
    /// Anything narrower than `int` becomes `int`, or `unsigned int` where `int` cannot hold
    /// every value it had. A floating type, a pointer and a `_BitInt` are each their own
    /// answer, the last because C23 6.3.1.1p2 says so and because that is the point of the
    /// type: it is the one integer type in C that does what it says.
    pub fn promote(&mut self, expr: ExprId) -> ExprId {
        let expr = self.value_promoting_bits(expr);
        let ty = self.tast[expr].ty;
        let promoted = rucc_types::promote(self.types, ty, self.target);
        self.arithmetic(expr, promoted)
    }

    /// The value of a bit-field with the integer promotions applied to its width.
    ///
    /// A bit-field is narrower than the type it was declared with, and it is the width that
    /// decides. The caller passes the width because the tree holds the field index and the
    /// width is a fact about the record, not about the expression.
    pub fn promote_bits(&mut self, expr: ExprId, width: u32) -> ExprId {
        let expr = self.value(expr);
        let ty = self.tast[expr].ty;
        let promoted = rucc_types::promote_bit_field(self.types, ty, width, self.target);
        self.arithmetic(expr, promoted)
    }

    /// The value of an expression, promoted by its width first where it names a bit-field.
    ///
    /// This is what every operand that is about to be promoted goes through, because the type a
    /// bit-field was declared with is not the type it brings to an operator: `unsigned b:1` is
    /// an `int` in `b + 1` and not an `unsigned int`, and a whole expression comes out signed or
    /// unsigned on the strength of that one width.
    fn value_promoting_bits(&mut self, expr: ExprId) -> ExprId {
        match self.bit_field_width(expr) {
            Some(width) => self.promote_bits(expr, width),
            None => self.value(expr),
        }
    }

    /// The width of the bit-field an expression names, or [`None`] where it names none.
    ///
    /// The width lives on the record rather than on the expression, so this asks the type table
    /// rather than reading it off the node. Only a member access can be one: a bit-field has no
    /// address, so there is no other expression that can arrive still being one.
    ///
    /// The lvalue conversion is looked through, because most callers read the object before they
    /// know they are about to promote it and the value they are left holding is still as wide as
    /// the field was.
    fn bit_field_width(&self, expr: ExprId) -> Option<u32> {
        let expr = match self.tast[expr].kind {
            ExprKind::Convert { kind: Conversion::Lvalue, operand } => operand,
            _ => expr,
        };
        let ExprKind::Member { base, field } = self.tast[expr].kind else { return None };
        let base = self.types.canonical(self.tast[base].ty);
        let TypeKind::Record(record) = self.types.kind(base) else { return None };
        self.types.record_info(record).fields.get(field as usize)?.bits
    }

    /// The usual arithmetic conversions, 6.3.1.8: both operands converted to one type.
    ///
    /// [`None`] where either operand is not arithmetic, which is not a failure of this rule but
    /// a question it does not answer, since `p + 1` is pointer arithmetic and never reaches it.
    pub fn usual_arithmetic(&mut self, lhs: ExprId, rhs: ExprId) -> Option<(ExprId, ExprId)> {
        let (lhs, rhs) = (self.value_promoting_bits(lhs), self.value_promoting_bits(rhs));
        let common = rucc_types::usual_arithmetic(
            self.types,
            self.tast[lhs].ty,
            self.tast[rhs].ty,
            self.target,
        )?;
        Some((self.arithmetic(lhs, common), self.arithmetic(rhs, common)))
    }

    /// A scalar as a condition, which is a comparison against zero and not a truncation.
    ///
    /// That is why it is [`Conversion::Bool`] rather than [`Conversion::Arithmetic`]: `(bool)
    /// 256` is true and `(char) 256` is zero, and a compiler that treats the two the same is
    /// wrong about one of them.
    pub fn to_bool(&mut self, expr: ExprId) -> ExprId {
        let expr = self.value(expr);
        let boolean = self.types.boolean();
        if self.tast[expr].ty == boolean {
            return expr;
        }
        self.write(Conversion::Bool, expr, boolean)
    }

    /// A value discarded, which is what a cast to `void` does.
    ///
    /// An expression statement does not write one of these, even though it discards a value too.
    /// The statement is what does the discarding, and a statement expression's value is the last
    /// statement's, so an expression statement that had thrown its type away would have nothing
    /// left to give.
    pub fn to_void(&mut self, expr: ExprId) -> ExprId {
        if is_void(self.types, self.tast[expr].ty) {
            return expr;
        }
        let void = self.types.void();
        self.write(Conversion::Void, expr, void)
    }

    /// A value converted to a given type, with the kind of conversion worked out from the two.
    ///
    /// This is what an assignment, an argument, a `return` and an initializer all do. It writes
    /// the conversion the pair calls for and does not judge whether the pair is allowed: the
    /// caller has the span and the wording, and a conversion that should have been diagnosed is
    /// a diagnostic the caller owes rather than a node this refuses to write.
    pub fn to_type(&mut self, expr: ExprId, ty: TypeId) -> ExprId {
        let expr = self.value(expr);
        let from = self.tast[expr].ty;
        let target = self.read_as(ty);
        if from == target {
            return expr;
        }
        if is_void(self.types, target) {
            return self.to_void(expr);
        }
        let boolean = self.types.boolean();
        if target == boolean {
            return self.to_bool(expr);
        }
        let kind = if is_pointer(self.types, target) {
            // A null pointer constant is not the integer zero converted. The constant may have
            // any integer type and `(void *)0` is one of them, so what makes it a null pointer
            // is what it says rather than what it weighs.
            if self.is_null_pointer_constant(expr) {
                Conversion::NullPointer
            } else {
                Conversion::Pointer
            }
        } else if is_arithmetic(self.types, target) && is_arithmetic(self.types, from) {
            Conversion::Arithmetic
        } else {
            // A record to a record of the same type, or anything else the caller has already
            // decided about. There is nothing to compute, so the node records that a value of
            // one type is being used as another and the verifier can see it happened.
            Conversion::Pointer
        };
        self.write(kind, expr, target)
    }

    /// Whether an expression is a null pointer constant, 6.3.2.3p3.
    ///
    /// An integer constant expression with the value zero, or such an expression cast to `void
    /// *`. The casts and the conversions are looked through because `(void *)0` is one and so
    /// is `(long)0`, and stopping at the first node would see a cast rather than a zero.
    #[must_use]
    pub fn is_null_pointer_constant(&self, expr: ExprId) -> bool {
        match self.tast[expr].kind {
            ExprKind::Const(value) => self.tast[value] == Const::Int(0),
            ExprKind::Cast(inner) | ExprKind::Convert { operand: inner, .. } => {
                self.is_null_pointer_constant(inner)
            }
            _ => false,
        }
    }

    /// Writes an arithmetic conversion, or nothing where the type is already the one wanted.
    fn arithmetic(&mut self, expr: ExprId, ty: TypeId) -> ExprId {
        if self.tast[expr].ty == ty {
            return expr;
        }
        self.write(Conversion::Arithmetic, expr, ty)
    }

    /// The type a value has once it has been read out of an object.
    ///
    /// The qualifiers and the atomicity come off, because neither is part of a value: `const
    /// int x; x + 1` has an `int` on the left of the `+` and not a `const int`.
    pub(crate) fn read_as(&mut self, ty: TypeId) -> TypeId {
        let stripped = match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Atomic(inner) => inner,
            _ => ty,
        };
        self.types.unqualified(stripped)
    }

    /// Writes one conversion node over an operand.
    fn write(&mut self, kind: Conversion, operand: ExprId, ty: TypeId) -> ExprId {
        let span = self.tast.expr_span(operand);
        let node = Expr::new(ExprKind::Convert { kind, operand }, ty, Category::Rvalue);
        self.tast.expr(node, span)
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_diag::Span;
    use rucc_target::{TargetInfo, Triple};
    use rucc_types::{ArrayLen, FunctionType, IntKind, Qualifiers};

    use super::*;
    use crate::decl::{Decl, DeclKind, DeclList, Definition, Linkage, StorageDuration};
    use crate::print::Printer;

    struct Fixture {
        tast: Tast,
        types: Types,
        names: Interner,
        target: TargetInfo,
    }

    impl Fixture {
        fn new() -> Fixture {
            let target =
                TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a triple"));
            Fixture { tast: Tast::new(), types: Types::new(), names: Interner::new(), target }
        }

        fn conv(&mut self) -> Conv<'_> {
            Conv { tast: &mut self.tast, types: &mut self.types, target: &self.target }
        }

        /// An lvalue of the given type, which is what a use of an object is.
        fn object(&mut self, ty: TypeId) -> ExprId {
            let decl = self.tast.decl(
                Decl {
                    name: None,
                    ty,
                    kind: DeclKind::Object,
                    linkage: Linkage::None,
                    duration: StorageDuration::Automatic,
                    state: Definition::Defined,
                    alignment: None,
                    constant: false,
                    retained: false,
                    init: None,
                    params: DeclList::EMPTY,
                    body: None,
                },
                Span::DUMMY,
            );
            self.tast.expr(Expr::new(ExprKind::Decl(decl), ty, Category::Lvalue), Span::DUMMY)
        }

        /// A use of the one member of a `struct` that has one, which is a bit-field of `bits`.
        fn bit_field(&mut self, ty: TypeId, bits: u32) -> ExprId {
            let fields = [rucc_types::FieldDecl::bit_field(None, ty, bits)];
            let id = self.types.declare_record(rucc_types::RecordKind::Struct, None);
            let laid_out = rucc_types::layout_record(
                &self.types,
                rucc_types::RecordKind::Struct,
                &fields,
                &rucc_types::RecordOptions::default(),
                &self.target,
            )
            .expect("a layout");
            self.types.complete_record(id, laid_out);
            let record = self.types.record(id);
            let base = self.object(record);
            self.tast.expr(
                Expr::new(ExprKind::Member { base, field: 0 }, ty, Category::Bitfield),
                Span::DUMMY,
            )
        }

        fn zero(&mut self, ty: TypeId) -> ExprId {
            let value = self.tast.add_const(Const::Int(0));
            self.tast.expr(Expr::new(ExprKind::Const(value), ty, Category::Rvalue), Span::DUMMY)
        }

        fn text(&self, expr: ExprId) -> String {
            let mut printer = Printer::new(&self.tast, &self.types, &self.names);
            printer.expr(expr);
            printer.finish()
        }
    }

    #[test]
    fn reading_an_object_drops_the_qualifiers_because_they_are_not_part_of_a_value() {
        let mut f = Fixture::new();
        let int = f.types.int(IntKind::Int);
        let constant = f.types.qualified(int, Qualifiers::CONST);
        let object = f.object(constant);
        let read = f.conv().value(object);

        assert_eq!(f.tast[read].ty, int);
        assert_eq!(f.tast[read].category, Category::Rvalue);
        assert_eq!(f.text(read), "convert lvalue : int\n  decl #0 : const int lvalue\n");
    }

    #[test]
    fn an_atomic_object_reads_as_the_type_it_wraps() {
        let mut f = Fixture::new();
        let int = f.types.int(IntKind::Int);
        let atomic = f.types.atomic(int);
        let object = f.object(atomic);
        let read = f.conv().value(object);

        assert_eq!(f.tast[read].ty, int);
    }

    #[test]
    fn an_array_decays_and_is_not_read() {
        let mut f = Fixture::new();
        let int = f.types.int(IntKind::Int);
        let array = f.types.array(int, ArrayLen::Fixed(3));
        let object = f.object(array);
        let decayed = f.conv().value(object);

        // Not an lvalue conversion, which is why `sizeof a` is the array's size: the decay is a
        // step of its own and `sizeof` is the operator that does not take it.
        assert_eq!(f.text(decayed), "convert array-decay : int *\n  decl #0 : int[3] lvalue\n");
    }

    #[test]
    fn a_function_decays_to_a_pointer_to_itself() {
        let mut f = Fixture::new();
        let void = f.types.void();
        let signature =
            FunctionType { ret: void, params: Vec::new(), variadic: false, prototyped: true };
        let function = f.types.function(signature);
        let designator =
            f.tast.expr(Expr::new(ExprKind::Error, function, Category::Function), Span::DUMMY);
        let decayed = f.conv().value(designator);

        assert_eq!(
            f.text(decayed),
            "convert function-decay : void (*)(void)\n  error : void(void) function\n"
        );
    }

    #[test]
    fn a_narrow_integer_promotes_and_an_int_does_not_move() {
        let mut f = Fixture::new();
        let char_type = f.types.int(IntKind::Char);
        let int = f.types.int(IntKind::Int);
        let narrow = f.object(char_type);
        let wide = f.object(int);

        let promoted = f.conv().promote(narrow);
        assert_eq!(f.tast[promoted].ty, int);
        assert_eq!(
            f.text(promoted),
            "convert arithmetic : int\n  convert lvalue : char\n    decl #0 : char lvalue\n"
        );

        // Nothing is written where nothing happens, so a dump has no noise in it.
        let already = f.conv().promote(wide);
        assert_eq!(f.text(already), "convert lvalue : int\n  decl #1 : int lvalue\n");
    }

    #[test]
    fn a_bit_field_promotes_by_its_width_and_not_by_its_type() {
        let mut f = Fixture::new();
        let unsigned = f.types.int(IntKind::UInt);
        let int = f.types.int(IntKind::Int);
        let three = f.object(unsigned);
        let full = f.object(unsigned);

        // Every three bit value fits in an `int`, so the promotion changes the signedness.
        let narrow = f.conv().promote_bits(three, 3);
        assert_eq!(f.tast[narrow].ty, int);
        // Thirty two bit values no longer do.
        let wide = f.conv().promote_bits(full, 32);
        assert_eq!(f.tast[wide].ty, unsigned);
    }

    #[test]
    fn a_bit_field_operand_promotes_by_its_width_without_being_asked() {
        // The width is not on the operand, so this is the one promotion that has to be found
        // rather than read off the node, and forgetting it makes `b.flag + 1` come out unsigned
        // for a one bit field. gcc says `int`, and so does 6.3.1.1p2.
        let mut f = Fixture::new();
        let unsigned = f.types.int(IntKind::UInt);
        let int = f.types.int(IntKind::Int);
        let one = f.zero(int);
        let again = f.zero(int);

        let narrow = f.bit_field(unsigned, 1);
        let (lhs, rhs) = f.conv().usual_arithmetic(narrow, one).expect("both are arithmetic");
        assert_eq!(f.tast[lhs].ty, int);
        assert_eq!(f.tast[rhs].ty, int);

        // A field as wide as the type it was declared with keeps that type, which is the case
        // that says the width is what decides and not the fact of being a bit-field.
        let full = f.bit_field(unsigned, 32);
        let (lhs, rhs) = f.conv().usual_arithmetic(full, again).expect("both are arithmetic");
        assert_eq!(f.tast[lhs].ty, unsigned);
        assert_eq!(f.tast[rhs].ty, unsigned);
    }

    #[test]
    fn a_bit_field_that_has_already_been_read_still_promotes_by_its_width() {
        // Almost every caller reads the object before it knows it is about to promote, so the
        // member is under an lvalue conversion by the time the promotion looks for it.
        let mut f = Fixture::new();
        let unsigned = f.types.int(IntKind::UInt);
        let int = f.types.int(IntKind::Int);
        let narrow = f.bit_field(unsigned, 1);
        let read = f.conv().value(narrow);

        let promoted = f.conv().promote(read);
        assert_eq!(f.tast[promoted].ty, int);
    }

    #[test]
    fn the_usual_arithmetic_conversions_move_both_sides_to_one_type() {
        let mut f = Fixture::new();
        let int = f.types.int(IntKind::Int);
        let long = f.types.int(IntKind::Long);
        let narrow = f.object(int);
        let wide = f.object(long);

        let (lhs, rhs) = f.conv().usual_arithmetic(narrow, wide).expect("both are arithmetic");
        assert_eq!(f.tast[lhs].ty, long);
        assert_eq!(f.tast[rhs].ty, long);
    }

    #[test]
    fn a_pointer_pair_has_no_usual_arithmetic_conversion() {
        let mut f = Fixture::new();
        let int = f.types.int(IntKind::Int);
        let pointer = f.types.pointer(int);
        let left = f.object(pointer);
        let right = f.object(int);

        assert!(f.conv().usual_arithmetic(left, right).is_none());
    }

    #[test]
    fn a_condition_is_a_comparison_against_zero_and_not_a_truncation() {
        let mut f = Fixture::new();
        let int = f.types.int(IntKind::Int);
        let object = f.object(int);
        let condition = f.conv().to_bool(object);

        // `(bool) 256` is true and `(char) 256` is zero, which is why this is its own kind.
        assert_eq!(
            f.text(condition),
            "convert bool : _Bool\n  convert lvalue : int\n    decl #0 : int lvalue\n"
        );
    }

    #[test]
    fn a_zero_of_any_integer_type_is_a_null_pointer_constant() {
        let mut f = Fixture::new();
        let long = f.types.int(IntKind::Long);
        let int = f.types.int(IntKind::Int);
        let pointer = f.types.pointer(int);
        let zero = f.zero(long);
        let null = f.conv().to_type(zero, pointer);

        assert_eq!(f.text(null), "convert null-pointer : int *\n  const 0 : long\n");
    }

    #[test]
    fn a_pointer_that_is_not_a_constant_zero_is_an_ordinary_pointer_conversion() {
        let mut f = Fixture::new();
        let int = f.types.int(IntKind::Int);
        let void = f.types.void();
        let from = f.types.pointer(void);
        let to = f.types.pointer(int);
        let object = f.object(from);
        let converted = f.conv().to_type(object, to);

        assert_eq!(
            f.text(converted),
            "convert pointer : int *\n  convert lvalue : void *\n    decl #0 : void * lvalue\n"
        );
    }

    #[test]
    fn converting_to_the_type_it_already_has_writes_nothing() {
        let mut f = Fixture::new();
        let int = f.types.int(IntKind::Int);
        let object = f.object(int);
        let read = f.conv().value(object);
        let again = f.conv().to_type(read, int);

        assert_eq!(read, again);
    }

    #[test]
    fn a_value_is_discarded_by_a_node_rather_than_by_being_ignored() {
        let mut f = Fixture::new();
        let int = f.types.int(IntKind::Int);
        let object = f.object(int);
        let dropped = f.conv().to_void(object);

        assert_eq!(f.text(dropped), "convert void : void\n  decl #0 : int lvalue\n");
    }
}
