//! The operators that name a type rather than take a value.
//!
//! Design: `spec/07-types-and-semantics.md` sections 7.2 and 7.4.
//!
//! A cast, `sizeof`, `alignof`, `_Generic`, `offsetof`, `va_arg` and the two `__builtin` forms
//! that take type names. They are here rather than beside the other operators because they have
//! almost nothing in common with those: each of them asks the type builder a question first, and
//! most of them answer with a constant rather than with something to be computed at run time.
//!
//! Every wording below was measured against gcc 13.3 on x86-64 Linux rather than recalled, which
//! matters more here than usual because these are the messages a configure script reads.
//!
//! # What is folded and what is not
//!
//! `sizeof`, `alignof`, `offsetof` and `__builtin_types_compatible_p` are constants, so what
//! they leave in the tree is a number and the operand is gone. That is not an optimization, it
//! is what the language says they are: `int a[sizeof(int)];` is a fixed array and not one whose
//! bound has to be worked out later.
//!
//! The exception is `sizeof` of a variable length array, which is a computation, and the
//! computation is the array's own size expression rather than a fresh one. The node is shared
//! with the array's type, which is the point: C evaluates a variable length array's size once,
//! where it was declared, and `sizeof` reads what was stored. A walk to the IR that emits the
//! expression again at each `sizeof` would call whatever the bound calls a second time.
//!
//! # What is not here yet
//!
//! `va_arg` is here but does not check what it is handed, since the type to check against is
//! `__builtin_va_list` and this compiler has no builtin declarations yet.

use rucc_ast::{self as ast, Designator};
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{
    ArrayLen, FloatKind, Layout, LayoutError, RecordId, TypeId, TypeKind, compatible,
    is_arithmetic, is_complete, is_floating, is_function, is_integer, is_pointer, is_record,
    is_void, layout,
};

use crate::check::Checker;
use crate::decl::InitEntry;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::tast::Const;

/// Which of the two measurements is being asked for.
///
/// One type rather than two functions because the rules are the same rule with one word changed
/// in each message, and gcc changes that word to `__alignof__` even where the program wrote
/// `_Alignof`, which is the sort of detail that gets lost when the two are written apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Measure {
    /// `sizeof`.
    Size,
    /// `alignof`, `_Alignof` and GNU's `__alignof__`.
    Align,
}

impl Measure {
    /// What gcc calls the operator in a message about it.
    fn as_str(self) -> &'static str {
        match self {
            Measure::Size => "sizeof",
            // Not `_Alignof`. gcc words every one of these after the GNU spelling whatever the
            // program wrote, and a build log that greps for the message wants what gcc printed.
            Measure::Align => "__alignof__",
        }
    }

    /// Which half of a layout the operator answers with.
    fn of(self, layout: Layout) -> u64 {
        match self {
            Measure::Size => layout.size,
            Measure::Align => layout.align,
        }
    }
}

impl Checker<'_> {
    /// `(ty)operand`, which converts a value and is not a way to reinterpret an object.
    pub(super) fn cast(&mut self, ty: ast::TypeNameId, operand: ast::ExprId, span: Span) -> ExprId {
        // The type first, so that a mistake in it is reported even where the operand has one
        // too. The two are independent and a program with both wants to hear about both.
        let target = self.type_name(ty);
        let operand = self.expr(operand);
        let operand = self.value(operand);
        if self.is_poisoned(operand) {
            return self.poison(span);
        }
        let from = self.tast[operand].ty;
        // A cast to a union builds an object rather than converting a value, so it leaves before
        // the conversions get a look at it. A cast of a union to its own type is an ordinary one
        // that does nothing, which is why the two types are compared before taking this way out.
        if self.is_union(target) && !compatible(&self.types, target, from) {
            return self.union_cast(target, operand, span);
        }
        if !self.castable(target, from, span) {
            return self.poison(span);
        }
        if is_void(&self.types, target) {
            // A cast to `void` is a value being discarded, which the tree already has a node
            // for, and writing a second kind of node meaning the same thing would leave every
            // reader asking which one it had.
            return self.conv().to_void(operand);
        }
        self.cast_warnings(target, self.tast[operand].ty, span);
        self.tast.expr(Expr::new(ExprKind::Cast(operand), target, Category::Rvalue), span)
    }

    /// Whether a value of `from` may be written as one of `target`, with the reason where not.
    fn castable(&mut self, target: TypeId, from: TypeId, span: Span) -> bool {
        match self.types.kind(self.types.canonical(target)) {
            TypeKind::Array { .. } => return self.bad_cast("cast specifies array type", span),
            TypeKind::Function(_) => return self.bad_cast("cast specifies function type", span),
            TypeKind::Void => return true,
            _ => {}
        }
        if is_record(&self.types, target) {
            // gcc accepts a cast of a record to its own type, which does nothing and which ISO
            // C does not have. A cast to a union of some other type is GNU's and has already
            // been taken care of, so what is left here is a cast nobody has a meaning for.
            if compatible(&self.types, target, from) {
                return true;
            }
            return self.bad_cast("conversion to non-scalar type requested", span);
        }
        if is_record(&self.types, from) {
            let word =
                if is_floating(&self.types, target) { "a floating-point" } else { "an integer" };
            return self.bad_cast(&format!("aggregate value used where {word} was expected"), span);
        }
        if is_pointer(&self.types, target)
            && !is_integer(&self.types, from)
            && !is_pointer(&self.types, from)
        {
            // An integer or another pointer, and nothing else. A floating value is the case
            // this catches, and it is the one gcc spells this way too.
            return self.bad_cast("cannot convert to a pointer type", span);
        }
        if is_floating(&self.types, target) && is_pointer(&self.types, from) {
            return self.bad_cast("pointer value used where a floating-point was expected", span);
        }
        if !is_arithmetic(&self.types, target) && !is_pointer(&self.types, target) {
            return self.bad_cast("conversion to non-scalar type requested", span);
        }
        true
    }

    /// `(union U)x`, GNU's cast to a union, which builds a union holding `x` in the member that
    /// has its type.
    ///
    /// ISO C has no such cast and gcc warns about it under `-pedantic`, which this does not say
    /// yet because there is no such option to answer to. What it does say is the message for a
    /// type no member has, which is an error in every mode.
    ///
    /// The member is found by type and the qualifiers are not part of the search, so a `const`
    /// member takes a value that is not one. A bit-field member is skipped, since there is no
    /// way to write one that gcc will find and a value cast into one would be truncated by the
    /// width rather than converted.
    fn union_cast(&mut self, target: TypeId, operand: ExprId, span: Span) -> ExprId {
        let from = self.types.unqualified(self.tast[operand].ty);
        let TypeKind::Record(record) = self.types.kind(self.types.canonical(target)) else {
            return self.poison(span);
        };
        // Copied out because finding the member asks the type table for the unqualified form of
        // each member's type, which it cannot answer while the member list is borrowed from it.
        // An incomplete union has no members and so lands on the message below, which is what
        // gcc says about it too.
        let fields = self.types.record_info(record).fields.to_vec();
        let mut found = None;
        for field in fields {
            let ty = self.types.unqualified(field.ty);
            if field.bits.is_none() && compatible(&self.types, ty, from) {
                found = Some(field);
                break;
            }
        }
        let Some(field) = found else {
            self.report(
                Diagnostic::error("cast to union type from type not present in union", span)
                    .with_code("E0647"),
            );
            return self.poison(span);
        };
        let entries = self.tast.add_init_entries(&[InitEntry::at(field.byte_offset(), operand)]);
        let decl = self.literal_decl(target, entries, span);
        self.tast.expr(Expr::new(ExprKind::CompoundLiteral(decl), target, Category::Rvalue), span)
    }

    /// Whether a record type is a `union`, which is the only one a value can be cast to.
    fn is_union(&self, ty: TypeId) -> bool {
        let TypeKind::Record(id) = self.types.kind(self.types.canonical(ty)) else { return false };
        self.types.record_info(id).kind == rucc_types::RecordKind::Union
    }

    /// The two warnings a cast that is allowed still gets, which are about the width.
    ///
    /// A pointer and an integer of different sizes is almost always a mistake and is the one
    /// gcc warns about by default, since the value does not survive the round trip.
    fn cast_warnings(&mut self, target: TypeId, from: TypeId, span: Span) {
        let pointer = u64::from(self.cx.target.pointer_width);
        let width = |ty| layout(&self.types, ty, self.cx.target).map(|l| l.size * 8).ok();
        let (message, code) = if is_pointer(&self.types, from) && is_integer(&self.types, target) {
            if width(target) == Some(pointer) {
                return;
            }
            ("cast from pointer to integer of different size", "E0567")
        } else if is_integer(&self.types, from) && is_pointer(&self.types, target) {
            if width(from) == Some(pointer) {
                return;
            }
            ("cast to pointer from integer of different size", "E0568")
        } else {
            return;
        };
        self.report(Diagnostic::warning(message.to_string(), span).with_code(code));
    }

    /// Reports a cast that is not one and answers that it was refused.
    fn bad_cast(&mut self, message: &str, span: Span) -> bool {
        self.report(Diagnostic::error(message.to_string(), span).with_code("E0569"));
        false
    }

    /// `sizeof operand` and `__alignof__ operand`, whose operand is not evaluated.
    pub(super) fn measure_expr(
        &mut self,
        operand: ast::ExprId,
        what: Measure,
        span: Span,
    ) -> ExprId {
        // Not `value`: the whole point of `sizeof a` on an array is that the array does not
        // decay, and a function does not decay under it either. The operand is still checked,
        // because `sizeof (1/0)` is a diagnostic about the division whether or not the value
        // is ever wanted.
        let operand = self.expr(operand);
        if self.is_poisoned(operand) {
            return self.poison(span);
        }
        if what == Measure::Size && self.tast[operand].category == Category::Bitfield {
            self.report(
                Diagnostic::error("'sizeof' applied to a bit-field".to_string(), span)
                    .with_code("E0570"),
            );
            return self.poison(span);
        }
        let ty = self.tast[operand].ty;
        self.measure(ty, what, span)
    }

    /// `sizeof (ty)` and `_Alignof (ty)`.
    pub(super) fn measure_type(
        &mut self,
        ty: ast::TypeNameId,
        what: Measure,
        span: Span,
    ) -> ExprId {
        let ty = self.type_name(ty);
        self.measure(ty, what, span)
    }

    /// What either operator answers for a type, which is a constant except for one case.
    fn measure(&mut self, ty: TypeId, what: Measure, span: Span) -> ExprId {
        if what == Measure::Size && self.is_variable_length(ty) {
            return match self.size_expr(ty, span) {
                Some(size) => size,
                None => self.poison(span),
            };
        }
        // An array's alignment is its element's, which is the answer for a variable length one
        // as well even though it has no size to speak of.
        let measured = match what {
            Measure::Align => layout(&self.types, self.element_of(ty), self.cx.target),
            Measure::Size => layout(&self.types, ty, self.cx.target),
        };
        let value = match measured {
            Ok(layout) => what.of(layout),
            // GNU C gives `void` and a function type the value one so that `p + 1` on a `void *`
            // and on a function pointer means what everyone who writes it means. ISO C has no
            // answer at all, which is why this is a warning rather than silence.
            Err(LayoutError::Incomplete) if is_void(&self.types, ty) => {
                self.measure_warning(what, "a void type", span);
                1
            }
            Err(LayoutError::Function) => {
                self.measure_warning(what, "a function type", span);
                1
            }
            Err(LayoutError::Incomplete) => {
                let spelled = self.spell(ty);
                self.report(
                    Diagnostic::error(
                        format!(
                            "invalid application of '{}' to incomplete type '{spelled}'",
                            what.as_str()
                        ),
                        span,
                    )
                    .with_code("E0571"),
                );
                return self.poison(span);
            }
            Err(LayoutError::TooLarge) => {
                let spelled = self.spell(ty);
                self.report(
                    Diagnostic::error(format!("type '{spelled}' is too large"), span)
                        .with_code("E0560"),
                );
                return self.poison(span);
            }
        };
        let size = self.size_type();
        self.constant(Const::Int(i128::from(value)), size, span)
    }

    /// The warning for a type that has no size and is measured all the same.
    fn measure_warning(&mut self, what: Measure, subject: &str, span: Span) {
        self.report(
            Diagnostic::warning(
                format!("invalid application of '{}' to {subject}", what.as_str()),
                span,
            )
            .with_code("E0572"),
        );
    }

    /// The size of a variable length array, as the expression that computes it.
    ///
    /// The count is the array's own size expression rather than a copy, since C evaluates it
    /// once where the array was declared and every `sizeof` after that reads what was stored.
    fn size_expr(&mut self, ty: TypeId, span: Span) -> Option<ExprId> {
        let TypeKind::Array { elem, len: ArrayLen::Variable(vla) } =
            self.types.kind(self.types.canonical(ty))
        else {
            let measured = layout(&self.types, ty, self.cx.target).ok()?;
            let size = self.size_type();
            return Some(self.constant(Const::Int(i128::from(measured.size)), size, span));
        };
        let elem = self.size_expr(elem, span)?;
        let count = self.tast.vla_size(vla);
        let size = self.size_type();
        let count = self.conv().to_type(count, size);
        let node = ExprKind::Binary { op: ast::BinaryOp::Mul, lhs: count, rhs: elem };
        Some(self.tast.expr(Expr::new(node, size, Category::Rvalue), span))
    }

    /// The type whose alignment an array's is, which is its element's however deep it goes.
    fn element_of(&self, ty: TypeId) -> TypeId {
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Array { elem, .. } => self.element_of(elem),
            _ => ty,
        }
    }

    /// `_Generic(control, ...)`, which chooses an expression by the type of another.
    pub(super) fn generic(
        &mut self,
        control: ast::ExprId,
        assocs: ast::GenericList,
        span: Span,
    ) -> ExprId {
        // The controlling expression is never evaluated and its type is the one it has after
        // the lvalue conversion, which is why `_Generic(a, int *: ...)` matches an `int[4]` and
        // why a `const int` matches `int`.
        let control = self.expr(control);
        let control = self.value(control);
        let controlling = self.tast[control].ty;

        let mut chosen = None;
        let mut fallback = None;
        let mut seen: Vec<TypeId> = Vec::new();
        for index in 0..self.ast[assocs].len() {
            let assoc = self.ast[assocs][index];
            // Every association is checked, chosen or not, because each of them is an
            // expression the program wrote and a constraint it breaks is one it broke.
            let value = self.expr(assoc.value);
            let Some(name) = assoc.ty else {
                if fallback.is_some() {
                    self.report(
                        Diagnostic::error(
                            "duplicate 'default' case in '_Generic'".to_string(),
                            span,
                        )
                        .with_code("E0573"),
                    );
                    continue;
                }
                fallback = Some(value);
                continue;
            };
            let ty = self.type_name(name);
            if !self.generic_assoc_type(ty, span) {
                continue;
            }
            if seen.iter().any(|&other| compatible(&self.types, other, ty)) {
                self.report(
                    Diagnostic::error(
                        "'_Generic' specifies two compatible types".to_string(),
                        span,
                    )
                    .with_code("E0574"),
                );
                continue;
            }
            seen.push(ty);
            if chosen.is_none() && compatible(&self.types, controlling, ty) {
                chosen = Some(value);
            }
        }
        match chosen.or(fallback) {
            Some(value) => value,
            None => {
                let spelled = self.spell(controlling);
                self.report(
                    Diagnostic::error(
                        format!(
                            "'_Generic' selector of type '{spelled}' is not compatible with any \
                             association"
                        ),
                        span,
                    )
                    .with_code("E0575"),
                );
                self.poison(span)
            }
        }
    }

    /// Whether an association names a type an association may name.
    fn generic_assoc_type(&mut self, ty: TypeId, span: Span) -> bool {
        if is_function(&self.types, ty) {
            self.report(
                Diagnostic::error("'_Generic' association has function type".to_string(), span)
                    .with_code("E0576"),
            );
            return false;
        }
        if !is_complete(&self.types, ty) {
            self.report(
                Diagnostic::error("'_Generic' association has incomplete type".to_string(), span)
                    .with_code("E0577"),
            );
            return false;
        }
        true
    }

    /// `__builtin_offsetof(ty, path)`, which is a constant and not an address.
    pub(super) fn offset_of(
        &mut self,
        ty: ast::TypeNameId,
        path: ast::DesignatorList,
        span: Span,
    ) -> ExprId {
        let mut ty = self.type_name(ty);
        let mut offset = 0u64;
        for index in 0..self.ast[path].len() {
            let step = self.ast[path][index];
            let Some((next, bytes)) = self.offset_step(ty, step, span) else {
                return self.poison(span);
            };
            ty = next;
            offset += bytes;
        }
        let size = self.size_type();
        self.constant(Const::Int(i128::from(offset)), size, span)
    }

    /// One step of an offset path: the type it reaches and what it adds to the offset.
    fn offset_step(&mut self, ty: TypeId, step: Designator, span: Span) -> Option<(TypeId, u64)> {
        match step {
            Designator::Field(name) | Designator::ObsoleteField(name) => {
                self.offset_field(ty, name, span)
            }
            Designator::Index(expr) => {
                let TypeKind::Array { elem, .. } = self.types.kind(self.types.canonical(ty)) else {
                    let spelled = self.spell(ty);
                    self.report(
                        Diagnostic::error(
                            format!(
                                "subscripted value is neither array nor pointer, but '{spelled}'"
                            ),
                            span,
                        )
                        .with_code("E0578"),
                    );
                    return None;
                };
                let expr = self.expr(expr);
                let index = self.eval_integer(expr).ok()?;
                let size = layout(&self.types, elem, self.cx.target).ok()?.size;
                Some((elem, size * u64::try_from(index).unwrap_or(0)))
            }
            // A range designates more than one element, so there is no one offset to answer
            // with. It is legal in an initializer and nowhere near an `offsetof`.
            Designator::Range { .. } => {
                self.report(
                    Diagnostic::error("a range is not a member designator".to_string(), span)
                        .with_code("E0579"),
                );
                None
            }
        }
    }

    /// The member step of an offset path, which is where the record rules are.
    fn offset_field(&mut self, ty: TypeId, name: Symbol, span: Span) -> Option<(TypeId, u64)> {
        let TypeKind::Record(record) = self.types.kind(self.types.canonical(ty)) else {
            let name = self.text(name).to_owned();
            self.report(
                Diagnostic::error(
                    format!("request for member '{name}' in something not a structure or union"),
                    span,
                )
                .with_code("E0502"),
            );
            return None;
        };
        if !is_complete(&self.types, ty) {
            let spelled = self.spell(ty);
            self.report(
                Diagnostic::error(format!("invalid use of undefined type '{spelled}'"), span)
                    .with_code("E0503"),
            );
            return None;
        }
        let Some(path) = self.find_field(record, name) else {
            let (spelled, name) = (self.spell(ty), self.text(name).to_owned());
            self.report(
                Diagnostic::error(format!("'{spelled}' has no member named '{name}'"), span)
                    .with_code("E0502"),
            );
            return None;
        };
        self.offset_chain(record, &path, span)
    }

    /// The offset of a member reached through however many anonymous members hold it.
    fn offset_chain(
        &mut self,
        record: RecordId,
        path: &[u32],
        span: Span,
    ) -> Option<(TypeId, u64)> {
        let mut record = record;
        let mut offset = 0;
        let mut ty = self.types.record(record);
        for (step, &index) in path.iter().enumerate() {
            let field = self.types.record_info(record).fields[index as usize];
            if field.is_bit_field() {
                let name = match field.name {
                    Some(name) => format!(" '{}'", self.text(name)),
                    None => String::new(),
                };
                self.report(
                    Diagnostic::error(
                        format!("attempt to take address of bit-field structure member{name}"),
                        span,
                    )
                    .with_code("E0580"),
                );
                return None;
            }
            offset += field.byte_offset();
            ty = field.ty;
            if step + 1 < path.len() {
                let TypeKind::Record(inner) = self.types.kind(self.types.canonical(ty)) else {
                    unreachable!("a member path only goes through records");
                };
                record = inner;
            }
        }
        Some((ty, offset))
    }

    /// `__builtin_types_compatible_p(a, b)`, which is a constant the preprocessor cannot ask.
    pub(super) fn types_compatible(
        &mut self,
        a: ast::TypeNameId,
        b: ast::TypeNameId,
        span: Span,
    ) -> ExprId {
        let a = self.type_name(a);
        let b = self.type_name(b);
        // The top level qualifiers come off, which is what gcc documents and what makes
        // `__builtin_types_compatible_p(const int, int)` answer one.
        let a = self.types.unqualified(a);
        let b = self.types.unqualified(b);
        let same = compatible(&self.types, a, b);
        let int = self.int();
        self.constant(Const::Int(i128::from(same)), int, span)
    }

    /// `__builtin_choose_expr(cond, then, otherwise)`, which is a conditional made of types.
    ///
    /// The arm not taken is not checked at all, which is the whole reason this exists rather
    /// than being written as `cond ? then : otherwise`: the idiom it is for has one arm that
    /// would not compile for the type the other arm is there to handle.
    pub(super) fn choose_expr(
        &mut self,
        cond: ast::ExprId,
        then: ast::ExprId,
        otherwise: ast::ExprId,
        span: Span,
    ) -> ExprId {
        let cond = self.expr(cond);
        let Ok(value) = self.eval_integer(cond) else {
            self.report(
                Diagnostic::error(
                    "first argument to '__builtin_choose_expr' not a constant".to_string(),
                    span,
                )
                .with_code("E0581"),
            );
            return self.poison(span);
        };
        // The value of the whole is the arm's own, lvalue and all, which is what lets it stand
        // on the left of an assignment the way the arm it chose would have.
        if value != 0 { self.expr(then) } else { self.expr(otherwise) }
    }

    /// `__builtin_va_arg(list, ty)`, the one of these that is not a constant.
    pub(super) fn va_arg(&mut self, list: ast::ExprId, ty: ast::TypeNameId, span: Span) -> ExprId {
        let ty = self.type_name(ty);
        let list = self.expr(list);
        let list = self.value(list);
        if self.is_poisoned(list) {
            return self.poison(span);
        }
        // What this ought to ask is whether the argument has type `va_list`, and it cannot,
        // because `va_list` is a typedef of `__builtin_va_list` and there are no builtin
        // declarations yet. A pointer is what every target's `va_list` becomes once it has
        // decayed, so that is what is asked for in the meantime.
        if !is_pointer(&self.types, self.tast[list].ty) {
            self.report(
                Diagnostic::error(
                    "first argument to 'va_arg' not of type 'va_list'".to_string(),
                    span,
                )
                .with_code("E0582"),
            );
            return self.poison(span);
        }
        if is_function(&self.types, ty) {
            let spelled = self.spell(ty);
            self.report(
                Diagnostic::error(
                    format!("second argument to 'va_arg' is a function type '{spelled}'"),
                    span,
                )
                .with_code("E0583"),
            );
            return self.poison(span);
        }
        if !is_complete(&self.types, ty) {
            let spelled = self.spell(ty);
            self.report(
                Diagnostic::error(
                    format!("second argument to 'va_arg' is of incomplete type '{spelled}'"),
                    span,
                )
                .with_code("E0584"),
            );
            return self.poison(span);
        }
        self.va_arg_promotion(ty, span);
        self.tast.expr(Expr::new(ExprKind::VaArg { list }, ty, Category::Rvalue), span)
    }

    /// The warning for asking for a type that could never have been passed.
    ///
    /// An argument beyond a prototype takes the default argument promotions, so nothing in the
    /// list is ever a `char` or a `float`, and asking for one reads the wrong number of bytes.
    fn va_arg_promotion(&mut self, ty: TypeId, span: Span) {
        if !is_arithmetic(&self.types, ty) {
            return;
        }
        let target = self.cx.target;
        let promoted = if self.types.canonical(ty) == self.types.float(FloatKind::Float) {
            self.types.float(FloatKind::Double)
        } else {
            rucc_types::promote(&mut self.types, ty, target)
        };
        if promoted == ty {
            return;
        }
        let (from, to) = (self.spell(ty), self.spell(promoted));
        self.report(
            Diagnostic::warning(
                format!("'{from}' is promoted to '{to}' when passed through '...'"),
                span,
            )
            .with_code("E0585"),
        );
    }
}

/// The operators that name a type, against the constants they fold to and the tree they leave.
#[cfg(test)]
mod tests {
    use rucc_ast::{ArraySize, BuiltinSet, Derived, GenericAssoc, Quals, TypeSpec};
    use rucc_types::{FieldDecl, FunctionType, IntKind};

    use super::*;
    use crate::check::expr::tests::{Fixture, dump, message, messages, record};
    use crate::scope::{Tag, TagKind};

    /// A `struct` laid out and bound to its tag, so that a type name can name it.
    fn tagged(checker: &mut Checker<'_>, tag: Symbol, fields: &[FieldDecl]) -> TypeId {
        let ty = record(checker, Some(tag), fields);
        checker.scopes.declare_tag(tag, Tag { kind: TagKind::Struct, ty });
        ty
    }

    /// A `union` laid out and bound to its tag, which is what a cast to a union needs.
    fn union_of(checker: &mut Checker<'_>, tag: Symbol, fields: &[FieldDecl]) -> TypeId {
        let id = checker.types.declare_record(rucc_types::RecordKind::Union, Some(tag));
        let ty = checker.types.record(id);
        let laid_out = rucc_types::layout_record(
            &checker.types,
            rucc_types::RecordKind::Union,
            fields,
            &rucc_types::RecordOptions::default(),
            checker.cx.target,
        )
        .expect("a layout");
        checker.types.complete_record(id, laid_out);
        checker.scopes.declare_tag(tag, Tag { kind: TagKind::Union, ty });
        ty
    }

    /// A type name naming a tag some other declaration defined.
    fn tag_name(fixture: &mut Fixture, kind: ast::RecordKind, tag: Symbol) -> ast::TypeNameId {
        let specs = fixture.specs(TypeSpec::Record {
            kind,
            tag: Some(tag),
            fields: None,
            attrs: rucc_ast::AttrList::EMPTY,
        });
        fixture.type_name(specs, &[])
    }

    /// A pointer with no qualifiers on it.
    fn pointer() -> Derived {
        Derived::Pointer { quals: Quals::NONE, attrs: rucc_ast::AttrList::EMPTY }
    }

    /// A prototype that takes nothing, which is the declarator step that makes a function type.
    fn call(fixture: &mut Fixture) -> Derived {
        let params = fixture.ast.add_param_list(&[]);
        Derived::Function { params, variadic: false, kind: ast::ParamKind::Void }
    }

    /// A fixed array bound.
    fn fixed(fixture: &mut Fixture, count: u128) -> Derived {
        let size = fixture.int(count, IntKind::Int);
        Derived::Array { size: ArraySize::Expr(size), quals: Quals::NONE, has_static: false }
    }

    /// A type name made of keywords and however many declarator steps.
    fn named(
        fixture: &mut Fixture,
        written: &[BuiltinSet],
        derived: &[Derived],
    ) -> ast::TypeNameId {
        let specs = fixture.keywords(written);
        fixture.type_name(specs, derived)
    }

    /// `int`, as a type name, which is what most of these are cast to and measured.
    fn int_name(fixture: &mut Fixture) -> ast::TypeNameId {
        named(fixture, &[BuiltinSet::INT], &[])
    }

    /// A `sizeof` or an `_Alignof` of a type name.
    fn measure_of(fixture: &mut Fixture, ty: ast::TypeNameId, what: Measure) -> ast::ExprId {
        let node = match what {
            Measure::Size => ast::Expr::SizeofType(ty),
            Measure::Align => ast::Expr::AlignofType(ty),
        };
        fixture.expr(node)
    }

    /// The value a folded constant node holds.
    fn folded(checker: &Checker<'_>, id: ExprId) -> i128 {
        let ExprKind::Const(value) = checker.tast[id].kind else {
            panic!("a constant, got {:?}", checker.tast[id].kind);
        };
        let Const::Int(value) = checker.tast[value] else { panic!("an integer constant") };
        value
    }

    /// How the type of a checked node is written.
    fn typed(checker: &Checker<'_>, id: ExprId) -> String {
        checker.spell(checker.tast[id].ty)
    }

    #[test]
    fn a_cast_is_a_node_of_its_own_because_the_program_asked_for_it() {
        let mut f = Fixture::new();
        let one = f.one();
        let long = named(&mut f, &[BuiltinSet::LONG], &[]);
        let cast = f.expr(ast::Expr::Cast { ty: long, operand: one });

        let mut c = f.checker();
        let id = c.check_expr(cast);

        assert_eq!(dump(&c, id), "cast : long\n  const 1 : int\n");
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_cast_to_void_is_the_value_discarded_and_not_a_second_kind_of_node() {
        let mut f = Fixture::new();
        let one = f.one();
        let void = named(&mut f, &[BuiltinSet::VOID], &[]);
        let cast = f.expr(ast::Expr::Cast { ty: void, operand: one });

        let mut c = f.checker();
        let id = c.check_expr(cast);

        assert_eq!(dump(&c, id), "convert void : void\n  const 1 : int\n");
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_cast_to_a_type_no_value_can_have_says_which_type_it_was() {
        let mut f = Fixture::new();
        let one = f.one();
        let three = fixed(&mut f, 3);
        let call = call(&mut f);
        let specs = f.keywords(&[BuiltinSet::INT]);
        let array = f.type_name(specs, &[three]);
        let function = f.type_name(specs, &[call]);
        let to_array = f.expr(ast::Expr::Cast { ty: array, operand: one });
        let to_function = f.expr(ast::Expr::Cast { ty: function, operand: one });

        let mut c = f.checker();
        c.check_expr(to_array);
        c.check_expr(to_function);

        assert_eq!(messages(&c), ["cast specifies array type", "cast specifies function type"]);
    }

    #[test]
    fn a_pointer_casts_to_another_pointer_and_a_floating_value_casts_to_neither() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let use_p = f.expr(ast::Expr::Name(p));
        let one_point_five = f.float("1.5");
        let specs = f.keywords(&[BuiltinSet::CHAR]);
        let to_char_pointer = f.type_name(specs, &[pointer()]);
        let other_specs = f.keywords(&[BuiltinSet::CHAR]);
        let again = f.type_name(other_specs, &[pointer()]);
        let repointed = f.expr(ast::Expr::Cast { ty: to_char_pointer, operand: use_p });
        let from_floating = f.expr(ast::Expr::Cast { ty: again, operand: one_point_five });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let ty = c.types.pointer(int);
        c.declare_object(p, ty, Span::DUMMY);
        let id = c.check_expr(repointed);
        c.check_expr(from_floating);

        // No warning about the width, because the two are the same width by construction.
        assert_eq!(
            dump(&c, id),
            "cast : char *\n  convert lvalue : int *\n    decl #0 p : int * lvalue\n"
        );
        assert_eq!(messages(&c), ["cannot convert to a pointer type"]);
    }

    #[test]
    fn a_cast_that_meets_an_aggregate_says_which_side_of_it_was_wrong() {
        let mut f = Fixture::new();
        let s = f.name("s");
        let x = f.name("x");
        let use_s = f.expr(ast::Expr::Name(s));
        let one = f.one();
        let tag = f.name("S");
        let int_name = int_name(&mut f);
        let record_specs = f.specs(TypeSpec::Record {
            kind: ast::RecordKind::Struct,
            tag: Some(tag),
            fields: None,
            attrs: rucc_ast::AttrList::EMPTY,
        });
        let record_name = f.type_name(record_specs, &[]);
        let from_aggregate = f.expr(ast::Expr::Cast { ty: int_name, operand: use_s });
        let to_aggregate = f.expr(ast::Expr::Cast { ty: record_name, operand: one });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let ty = tagged(&mut c, tag, &[FieldDecl::new(Some(x), int)]);
        c.declare_object(s, ty, Span::DUMMY);
        c.check_expr(from_aggregate);
        c.check_expr(to_aggregate);

        assert_eq!(
            messages(&c),
            [
                "aggregate value used where an integer was expected",
                "conversion to non-scalar type requested",
            ]
        );
    }

    #[test]
    fn a_cast_of_a_record_to_its_own_type_is_allowed_and_does_nothing() {
        let mut f = Fixture::new();
        let s = f.name("s");
        let x = f.name("x");
        let use_s = f.expr(ast::Expr::Name(s));
        let tag = f.name("S");
        let specs = f.specs(TypeSpec::Record {
            kind: ast::RecordKind::Struct,
            tag: Some(tag),
            fields: None,
            attrs: rucc_ast::AttrList::EMPTY,
        });
        let name = f.type_name(specs, &[]);
        let cast = f.expr(ast::Expr::Cast { ty: name, operand: use_s });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let ty = tagged(&mut c, tag, &[FieldDecl::new(Some(x), int)]);
        c.declare_object(s, ty, Span::DUMMY);
        let id = c.check_expr(cast);

        assert_eq!(typed(&c, id), "struct S");
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_cast_to_a_union_builds_the_object_rather_than_converting_the_value() {
        let mut f = Fixture::new();
        let x = f.name("x");
        let i = f.name("i");
        let d = f.name("d");
        let use_x = f.expr(ast::Expr::Name(x));
        let tag = f.name("U");
        let name = tag_name(&mut f, ast::RecordKind::Union, tag);
        let cast = f.expr(ast::Expr::Cast { ty: name, operand: use_x });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let double = c.types.float(FloatKind::Double);
        let ty =
            union_of(&mut c, tag, &[FieldDecl::new(Some(i), int), FieldDecl::new(Some(d), double)]);
        c.declare_object(x, int, Span::DUMMY);
        let id = c.check_expr(cast);

        assert_eq!(c.tast[id].ty, ty);
        assert_eq!(
            dump(&c, id),
            "compound-literal #1 : union U\n  decl #1 : union U object static defined\n    \
             init\n      +0\n        convert lvalue : int\n          decl #0 x : int lvalue\n"
        );
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_cast_to_a_union_no_member_of_which_has_the_type_says_so() {
        let mut f = Fixture::new();
        let x = f.name("x");
        let i = f.name("i");
        let use_x = f.expr(ast::Expr::Name(x));
        let tag = f.name("U");
        let name = tag_name(&mut f, ast::RecordKind::Union, tag);
        let cast = f.expr(ast::Expr::Cast { ty: name, operand: use_x });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let long = c.types.int(IntKind::Long);
        union_of(&mut c, tag, &[FieldDecl::new(Some(i), int)]);
        c.declare_object(x, long, Span::DUMMY);
        let id = c.check_expr(cast);

        assert_eq!(messages(&c), ["cast to union type from type not present in union"]);
        assert!(c.is_poisoned(id));
    }

    #[test]
    fn a_cast_of_a_union_to_its_own_type_is_an_ordinary_cast() {
        let mut f = Fixture::new();
        let u = f.name("u");
        let i = f.name("i");
        let use_u = f.expr(ast::Expr::Name(u));
        let tag = f.name("U");
        let name = tag_name(&mut f, ast::RecordKind::Union, tag);
        let cast = f.expr(ast::Expr::Cast { ty: name, operand: use_u });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let ty = union_of(&mut c, tag, &[FieldDecl::new(Some(i), int)]);
        c.declare_object(u, ty, Span::DUMMY);
        let id = c.check_expr(cast);

        assert!(matches!(c.tast[id].kind, ExprKind::Cast(_)));
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_cast_to_a_union_finds_a_member_whose_type_is_qualified_and_skips_a_bit_field() {
        let mut f = Fixture::new();
        let x = f.name("x");
        let i = f.name("i");
        let j = f.name("j");
        let use_x = f.expr(ast::Expr::Name(x));
        let tag = f.name("U");
        let name = tag_name(&mut f, ast::RecordKind::Union, tag);
        let cast = f.expr(ast::Expr::Cast { ty: name, operand: use_x });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let konst = c.types.qualified(int, rucc_types::Qualifiers::CONST);
        let fields = [FieldDecl::bit_field(Some(i), int, 3), FieldDecl::new(Some(j), konst)];
        let ty = union_of(&mut c, tag, &fields);
        c.declare_object(x, int, Span::DUMMY);
        let id = c.check_expr(cast);

        assert_eq!(c.tast[id].ty, ty);
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn a_cast_between_a_pointer_and_an_integer_is_measured_by_the_width() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let use_p = f.expr(ast::Expr::Name(p));
        let again = f.expr(ast::Expr::Name(p));
        let one = f.one();
        let int_name = int_name(&mut f);
        let long = named(&mut f, &[BuiltinSet::LONG], &[]);
        let specs = f.keywords(&[BuiltinSet::INT]);
        let to_pointer = f.type_name(specs, &[pointer()]);
        let narrow = f.expr(ast::Expr::Cast { ty: int_name, operand: use_p });
        let wide = f.expr(ast::Expr::Cast { ty: long, operand: again });
        let back = f.expr(ast::Expr::Cast { ty: to_pointer, operand: one });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let ty = c.types.pointer(int);
        c.declare_object(p, ty, Span::DUMMY);
        c.check_expr(narrow);
        c.check_expr(wide);
        c.check_expr(back);

        // The one that fits says nothing, which is the whole point of measuring rather than
        // warning about every cast that crosses between the two.
        assert_eq!(
            messages(&c),
            [
                "cast from pointer to integer of different size",
                "cast to pointer from integer of different size",
            ]
        );
    }

    #[test]
    fn sizeof_is_a_constant_of_the_type_the_target_measures_lengths_in() {
        let mut f = Fixture::new();
        let ty = int_name(&mut f);
        let size = measure_of(&mut f, ty, Measure::Size);

        let mut c = f.checker();
        let id = c.check_expr(size);

        assert_eq!(folded(&c, id), 4);
        assert_eq!(typed(&c, id), "unsigned long");
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn sizeof_an_expression_neither_reads_it_nor_lets_an_array_decay() {
        let mut f = Fixture::new();
        let a = f.name("a");
        let use_a = f.expr(ast::Expr::Name(a));
        let size = f.expr(ast::Expr::SizeofExpr(use_a));

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let ty = c.types.array(int, ArrayLen::Fixed(4));
        c.declare_object(a, ty, Span::DUMMY);
        let id = c.check_expr(size);

        // Sixteen and not eight, which is the difference between measuring the array and
        // measuring the pointer it would have become anywhere else.
        assert_eq!(folded(&c, id), 16);
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn sizeof_a_bit_field_is_refused_and_alignof_one_is_not() {
        let mut f = Fixture::new();
        let s = f.name("s");
        let b = f.name("b");
        let base = f.expr(ast::Expr::Name(s));
        let member = f.expr(ast::Expr::Member { base, name: b, arrow: false });
        let size = f.expr(ast::Expr::SizeofExpr(member));
        let again = f.expr(ast::Expr::Name(s));
        let member = f.expr(ast::Expr::Member { base: again, name: b, arrow: false });
        let align = f.expr(ast::Expr::AlignofExpr(member));

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let field = FieldDecl { name: Some(b), ty: int, bits: Some(3), align: None, packed: false };
        let ty = record(&mut c, None, &[field]);
        c.declare_object(s, ty, Span::DUMMY);
        c.check_expr(size);
        let aligned = c.check_expr(align);

        assert_eq!(message(&c), "'sizeof' applied to a bit-field");
        assert_eq!(folded(&c, aligned), 4);
    }

    #[test]
    fn a_type_with_no_size_is_measured_as_one_and_said_to_be_wrong() {
        let mut f = Fixture::new();
        let void = named(&mut f, &[BuiltinSet::VOID], &[]);
        let size = measure_of(&mut f, void, Measure::Size);
        let call = call(&mut f);
        let specs = f.keywords(&[BuiltinSet::VOID]);
        let function = f.type_name(specs, &[call]);
        let of_function = measure_of(&mut f, function, Measure::Size);

        let mut c = f.checker();
        let void_size = c.check_expr(size);
        let function_size = c.check_expr(of_function);

        // GNU C gives both the value one, so that `p + 1` on a `void *` and on a function
        // pointer means what everyone who writes it means.
        assert_eq!(folded(&c, void_size), 1);
        assert_eq!(folded(&c, function_size), 1);
        assert_eq!(
            messages(&c),
            [
                "invalid application of 'sizeof' to a void type",
                "invalid application of 'sizeof' to a function type",
            ]
        );
    }

    #[test]
    fn a_type_with_no_definition_is_refused_and_the_message_names_the_operator() {
        let mut f = Fixture::new();
        let tag = f.name("S");
        let specs = f.specs(TypeSpec::Record {
            kind: ast::RecordKind::Struct,
            tag: Some(tag),
            fields: None,
            attrs: rucc_ast::AttrList::EMPTY,
        });
        let name = f.type_name(specs, &[]);
        let size = measure_of(&mut f, name, Measure::Size);
        let align = measure_of(&mut f, name, Measure::Align);

        let mut c = f.checker();
        c.check_expr(size);
        c.check_expr(align);

        // gcc words the second one after the GNU spelling whatever the program wrote, and a
        // build log that greps for the message wants what gcc printed.
        assert_eq!(
            messages(&c),
            [
                "invalid application of 'sizeof' to incomplete type 'struct S'",
                "invalid application of '__alignof__' to incomplete type 'struct S'",
            ]
        );
    }

    #[test]
    fn sizeof_a_variable_length_array_is_the_size_it_was_declared_with() {
        let mut f = Fixture::new();
        let n = f.name("n");
        let count = f.expr(ast::Expr::Name(n));
        let specs = f.keywords(&[BuiltinSet::INT]);
        let bound =
            Derived::Array { size: ArraySize::Expr(count), quals: Quals::NONE, has_static: false };
        let ty = f.type_name(specs, &[bound]);
        let size = measure_of(&mut f, ty, Measure::Size);
        let align = measure_of(&mut f, ty, Measure::Align);

        let mut c = f.checker();
        // A variably modified type is only allowed inside a function, so this is one.
        c.scopes.push();
        let int = c.types.int(IntKind::Int);
        c.declare_object(n, int, Span::DUMMY);
        let measured = c.check_expr(size);
        let aligned = c.check_expr(align);

        assert_eq!(messages(&c), Vec::<String>::new());
        assert_eq!(
            dump(&c, measured),
            "binary * : unsigned long\n  convert arithmetic : unsigned long\n    convert lvalue : \
             int\n      decl #0 n : int lvalue\n  const 4 : unsigned long\n"
        );
        // An array's alignment is its element's, which is an answer even where its size is not.
        assert_eq!(folded(&c, aligned), 4);
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn generic_chooses_by_the_type_the_controlling_expression_has_after_its_conversions() {
        let mut f = Fixture::new();
        let a = f.name("a");
        let control = f.expr(ast::Expr::Name(a));
        let one = f.int(1, IntKind::Int);
        let two = f.int(2, IntKind::Int);
        let specs = f.keywords(&[BuiltinSet::INT]);
        let to_pointer = f.type_name(specs, &[pointer()]);
        let plain = f.type_name(specs, &[]);
        let assocs = f.ast.add_generic_list(&[
            GenericAssoc { ty: Some(plain), value: one },
            GenericAssoc { ty: Some(to_pointer), value: two },
        ]);
        let generic = f.expr(ast::Expr::Generic { control, assocs });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let ty = c.types.array(int, ArrayLen::Fixed(4));
        c.declare_object(a, ty, Span::DUMMY);
        let id = c.check_expr(generic);

        // The array decayed before it was matched, which is why an `int[4]` selects `int *`.
        assert_eq!(folded(&c, id), 2);
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn generic_falls_back_to_the_default_and_says_so_where_there_is_none() {
        let mut f = Fixture::new();
        let one = f.one();
        let two = f.int(2, IntKind::Int);
        let three = f.int(3, IntKind::Int);
        let control = f.float("1.5");
        let other = f.float("1.5");
        let specs = f.keywords(&[BuiltinSet::INT]);
        let plain = f.type_name(specs, &[]);
        let with_default = f.ast.add_generic_list(&[
            GenericAssoc { ty: Some(plain), value: one },
            GenericAssoc { ty: None, value: two },
        ]);
        let without = f.ast.add_generic_list(&[GenericAssoc { ty: Some(plain), value: three }]);
        let chosen = f.expr(ast::Expr::Generic { control, assocs: with_default });
        let unmatched = f.expr(ast::Expr::Generic { control: other, assocs: without });

        let mut c = f.checker();
        let id = c.check_expr(chosen);
        c.check_expr(unmatched);

        assert_eq!(folded(&c, id), 2);
        assert_eq!(
            message(&c),
            "'_Generic' selector of type 'double' is not compatible with any association"
        );
    }

    #[test]
    fn generic_refuses_two_associations_that_could_both_match_and_two_defaults() {
        let mut f = Fixture::new();
        let control = f.one();
        let other = f.one();
        let one = f.int(1, IntKind::Int);
        let two = f.int(2, IntKind::Int);
        let three = f.int(3, IntKind::Int);
        let four = f.int(4, IntKind::Int);
        let specs = f.keywords(&[BuiltinSet::INT]);
        let plain = f.type_name(specs, &[]);
        let again = f.type_name(specs, &[]);
        let twice = f.ast.add_generic_list(&[
            GenericAssoc { ty: Some(plain), value: one },
            GenericAssoc { ty: Some(again), value: two },
        ]);
        let defaults = f.ast.add_generic_list(&[
            GenericAssoc { ty: None, value: three },
            GenericAssoc { ty: None, value: four },
        ]);
        let compatible = f.expr(ast::Expr::Generic { control, assocs: twice });
        let duplicated = f.expr(ast::Expr::Generic { control: other, assocs: defaults });

        let mut c = f.checker();
        let first = c.check_expr(compatible);
        let second = c.check_expr(duplicated);

        assert_eq!(
            messages(&c),
            ["'_Generic' specifies two compatible types", "duplicate 'default' case in '_Generic'",]
        );
        // The first of each is what the expression means, so that one mistake in a selection
        // does not poison every use of what it selected.
        assert_eq!(folded(&c, first), 1);
        assert_eq!(folded(&c, second), 3);
    }

    #[test]
    fn an_association_that_could_not_be_a_value_is_refused_where_it_is_written() {
        let mut f = Fixture::new();
        let control = f.one();
        let one = f.int(1, IntKind::Int);
        let two = f.int(2, IntKind::Int);
        let three = f.int(3, IntKind::Int);
        let tag = f.name("S");
        let record_specs = f.specs(TypeSpec::Record {
            kind: ast::RecordKind::Struct,
            tag: Some(tag),
            fields: None,
            attrs: rucc_ast::AttrList::EMPTY,
        });
        let incomplete = f.type_name(record_specs, &[]);
        let call = call(&mut f);
        let specs = f.keywords(&[BuiltinSet::VOID]);
        let function = f.type_name(specs, &[call]);
        let assocs = f.ast.add_generic_list(&[
            GenericAssoc { ty: Some(incomplete), value: one },
            GenericAssoc { ty: Some(function), value: two },
            GenericAssoc { ty: None, value: three },
        ]);
        let generic = f.expr(ast::Expr::Generic { control, assocs });

        let mut c = f.checker();
        let id = c.check_expr(generic);

        assert_eq!(
            messages(&c),
            [
                "'_Generic' association has incomplete type",
                "'_Generic' association has function type",
            ]
        );
        assert_eq!(folded(&c, id), 3);
    }

    #[test]
    fn offsetof_is_a_byte_offset_and_reaches_through_an_anonymous_member() {
        let mut f = Fixture::new();
        let tag = f.name("S");
        let x = f.name("x");
        let y = f.name("y");
        let specs = f.specs(TypeSpec::Record {
            kind: ast::RecordKind::Struct,
            tag: Some(tag),
            fields: None,
            attrs: rucc_ast::AttrList::EMPTY,
        });
        let name = f.type_name(specs, &[]);
        let path = f.ast.add_designator_list(&[Designator::Field(y)]);
        let offset = f.expr(ast::Expr::Offsetof { ty: name, path });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let inner = record(&mut c, None, &[FieldDecl::new(Some(y), int)]);
        let fields = [FieldDecl::new(Some(x), int), FieldDecl::new(None, inner)];
        tagged(&mut c, tag, &fields);
        let id = c.check_expr(offset);

        assert_eq!(folded(&c, id), 4);
        assert_eq!(typed(&c, id), "unsigned long");
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn offsetof_walks_a_path_of_members_and_subscripts() {
        let mut f = Fixture::new();
        let tag = f.name("S");
        let inner_tag = f.name("T");
        let a = f.name("a");
        let b = f.name("b");
        let one = f.int(1, IntKind::Int);
        let specs = f.specs(TypeSpec::Record {
            kind: ast::RecordKind::Struct,
            tag: Some(tag),
            fields: None,
            attrs: rucc_ast::AttrList::EMPTY,
        });
        let name = f.type_name(specs, &[]);
        let path = f.ast.add_designator_list(&[
            Designator::Field(a),
            Designator::Index(one),
            Designator::Field(b),
        ]);
        let offset = f.expr(ast::Expr::Offsetof { ty: name, path });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let members = [FieldDecl::new(Some(a), int), FieldDecl::new(Some(b), int)];
        let inner = tagged(&mut c, inner_tag, &members);
        let array = c.types.array(inner, ArrayLen::Fixed(3));
        tagged(&mut c, tag, &[FieldDecl::new(Some(a), array)]);
        let id = c.check_expr(offset);

        // The second element of the array, and the second member of that.
        assert_eq!(folded(&c, id), 12);
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn offsetof_says_what_was_wrong_with_the_path_rather_than_answering_zero() {
        let mut f = Fixture::new();
        let tag = f.name("S");
        let missing = f.name("nope");
        let bits = f.name("b");
        let specs = f.specs(TypeSpec::Record {
            kind: ast::RecordKind::Struct,
            tag: Some(tag),
            fields: None,
            attrs: rucc_ast::AttrList::EMPTY,
        });
        let name = f.type_name(specs, &[]);
        let absent = f.ast.add_designator_list(&[Designator::Field(missing)]);
        let bit_field = f.ast.add_designator_list(&[Designator::Field(bits)]);
        let int_name = int_name(&mut f);
        let not_a_record = f.ast.add_designator_list(&[Designator::Field(bits)]);
        let no_member = f.expr(ast::Expr::Offsetof { ty: name, path: absent });
        let of_bit_field = f.expr(ast::Expr::Offsetof { ty: name, path: bit_field });
        let of_int = f.expr(ast::Expr::Offsetof { ty: int_name, path: not_a_record });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let field =
            FieldDecl { name: Some(bits), ty: int, bits: Some(3), align: None, packed: false };
        tagged(&mut c, tag, &[field]);
        c.check_expr(no_member);
        c.check_expr(of_bit_field);
        c.check_expr(of_int);

        assert_eq!(
            messages(&c),
            [
                "'struct S' has no member named 'nope'",
                "attempt to take address of bit-field structure member 'b'",
                "request for member 'b' in something not a structure or union",
            ]
        );
    }

    #[test]
    fn types_compatible_p_is_a_constant_that_ignores_the_top_level_qualifiers() {
        let mut f = Fixture::new();
        let plain_specs = f.keywords(&[BuiltinSet::INT]);
        let plain = f.type_name(plain_specs, &[]);
        let mut qualified_specs = rucc_ast::DeclSpecs::empty(Span::DUMMY);
        qualified_specs.ty =
            TypeSpec::Builtin(rucc_ast::Builtin::NONE.add(BuiltinSet::INT).expect("int"));
        qualified_specs.quals = Quals::CONST;
        let qualified_specs = f.ast.add_specs(qualified_specs);
        let qualified = f.type_name(qualified_specs, &[]);
        let long = named(&mut f, &[BuiltinSet::LONG], &[]);
        let same = f.expr(ast::Expr::TypesCompatible { a: plain, b: qualified });
        let different = f.expr(ast::Expr::TypesCompatible { a: plain, b: long });

        let mut c = f.checker();
        let yes = c.check_expr(same);
        let no = c.check_expr(different);

        assert_eq!(folded(&c, yes), 1);
        assert_eq!(typed(&c, yes), "int");
        assert_eq!(folded(&c, no), 0);
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn choose_expr_takes_one_arm_and_never_looks_at_the_other() {
        let mut f = Fixture::new();
        let cond = f.one();
        let then = f.int(7, IntKind::Int);
        // The arm not taken would be an error anywhere else, which is the whole reason this
        // operator exists rather than being written as a conditional.
        let otherwise = f.use_name("undeclared");
        let choose = f.expr(ast::Expr::ChooseExpr { cond, then, otherwise });

        let mut c = f.checker();
        let id = c.check_expr(choose);

        assert_eq!(folded(&c, id), 7);
        assert!(messages(&c).is_empty());
    }

    #[test]
    fn choose_expr_needs_a_constant_and_says_which_argument_was_not_one() {
        let mut f = Fixture::new();
        let n = f.name("n");
        let cond = f.expr(ast::Expr::Name(n));
        let then = f.int(7, IntKind::Int);
        let otherwise = f.int(8, IntKind::Int);
        let choose = f.expr(ast::Expr::ChooseExpr { cond, then, otherwise });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        c.declare_object(n, int, Span::DUMMY);
        let id = c.check_expr(choose);

        assert_eq!(message(&c), "first argument to '__builtin_choose_expr' not a constant");
        assert!(c.is_poisoned(id));
    }

    #[test]
    fn va_arg_has_the_type_it_was_asked_for_and_warns_where_it_could_not_be_passed() {
        let mut f = Fixture::new();
        let ap = f.name("ap");
        let list = f.expr(ast::Expr::Name(ap));
        let again = f.expr(ast::Expr::Name(ap));
        let int_name = int_name(&mut f);
        let char_name = named(&mut f, &[BuiltinSet::CHAR], &[]);
        let ordinary = f.expr(ast::Expr::VaArg { list, ty: int_name });
        let promoted = f.expr(ast::Expr::VaArg { list: again, ty: char_name });

        let mut c = f.checker();
        let void = c.types.void();
        let ty = c.types.pointer(void);
        c.declare_object(ap, ty, Span::DUMMY);
        let id = c.check_expr(ordinary);
        c.check_expr(promoted);

        assert_eq!(typed(&c, id), "int");
        assert_eq!(
            dump(&c, id),
            "va-arg : int\n  convert lvalue : void *\n    decl #0 ap : void * lvalue\n"
        );
        // An argument beyond a prototype takes the default argument promotions, so nothing in
        // the list is ever a `char` and asking for one reads the wrong number of bytes.
        assert_eq!(message(&c), "'char' is promoted to 'int' when passed through '...'");
    }

    #[test]
    fn va_arg_refuses_a_list_that_is_not_one_and_a_type_with_no_size() {
        let mut f = Fixture::new();
        let n = f.name("n");
        let ap = f.name("ap");
        let not_a_list = f.expr(ast::Expr::Name(n));
        let list = f.expr(ast::Expr::Name(ap));
        let int_name = int_name(&mut f);
        let tag = f.name("S");
        let specs = f.specs(TypeSpec::Record {
            kind: ast::RecordKind::Struct,
            tag: Some(tag),
            fields: None,
            attrs: rucc_ast::AttrList::EMPTY,
        });
        let incomplete = f.type_name(specs, &[]);
        let wrong_list = f.expr(ast::Expr::VaArg { list: not_a_list, ty: int_name });
        let wrong_type = f.expr(ast::Expr::VaArg { list, ty: incomplete });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let void = c.types.void();
        let pointer = c.types.pointer(void);
        c.declare_object(n, int, Span::DUMMY);
        c.declare_object(ap, pointer, Span::DUMMY);
        c.check_expr(wrong_list);
        c.check_expr(wrong_type);

        assert_eq!(
            messages(&c),
            [
                "first argument to 'va_arg' not of type 'va_list'",
                "second argument to 'va_arg' is of incomplete type 'struct S'",
            ]
        );
    }

    #[test]
    fn a_function_type_is_measured_by_its_own_rule_and_a_signature_is_not_a_size() {
        let mut f = Fixture::new();
        let fname = f.name("f");
        let use_f = f.expr(ast::Expr::Name(fname));
        let size = f.expr(ast::Expr::SizeofExpr(use_f));

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let signature =
            FunctionType { ret: int, params: Vec::new(), variadic: false, prototyped: true };
        let ty = c.types.function(signature);
        c.declare_object(fname, ty, Span::DUMMY);
        let id = c.check_expr(size);

        // The function did not decay under `sizeof`, which is why this is the warning about a
        // function type rather than the size of a pointer.
        assert_eq!(folded(&c, id), 1);
        assert_eq!(message(&c), "invalid application of 'sizeof' to a function type");
    }
}
