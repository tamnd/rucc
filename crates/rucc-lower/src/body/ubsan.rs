//! The undefined behaviour checks of `-fsanitize=`, which gcc calls UBSan. See
//! [`crate::unit::Sanitize`] for why they are put in by the walk.
//!
//! A check is a comparison in front of the operation, a branch to a block that tells the runtime
//! and a branch back. The runtime is libubsan in user space and `lib/ubsan.c` in the kernel, and
//! both read what they are told the way gcc lays it out: a pointer to a record that says where the
//! operator is and what the types of its operands are called, then each operand as a word. So the
//! record is laid out the way gcc lays it out, field for field, and the names are gcc's names.

use rucc_ast::{BinaryOp, UnaryOp};
use rucc_base::Symbol;
use rucc_diag::{BytePos, Span};
use rucc_ir::{
    Datum, Extra, Flags, Global, Imm, InstData, IntPred, Linkage as IrLinkage, Opcode, Reloc,
    Signature, Type, Value,
};
use rucc_sema::{Const, Conversion, DeclId, ExprId, ExprKind};
use rucc_tuple::Arch;
use rucc_types::{
    ArrayLen, FloatKind, IntKind, Qualifiers, RecordId, RecordKind, TypeId, TypeKind, VlaId,
};

use super::{Body, MemInfo, Ordered, Place, Stride, Where};
use crate::repr;
use crate::unit::{Check, Unit};

/// One operand as the check sees it: the value and the C type it has.
#[derive(Clone, Copy)]
pub(super) struct Operand {
    pub(super) value: Value,
    pub(super) ty: TypeId,
}

/// The pointer an access goes through, as the checks of `-fsanitize=null` and
/// `-fsanitize=alignment` see it.
///
/// gcc checks the pointer that the object an access names is reached through: `p` for `*p`, the
/// address of the element for `p[i]`, and the address of the whole record for `p->m`, `p[i].m`
/// and `p->a[i]`, which it calls a member access. The type is the type of what the pointer
/// points at, which is what the alignment is taken from and what the runtime is told.
#[derive(Debug, Clone, Copy)]
pub(super) struct Pointed {
    /// The pointer that is checked.
    pointer: Value,
    /// The type it points at.
    ty: TypeId,
    /// Whether the access is to a member of the record the pointer points at.
    member: bool,
    /// Where gcc says the access is: the `*`, the `[`, the `->` or the `.` of the expression
    /// that names the object, and the assignment operator for a store.
    at: BytePos,
}

/// What the runtime is told an access through a pointer was.
#[derive(Clone, Copy)]
enum Access {
    Load,
    Store,
}

/// How the length a subscript is checked against is found.
enum Length {
    /// An array of a fixed length.
    Fixed(u64),
    /// A variable length array.
    Variable(VlaId),
    /// A member that another member counts.
    Counted(Counted),
}

/// A member that `counted_by` says another member counts: the object they are members of, the
/// member itself, and the member that counts it with its type. The counter can be in an
/// anonymous member, so it is the indices that reach it from the object.
struct Counted {
    holder: ExprId,
    field: u32,
    counter: Vec<u32>,
    ty: TypeId,
}

/// One field of the record a check hands the runtime.
#[derive(Clone, Copy)]
enum Field {
    /// A place in the source: a pointer to the name of the file, then the line and the column
    /// as two numbers of four bytes. [`None`] is a place gcc does not know, which is a null
    /// name and two zeros.
    Loc(Option<BytePos>),
    /// A pointer to the descriptor of a type.
    Type(Symbol),
    /// One byte.
    Byte(u8),
    /// A number of four bytes.
    Int(u32),
}

impl Body<'_, '_> {
    /// Checks `lhs op rhs` in the type `ty` before it is done, for the checks that are on.
    ///
    /// `lhs` and `rhs` are the expressions the two operands were written as, which is how the
    /// column of the operator is found. The value of `rhs` is the count of a shift as it was
    /// written, before it is brought to the width of the other side, because that is the type the
    /// runtime is told it has. Nothing is checked in a type that is not one of the standard integer
    /// types, which is where gcc checks nothing either.
    pub(super) fn check_arithmetic(
        &mut self,
        op: BinaryOp,
        lhs: Operand,
        rhs: Operand,
        written: (ExprId, ExprId),
        span: Span,
    ) {
        if self.at.is_none() {
            return;
        }
        let Some(kind) = self.int_kind(lhs.ty) else { return };
        match op {
            BinaryOp::Div | BinaryOp::Rem => {
                // gcc makes one record for the two checks. The divisor zero is tested first, and
                // the quotient of the most negative value and minus one after it, each with what
                // its own check does when it fires. The second is made under `-fwrapv` as well.
                let divide = self.sanitize.divide;
                let overflow = if kind.is_signed(self.target().char_is_signed) {
                    self.sanitize.signed_overflow
                } else {
                    Check::Off
                };
                if !divide.is_on() && !overflow.is_on() {
                    return;
                }
                let at = self.operator(written, span);
                let ty = self.unit.int_type(kind);
                let data = self.unit.ubsan_data(at, &[ty]);
                let routine = "__ubsan_handle_divrem_overflow";
                let wide = self.func[rhs.value].ty;
                if divide.is_on() {
                    let zero = {
                        let mut build = self.build(span);
                        let zero = build.iconst(wide, 0);
                        build.icmp(IntPred::Eq, rhs.value, zero)
                    };
                    self.fire(zero, divide, routine, data, &[lhs, rhs], span);
                }
                if overflow.is_on() {
                    let fires = {
                        let mut build = self.build(span);
                        let least = build.iconst(wide, least(wide));
                        let minus = build.iconst(wide, -1);
                        let low = build.icmp(IntPred::Eq, lhs.value, least);
                        let by = build.icmp(IntPred::Eq, rhs.value, minus);
                        build.binary(Opcode::And, low, by, Flags::NONE)
                    };
                    self.fire(fires, overflow, routine, data, &[lhs, rhs], span);
                }
            }
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
                let check = self.overflow_check(kind);
                if !check.is_on() {
                    return;
                }
                // The other operand has the type of the operation too, whatever it was written as.
                let rhs = Operand { ty: lhs.ty, ..rhs };
                let at = self.operator(written, span);
                self.check_overflow(op, lhs, rhs, kind, at, span);
            }
            BinaryOp::Shl | BinaryOp::Shr => {
                let Some(count) = self.int_kind(rhs.ty) else { return };
                let signed = repr::is_signed(self.types(), self.target(), lhs.ty);
                let exponent = self.sanitize.shift_exponent;
                let base = if op == BinaryOp::Shl && signed && !self.unit.wrapping.signed {
                    self.sanitize.shift_base
                } else {
                    Check::Off
                };
                if !exponent.is_on() && !base.is_on() {
                    return;
                }
                let wide = self.func[lhs.value].ty;
                let prec = i128::from(wide.bits());
                let mut fires = None;
                if exponent.is_on() {
                    let ty = self.func[rhs.value].ty;
                    let mut build = self.build(span);
                    let top = build.iconst(ty, prec - 1);
                    fires = Some(build.icmp(IntPred::Ugt, rhs.value, top));
                }
                if base.is_on() {
                    // `(unsigned) lhs >> (prec - 1 - rhs)` is not zero, which is a bit set at or
                    // above the place the sign is shifted to. Masked so that a count the exponent
                    // check has already caught is not a shift the machine leaves undefined.
                    let count = self.widen(rhs.value, false, wide, span);
                    let mut build = self.build(span);
                    let top = build.iconst(wide, prec - 1);
                    let left = build.binary(Opcode::Sub, top, count, Flags::NONE);
                    let left = build.binary(Opcode::And, left, top, Flags::NONE);
                    let shifted = build.binary(Opcode::LShr, lhs.value, left, Flags::NONE);
                    let zero = build.iconst(wide, 0);
                    let set = build.icmp(IntPred::Ne, shifted, zero);
                    fires = Some(match fires {
                        Some(over) => build.binary(Opcode::Or, over, set, Flags::NONE),
                        None => set,
                    });
                }
                let Some(fires) = fires else { return };
                let check = if exponent.is_on() { exponent } else { base };
                let at = self.operator(written, span);
                let types = [self.unit.int_type(kind), self.unit.int_type(count)];
                let data = self.unit.ubsan_data(at, &types);
                self.fire(
                    fires,
                    check,
                    "__ubsan_handle_shift_out_of_bounds",
                    data,
                    &[lhs, rhs],
                    span,
                );
            }
            _ => {}
        }
    }

    /// The check of `-fsanitize=signed-integer-overflow` for an operation in `kind`, which is off
    /// for an unsigned type and under `-fwrapv`, where gcc makes none.
    fn overflow_check(&self, kind: IntKind) -> Check {
        if kind.is_signed(self.target().char_is_signed) && !self.unit.wrapping.signed {
            self.sanitize.signed_overflow
        } else {
            Check::Off
        }
    }

    /// The check of `lhs op rhs` in `kind` for an answer that `kind` cannot hold, said to be at
    /// `at`. gcc calls `__ubsan_handle_add_overflow`, `_sub_overflow` or `_mul_overflow` with a
    /// record of where and the type, and the two operands.
    fn check_overflow(
        &mut self,
        op: BinaryOp,
        lhs: Operand,
        rhs: Operand,
        kind: IntKind,
        at: BytePos,
        span: Span,
    ) {
        let check = self.overflow_check(kind);
        let (opcode, routine) = match op {
            BinaryOp::Add => (Opcode::SAddOverflow, "__ubsan_handle_add_overflow"),
            BinaryOp::Sub => (Opcode::SSubOverflow, "__ubsan_handle_sub_overflow"),
            _ => (Opcode::SMulOverflow, "__ubsan_handle_mul_overflow"),
        };
        let (_, fires) = self.build(span).checked(opcode, lhs.value, rhs.value);
        let ty = self.unit.int_type(kind);
        let data = self.unit.ubsan_data(at, &[ty]);
        self.fire(fires, check, routine, data, &[lhs, rhs], span);
    }

    /// `-value` in `ty` with the check of `-fsanitize=signed-integer-overflow` in front of it,
    /// which fires for the most negative value and calls `__ubsan_handle_negate_overflow` with
    /// the record and the value. gcc says the check is at the `-`, which is where `span` starts.
    pub(super) fn check_negate(&mut self, value: Value, ty: TypeId, span: Span) {
        if self.at.is_none() {
            return;
        }
        let Some(kind) = self.int_kind(ty) else { return };
        let check = self.overflow_check(kind);
        if !check.is_on() {
            return;
        }
        let fires = {
            let wide = self.func[value].ty;
            let mut build = self.build(span);
            let least = build.iconst(wide, least(wide));
            build.icmp(IntPred::Eq, value, least)
        };
        let data = {
            let ty = self.unit.int_type(kind);
            self.unit.ubsan_data(span.lo, &[ty])
        };
        let operand = Operand { value, ty };
        self.fire(fires, check, "__ubsan_handle_negate_overflow", data, &[operand], span);
    }

    /// The check of `++` or `--` on an object of type `ty` that held `old`, which is `old + 1` or
    /// `old - 1` and is checked as that.
    ///
    /// gcc does the step of a type narrower than `int`, and of a bit-field, in the unsigned type,
    /// so there is no check for it. `operand` is the object, and the check is said to be at the
    /// operator, which is after the object for a postfix one.
    pub(super) fn check_step(
        &mut self,
        old: Value,
        one: Value,
        (up, postfix): (bool, bool),
        (operand, bits): (ExprId, bool),
        span: Span,
    ) {
        if self.at.is_none() || bits {
            return;
        }
        let ty = self.tast()[operand].ty;
        let Some(kind) = self.int_kind(ty) else { return };
        if kind.rank() < IntKind::Int.rank() || !self.overflow_check(kind).is_on() {
            return;
        }
        let at = if postfix { self.after(operand, span) } else { span.lo };
        let op = if up { BinaryOp::Add } else { BinaryOp::Sub };
        let lhs = Operand { value: old, ty };
        let rhs = Operand { value: one, ty };
        self.check_overflow(op, lhs, rhs, kind, at, span);
    }

    /// Where the operator after `operand` is, which is the first byte after it that is not a space
    /// or a closing parenthesis, and where `span` starts when that is in another file.
    fn after(&self, operand: ExprId, span: Span) -> BytePos {
        self.next_of(operand, b"+-", span)
    }

    /// Where the first byte after `operand` that is not a space or a closing parenthesis is, when
    /// it is one of `starts`, and where `span` starts when it is not or when it is in another
    /// file.
    fn next_of(&self, operand: ExprId, starts: &[u8], span: Span) -> BytePos {
        let sources = self.unit.sources;
        let from = sources.real(self.tast().expr_span(operand).hi);
        let Some(id) = sources.lookup_file(from) else { return span.lo };
        let file = sources.file(id);
        let bytes = file.bytes();
        let start = (from - file.start) as usize;
        let found = bytes.get(start..).and_then(|after| {
            after.iter().position(|&byte| !byte.is_ascii_whitespace() && byte != b')')
        });
        match found {
            Some(at) if starts.contains(&bytes[start + at]) => from + at as u32,
            _ => span.lo,
        }
    }

    /// What an access through `pointer` to an object of type `ty` is checked as, which is
    /// [`None`] when no check of an access is on.
    pub(super) fn pointing(&self, pointer: Value, ty: TypeId, at: BytePos) -> Option<Pointed> {
        self.checks_access().then_some(Pointed { pointer, ty, member: false, at })
    }

    /// Whether a check of what an access goes through is on, which is `-fsanitize=null`,
    /// `-fsanitize=alignment` or `-fsanitize=object-size`.
    pub(super) fn checks_access(&self) -> bool {
        let sanitize = &self.sanitize;
        sanitize.null.is_on() || sanitize.alignment.is_on() || sanitize.object_size.is_on()
    }

    /// What an access to `base[index]` at `addr` is checked as, where `inner` is what the array
    /// that `base` decays from was reached through.
    ///
    /// An element of a member is a part of the member, so it is the record that is checked, at
    /// the `[`. An element of an array that a pointer points at, as in `(*q)[i]`, and an element
    /// that a pointer is stepped to are checked as themselves. An element of an array that is a
    /// variable is not checked, which is what gcc does too.
    pub(super) fn indexed_pointed(
        &self,
        (base, index): (ExprId, ExprId),
        addr: Value,
        ty: TypeId,
        inner: Option<Pointed>,
        span: Span,
    ) -> Option<Pointed> {
        let through = match inner {
            Some(inner) => !inner.member,
            None => {
                let base_ty = self.types().canonical(self.tast()[base].ty);
                matches!(self.types().kind(base_ty), TypeKind::Pointer(_))
            }
        };
        let at = |body: &Self| body.bracket(base, index, span);
        match inner {
            Some(inner) if inner.member => Some(Pointed { at: at(self), ..inner }),
            _ if through => self.pointing(addr, ty, at(self)),
            _ => None,
        }
    }

    /// What an access to a member of the object at `holder` is checked as, where `pointed` is
    /// what that object was reached through. `span` is the member expression, and the access is
    /// said to be at its `->` or `.`.
    pub(super) fn member_pointed(
        &self,
        pointed: Option<Pointed>,
        holder: ExprId,
        span: Span,
    ) -> Option<Pointed> {
        let pointed = pointed?;
        // `p->m` is `(*p).m` with a `*p` that the checking made, and that has the span of the
        // whole of `p->m`, so the operator is the one after `p`.
        let tast = self.tast();
        let before = match tast[holder].kind {
            ExprKind::Unary { op: UnaryOp::Deref, operand } if tast.expr_span(holder) == span => {
                operand
            }
            _ => holder,
        };
        let at = self.next_of(before, b"-.", span);
        Some(Pointed { member: true, at, ..pointed })
    }

    /// The check of `-fsanitize=pointer-overflow` on the step from the record at `record` to
    /// its member at `member`, when `holder` names a record that a pointer points at, as in
    /// `p->m`. The member is further on, so the step wraps when it ends below the record. gcc
    /// says it is at the `->` or the `.`.
    pub(super) fn check_member_step(
        &mut self,
        (record, member): (Place, Place),
        holder: ExprId,
        span: Span,
    ) {
        if !self.sanitize.pointer_overflow.is_on() || self.at.is_none() {
            return;
        }
        let tast = self.tast();
        let ExprKind::Unary { op: UnaryOp::Deref, operand } = tast[holder].kind else { return };
        let (Where::Addr(from), Where::Addr(to)) = (record.at, member.at) else { return };
        if from == to {
            return;
        }
        let before = if tast.expr_span(holder) == span { operand } else { holder };
        let at = self.next_of(before, b"-.", span);
        self.check_wrap((from, to), None, at, span);
    }

    /// The decay of the array `operand` names, at `place`. When a subscript is about to use it,
    /// what the array was reached through is handed to the subscript, and otherwise the decay of
    /// a member is checked as an access to it, where the expression starts, as gcc does.
    pub(super) fn check_decayed(&mut self, operand: ExprId, place: Place, span: Span) {
        if let Some(slot) = self.indexed.get_mut(&operand) {
            *slot = place.pointed;
            return;
        }
        if let Some(pointed) = place.pointed.filter(|pointed| pointed.member) {
            self.check_pointed(pointed, Access::Load, span.lo, span);
        }
    }

    /// `&` of the object at `place`, which is checked when it is a member reached through a
    /// pointer, at the `&`. gcc folds `&*p` and `&p[i]` to the pointer, so those are not.
    pub(super) fn check_taken(&mut self, place: Place, span: Span) {
        if let Some(pointed) = place.pointed.filter(|pointed| pointed.member) {
            self.check_pointed(pointed, Access::Load, span.lo, span);
        }
    }

    /// The check of a load from `place`, where the expression that names it says.
    pub(super) fn check_load(&mut self, place: Place, span: Span) {
        if let Some(pointed) = place.pointed {
            self.check_pointed(pointed, Access::Load, pointed.at, span);
        }
    }

    /// The check of a load from `place` that gcc says is at `at`, which is the start of a call
    /// for an argument and the assignment operator for the source of a copy.
    pub(super) fn check_load_at(&mut self, place: Place, at: BytePos, span: Span) {
        if let Some(pointed) = place.pointed {
            self.check_pointed(pointed, Access::Load, at, span);
        }
    }

    /// The check of a store to `place`.
    pub(super) fn check_store(&mut self, place: Place, span: Span) {
        if let Some(pointed) = place.pointed {
            self.check_pointed(pointed, Access::Store, pointed.at, span);
        }
    }

    /// `place` with a store to it said to be at `at`, the operator of the assignment or of the
    /// step that stores.
    pub(super) fn stored_at(place: Place, at: BytePos) -> Place {
        let pointed = place.pointed.map(|pointed| Pointed { at, ..pointed });
        Place { pointed, ..place }
    }

    /// Where the operator of `lhs = rhs` or `lhs op= rhs` is.
    pub(super) fn assigned_at(&self, operands: (ExprId, ExprId), span: Span) -> BytePos {
        self.operator(operands, span)
    }

    /// Where the `++` or `--` of a step of `operand` is.
    pub(super) fn stepped_at(&self, operand: ExprId, postfix: bool, span: Span) -> BytePos {
        if postfix { self.after(operand, span) } else { span.lo }
    }

    /// The check of `-fsanitize=null` and `-fsanitize=alignment` on an access through `pointed`.
    ///
    /// gcc calls `__ubsan_handle_type_mismatch_v1` with a record of where the access is, the type
    /// the pointer points at, the alignment of that type as a power of two and what the access
    /// was, which is 0 for a load, 1 for a store and 3 for a member access, and with the pointer.
    /// The alignment is checked only when the type asks for more than one byte, and when it is
    /// not checked the record says a power of zero and the pointer is passed as zero. When the
    /// two checks are asked for in different ways, a trap is made if either asks for one, and a
    /// report if either can go on after one.
    fn check_pointed(&mut self, pointed: Pointed, access: Access, at: BytePos, span: Span) {
        self.check_null_aligned(pointed, access, at, span);
        self.check_object_size(pointed, access, at, span);
    }

    /// The check of `-fsanitize=null` and `-fsanitize=alignment` that [`Self::check_pointed`]
    /// puts first.
    fn check_null_aligned(&mut self, pointed: Pointed, access: Access, at: BytePos, span: Span) {
        if self.at.is_none() {
            return;
        }
        let null = self.sanitize.null;
        let alignment = self.sanitize.alignment;
        let align = if alignment.is_on() {
            repr::align_of(self.types(), self.target(), pointed.ty)
        } else {
            0
        };
        let align = if align > 1 { align } else { 0 };
        if align == 0 && !null.is_on() {
            return;
        }
        let checks = [null.is_on().then_some(null), (align != 0).then_some(alignment)];
        let check = if checks.contains(&Some(Check::Trap)) {
            Check::Trap
        } else if checks.contains(&Some(Check::Report)) {
            Check::Report
        } else {
            Check::Abort
        };
        let address = self.address;
        let (fires, handed) = {
            let mut build = self.build(span);
            let number = build.unary(Opcode::PtrToInt, pointed.pointer, address);
            let zero = build.iconst(address, 0);
            let null_fires = null.is_on().then(|| build.icmp(IntPred::Eq, number, zero));
            let misaligned = (align != 0).then(|| {
                let mask = build.iconst(address, i128::from(align) - 1);
                let low = build.binary(Opcode::And, number, mask, Flags::NONE);
                build.icmp(IntPred::Ne, low, zero)
            });
            let fires = match (null_fires, misaligned) {
                (Some(null), Some(misaligned)) => {
                    build.binary(Opcode::Or, null, misaligned, Flags::NONE)
                }
                (Some(one), None) | (None, Some(one)) => one,
                (None, None) => return,
            };
            (fires, if align == 0 { zero } else { number })
        };
        let kind = match access {
            _ if pointed.member => 3,
            Access::Load => 0,
            Access::Store => 1,
        };
        let data = {
            let ty = self.unit.pointee_type(pointed.ty);
            let log = if align == 0 { 0 } else { align.trailing_zeros() as u8 };
            self.unit.ubsan_record(at, &[ty], &[log, kind])
        };
        let operand = Operand { value: handed, ty: pointed.ty };
        self.fire(fires, check, "__ubsan_handle_type_mismatch_v1", data, &[operand], span);
    }

    /// The check of `-fsanitize=object-size` on an access through `pointed`, which fires when
    /// the object the pointer points into has fewer bytes left from it than the access reads or
    /// writes.
    ///
    /// gcc calls `__ubsan_handle_type_mismatch_v1` with the record [`Self::check_pointed`] makes,
    /// with a power of zero, and with the pointer. How many bytes are left is the question that
    /// `__builtin_object_size` asks with a zero, which the optimizer answers when it can see the
    /// object and answers as the largest number when it cannot, and that never fires. A member is
    /// left out, because what is checked there is the record, and the record a program reaches
    /// one member of can be shorter than its type.
    fn check_object_size(&mut self, pointed: Pointed, access: Access, at: BytePos, span: Span) {
        let check = self.sanitize.object_size;
        if !check.is_on() || pointed.member || self.at.is_none() {
            return;
        }
        if repr::is_variable_length(self.types(), pointed.ty) {
            return;
        }
        let size = repr::size_of(self.types(), self.target(), pointed.ty);
        if size == 0 {
            return;
        }
        let address = self.address;
        let (fires, number) = {
            let args = self.func.push_values(&[pointed.pointer]);
            let data =
                InstData { args, extra: Extra::Question(0), ..InstData::new(Opcode::ObjectSize) };
            let mut build = self.build(span);
            let left = build.value(data, address);
            let wanted = build.iconst(address, i128::from(size));
            let fires = build.icmp(IntPred::Ugt, wanted, left);
            (fires, build.unary(Opcode::PtrToInt, pointed.pointer, address))
        };
        let kind = match access {
            Access::Load => 0,
            Access::Store => 1,
        };
        let data = {
            let ty = self.unit.pointee_type(pointed.ty);
            self.unit.ubsan_record(at, &[ty], &[0, kind])
        };
        let operand = Operand { value: number, ty: pointed.ty };
        self.fire(fires, check, "__ubsan_handle_type_mismatch_v1", data, &[operand], span);
    }

    /// The check of `-fsanitize=builtin` on `__builtin_clz` or `__builtin_ctz` of `value`, whose
    /// answer C leaves undefined for a zero.
    ///
    /// gcc calls `__ubsan_handle_invalid_builtin` with a record of where the call starts and a
    /// byte that is 1 for `clz` and 0 for `ctz`. An operand that is a constant other than zero
    /// has no check.
    pub(super) fn check_builtin(&mut self, value: Value, leading: bool, span: Span) {
        let check = self.sanitize.builtin;
        if !check.is_on() || self.at.is_none() || self.bits_of(value).is_some_and(|bits| bits != 0)
        {
            return;
        }
        let ty = self.func[value].ty;
        let fires = {
            let mut build = self.build(span);
            let zero = build.iconst(ty, 0);
            build.icmp(IntPred::Eq, value, zero)
        };
        let fields = [Field::Loc(Some(span.lo)), Field::Byte(u8::from(leading))];
        let data = self.unit.ubsan_fields(&fields);
        self.fire(fires, check, "__ubsan_handle_invalid_builtin", data, &[], span);
    }

    /// Whether `-fsanitize=nonnull-attribute` checks the argument `number`, counted from one, of
    /// a call to `callee`, which is when a `nonnull` attribute on the function names it.
    pub(super) fn checks_nonnull_arg(&self, callee: Option<DeclId>, number: usize) -> bool {
        self.sanitize.nonnull_attribute.is_on()
            && callee.is_some_and(|decl| self.tast().is_nonnull_arg(decl, number))
    }

    /// The check of `-fsanitize=nonnull-attribute` on the pointer arguments of a call that a
    /// `nonnull` attribute names, each with its number counted from one, after every argument
    /// is worked out.
    ///
    /// gcc calls `__ubsan_handle_nonnull_arg` with a record of where the call starts, where the
    /// attribute is, which gcc does not know either, and the number of the argument.
    pub(super) fn check_nonnull_args(&mut self, args: &[(usize, Value)], span: Span) {
        let check = self.sanitize.nonnull_attribute;
        for &(number, value) in args {
            if self.at.is_none() || !self.func[value].ty.is_ptr() {
                continue;
            }
            let fires = self.is_null(value, span);
            let number = u32::try_from(number).unwrap_or(u32::MAX);
            let fields = [Field::Loc(Some(span.lo)), Field::Loc(None), Field::Int(number)];
            let data = self.unit.ubsan_fields(&fields);
            self.fire(fires, check, "__ubsan_handle_nonnull_arg", data, &[], span);
        }
    }

    /// The check of `-fsanitize=returns-nonnull-attribute` on `value`, which a function that
    /// `returns_nonnull` says never answers with a null pointer is about to answer with.
    /// `expr` is the expression it was written as.
    ///
    /// gcc calls `__ubsan_handle_nonnull_return_v1` with a record of where the attribute is,
    /// which it does not know, and a second record of where the expression starts.
    pub(super) fn check_nonnull_return(&mut self, value: Value, expr: ExprId, span: Span) {
        let check = self.sanitize.returns_nonnull_attribute;
        if !check.is_on() || self.at.is_none() || !self.func[value].ty.is_ptr() {
            return;
        }
        let fires = self.is_null(value, span);
        let data = self.unit.ubsan_fields(&[Field::Loc(None)]);
        let at = self.tast().expr_span(expr).lo;
        let place = self.unit.ubsan_fields(&[Field::Loc(Some(at))]);
        let routine = "__ubsan_handle_nonnull_return_v1";
        self.fire_with(fires, check, routine, data, span, |body| {
            vec![body.global_addr(place, span)]
        });
    }

    /// Whether `pointer` is null.
    fn is_null(&mut self, pointer: Value, span: Span) -> Value {
        let address = self.address;
        let mut build = self.build(span);
        let number = build.unary(Opcode::PtrToInt, pointer, address);
        let zero = build.iconst(address, 0);
        build.icmp(IntPred::Eq, number, zero)
    }

    /// `base[index]` at `pointer`, which is what the array decays to or the pointer, stepped
    /// `steps` elements of `size` with the check of `-fsanitize=pointer-overflow`. `taken` is
    /// where the `&` of `&base[index]` is.
    ///
    /// gcc says the step is at the `[`, and at the `&` when the address is taken. An element of
    /// an array that is not reached through a pointer, at an index that is a constant, is not
    /// checked, which is what gcc leaves out as well.
    pub(super) fn element_step(
        &mut self,
        (base, index): (ExprId, ExprId),
        (pointer, steps): (Value, Value),
        (signed, size): (bool, Stride),
        taken: Option<BytePos>,
        span: Span,
    ) -> Value {
        let tast = self.tast();
        let named = match tast[base].kind {
            ExprKind::Convert { kind: Conversion::ArrayDecay, operand } => !self.pointed(operand),
            _ => false,
        };
        if !self.sanitize.pointer_overflow.is_on() || named && self.bits_of(steps).is_some() {
            return self.step(pointer, steps, signed, size, false, span);
        }
        let at = taken.unwrap_or_else(|| self.bracket(base, index, span));
        self.checked_step((pointer, steps), (signed, size), false, at, span)
    }

    /// `addr` stepped `steps` elements of `size`, back when `back` says so, with the check of
    /// `-fsanitize=pointer-overflow` said to be at `at`.
    ///
    /// gcc calls `__ubsan_handle_pointer_overflow` with a record of where the operator is, the
    /// address and the address it stepped to. It fires when a step forward ends below where it
    /// started or a step back ends above it, which is the address wrapping. A step of a constant
    /// zero has no check.
    pub(super) fn checked_step(
        &mut self,
        (addr, steps): (Value, Value),
        (signed, size): (bool, Stride),
        back: bool,
        at: BytePos,
        span: Span,
    ) -> Value {
        if !self.sanitize.pointer_overflow.is_on()
            || self.at.is_none()
            || self.bits_of(steps) == Some(0)
        {
            return self.step(addr, steps, signed, size, back, span);
        }
        // The bytes are worked out here, since their sign is what says which way is a wrap.
        let amount = self.scaled(steps, signed, size, back, span);
        let moved = self.step(addr, amount, true, Stride::Bytes(1), false, span);
        self.check_wrap((addr, moved), Some(amount), at, span);
        moved
    }

    /// The check of `-fsanitize=pointer-overflow` on a step from `from` to `to` by `amount`
    /// bytes, or by a number of bytes that is known not to be negative when there is no
    /// `amount`. It fires when a step forward ends below where it started or a step back ends
    /// above it. A step of zero bytes ends where it started and is neither.
    fn check_wrap(
        &mut self,
        (from, to): (Value, Value),
        amount: Option<Value>,
        at: BytePos,
        span: Span,
    ) {
        let check = self.sanitize.pointer_overflow;
        let address = self.address;
        let (fires, from, to) = {
            let mut build = self.build(span);
            let from = build.unary(Opcode::PtrToInt, from, address);
            let to = build.unary(Opcode::PtrToInt, to, address);
            let below = build.icmp(IntPred::Ult, to, from);
            // A step that is not zero moves the address, so it ends either below or above where
            // it started, and a step back wraps exactly when it does not end below.
            let fires = match amount {
                Some(amount) => {
                    let zero = build.iconst(address, 0);
                    let backward = build.icmp(IntPred::Slt, amount, zero);
                    build.binary(Opcode::Xor, below, backward, Flags::NONE)
                }
                None => below,
            };
            (fires, from, to)
        };
        let data = self.unit.ubsan_fields(&[Field::Loc(Some(at))]);
        let routine = "__ubsan_handle_pointer_overflow";
        self.fire_with(fires, check, routine, data, span, |_| vec![from, to]);
    }

    /// Where the operator of `lhs op rhs` is, for a step of a pointer.
    pub(super) fn stepped_by(&self, operands: (ExprId, ExprId), span: Span) -> BytePos {
        self.operator(operands, span)
    }

    /// Reads a `bool` in memory with the check of `-fsanitize=bool`, and [`None`] when the read is
    /// not one that is checked, which leaves it to [`Self::read`].
    ///
    /// gcc checks a `bool` that is read from memory and nothing else: not one in a register, not a
    /// bit-field, not an atomic one and not a `hardbool` type, which has its own check. The byte is
    /// read whole and fires when it is more than one, and the value after a report is its low bit,
    /// which is what gcc gives the program that goes on.
    pub(super) fn checked_bool(
        &mut self,
        place: Place,
        operand: ExprId,
        span: Span,
    ) -> Option<Value> {
        let check = self.sanitize.boolean;
        if !check.is_on() || self.at.is_none() {
            return None;
        }
        let Where::Addr(addr) = place.at else { return None };
        let canonical = self.types().canonical(place.ty);
        if !matches!(self.types().kind(canonical), TypeKind::Bool)
            || self.types().hardbool_of(place.ty).is_some()
            || self.atomic_access(place.ty) != Ordered::No
        {
            return None;
        }
        self.check_load(place, span);
        let info = MemInfo { owns: 0, ..self.info_of(place) };
        let flags = self.flags(place.ty);
        let (byte, fires) = {
            let mut build = self.build(span);
            let byte = build.load(Type::int(8), addr, info, flags);
            let one = build.iconst(Type::int(8), 1);
            (byte, build.icmp(IntPred::Ugt, byte, one))
        };
        let at = self.loaded_at(operand, span);
        let ty = self.unit.bool_type();
        let data = self.unit.ubsan_data(at, &[ty]);
        let operand = Operand { value: byte, ty: place.ty };
        self.fire(fires, check, "__ubsan_handle_load_invalid_value", data, &[operand], span);
        Some(self.build(span).unary(Opcode::Trunc, byte, Type::I1))
    }

    /// The check of `-fsanitize=unreachable`, put in where a `__builtin_unreachable()` is.
    ///
    /// Control that gets here has broken the promise, so there is nothing to test and the call is
    /// made each time. gcc has one routine for this, and it does not come back, so a report and an
    /// abort are the same call. The record has where the call is written and no type.
    pub(super) fn checked_unreachable(&mut self, span: Span) {
        let check = self.sanitize.unreachable;
        if !check.is_on() || self.at.is_none() {
            return;
        }
        if check == Check::Trap {
            self.build(span).inst(InstData::new(Opcode::Trap), &[]);
            return;
        }
        let data = self.unit.ubsan_data(span.lo, &[]);
        let callee = self.unit.names.intern("__ubsan_handle_builtin_unreachable");
        let args = [self.global_addr(data, span)];
        let params = [self.func[args[0]].ty];
        let signature = self.func.add_signature(Signature::new().with_params(&params));
        self.build(span).call(callee, signature, &args);
    }

    /// The check of `-fsanitize=vla-bound` on `count`, the length of a variable length array that
    /// a declaration has just worked out, which fires when it is not positive.
    ///
    /// gcc calls `__ubsan_handle_vla_bound_not_positive` with a record of where the declaration
    /// names what it declares and the type the length was written in, which is not promoted, and
    /// the length.
    pub(super) fn check_vla_bound(&mut self, vla: VlaId, count: Value, at: BytePos) {
        let check = self.sanitize.vla_bound;
        if !check.is_on() || self.at.is_none() {
            return;
        }
        let expr = self.tast().vla_size(vla);
        let ty = self.tast()[expr].ty;
        let span = self.tast().expr_span(expr);
        let Some(kind) = self.int_kind(ty) else { return };
        let signed = kind.is_signed(self.target().char_is_signed);
        let fires = {
            let wide = self.func[count].ty;
            let mut build = self.build(span);
            let zero = build.iconst(wide, 0);
            build.icmp(if signed { IntPred::Sle } else { IntPred::Eq }, count, zero)
        };
        let data = {
            let ty = self.unit.int_type(kind);
            self.unit.ubsan_data(at, &[ty])
        };
        let operand = Operand { value: count, ty };
        self.fire(fires, check, "__ubsan_handle_vla_bound_not_positive", data, &[operand], span);
    }

    /// Where gcc says a read from memory is, which is the `[` of a subscript and where the
    /// expression starts for anything else: the `*` of `*p` and the base of a member.
    fn loaded_at(&self, operand: ExprId, span: Span) -> BytePos {
        let expr = &self.tast()[operand];
        let ExprKind::Subscript { base, index } = expr.kind else {
            return self.tast().expr_span(operand).lo;
        };
        self.bracket(base, index, span)
    }

    /// Where the `[` of `base[index]` is, which is the first byte after the operand written
    /// first that is not a space or a closing parenthesis. The pointer can be written second, as
    /// in `i[a]`, so the operand written first is the one that ends first. When that byte is not a
    /// `[`, which is a subscript spelled inside a macro, the answer is where the whole expression
    /// starts.
    fn bracket(&self, base: ExprId, index: ExprId, span: Span) -> BytePos {
        let sources = self.unit.sources;
        let ends = [base, index].map(|operand| sources.real(self.tast().expr_span(operand).hi));
        let from = ends[0].min(ends[1]);
        let Some(id) = sources.lookup_file(from) else { return span.lo };
        let file = sources.file(id);
        let bytes = file.bytes();
        let start = (from - file.start) as usize;
        let found = bytes.get(start..).and_then(|after| {
            after.iter().position(|&byte| !byte.is_ascii_whitespace() && byte != b')')
        });
        match found {
            Some(at) if bytes[start + at] == b'[' => from + at as u32,
            _ => span.lo,
        }
    }

    /// `base[index]` with the check of `-fsanitize=bounds-strict` or `-fsanitize=bounds` in front
    /// of it, and [`None`] when the base is not an array of a length the check knows, which leaves
    /// the subscript to [`Self::element`]. `bounds` leaves out the arrays [`Self::trailing`] says.
    ///
    /// The length is known for an array of a fixed length, a trailing member of a record as well,
    /// for a variable length array, and for a member that `counted_by` says another member
    /// counts, which can be a pointer as well as an array of no length. The index is brought to a
    /// word the way its type says and compared as unsigned, so a negative index fires too. A
    /// constant index that is in range of a fixed length has no check, which is what gcc leaves
    /// out as well.
    pub(super) fn checked_element(
        &mut self,
        base: ExprId,
        index: ExprId,
        ty: TypeId,
        past: bool,
        span: Span,
    ) -> Option<Value> {
        self.at?;
        let (object, decayed) = match self.tast()[base].kind {
            ExprKind::Convert { kind: Conversion::ArrayDecay, operand } => (operand, true),
            ExprKind::Convert { kind: Conversion::Lvalue, operand } => (operand, false),
            _ => return None,
        };
        let object_ty = self.tast()[object].ty;
        let len = match self.types().kind(self.types().canonical(object_ty)) {
            TypeKind::Array { len, .. } if decayed => Some(len),
            TypeKind::Pointer(_) if !decayed => None,
            _ => return None,
        };
        // A member that `counted_by` counts is checked against its count either way.
        let sized = matches!(len, Some(ArrayLen::Fixed(_) | ArrayLen::Variable(_)));
        if sized && self.sanitize.trailing && self.trailing(object) {
            return None;
        }
        let index_ty = self.tast()[index].ty;
        let kind = self.int_kind(index_ty)?;
        let extra = u64::from(past);
        let length = match len {
            Some(ArrayLen::Fixed(count)) => {
                if let ExprKind::Const(id) = self.tast()[index].kind {
                    if let Const::Int(value) = self.tast()[id] {
                        if u64::try_from(value).is_ok_and(|value| value < count + extra) {
                            return None;
                        }
                    }
                }
                Length::Fixed(count)
            }
            Some(ArrayLen::Variable(vla)) => Length::Variable(vla),
            Some(ArrayLen::Unknown) | None => Length::Counted(self.counter(object)?),
            Some(ArrayLen::Star) => return None,
        };
        let check = self.sanitize.bounds;
        let address = self.address;
        let fixed = matches!(length, Length::Fixed(_));
        let (pointer, bound) = match length {
            Length::Fixed(count) => {
                let place = self.place(object);
                self.check_decayed(object, place, span);
                let pointer = self.taken(place, span);
                (pointer, self.build(span).iconst(address, i128::from(count + extra)))
            }
            Length::Variable(vla) => {
                let place = self.place(object);
                self.check_decayed(object, place, span);
                let pointer = self.taken(place, span);
                let count = self.count(vla);
                let ty = self.tast()[self.tast().vla_size(vla)].ty;
                let signed = repr::is_signed(self.types(), self.target(), ty);
                (pointer, self.widen(count, signed, address, span))
            }
            Length::Counted(counted) => {
                let holder = self.place(counted.holder);
                let at = self.member_at(holder, counted.holder, counted.field, object_ty, span);
                let object_span = self.tast().expr_span(object);
                let pointed = self.member_pointed(holder.pointed, counted.holder, object_span);
                let at = Place { pointed, ..at };
                let pointer = if decayed {
                    self.check_decayed(object, at, span);
                    self.taken(at, span)
                } else {
                    self.read(at, span)?
                };
                let at = match counted.counter[..] {
                    [index] => self.member_at(holder, counted.holder, index, counted.ty, span),
                    _ => self.nested_member(
                        holder,
                        counted.holder,
                        &counted.counter,
                        counted.ty,
                        span,
                    ),
                };
                let count = self.read(at, span)?;
                // gcc counts a negative number as none.
                let signed = repr::is_signed(self.types(), self.target(), counted.ty);
                let count = if signed {
                    let ty = self.func[count].ty;
                    let mut build = self.build(span);
                    let zero = build.iconst(ty, 0);
                    let below = build.icmp(IntPred::Slt, count, zero);
                    build.select(below, zero, count)
                } else {
                    count
                };
                (pointer, self.widen(count, false, address, span))
            }
        };
        let bound = if past && !fixed {
            let mut build = self.build(span);
            let one = build.iconst(address, 1);
            build.binary(Opcode::Add, bound, one, Flags::NONE)
        } else {
            bound
        };
        let steps = self.value(index);
        let signed = repr::is_signed(self.types(), self.target(), index_ty);
        let wide = self.widen(steps, signed, address, span);
        let fires = self.build(span).icmp(IntPred::Uge, wide, bound);
        let at = self.bracket(base, index, span);
        let promoted = if kind.rank() < IntKind::Int.rank() { IntKind::Int } else { kind };
        let described = match self.types().kind(self.types().canonical(object_ty)) {
            TypeKind::Pointer(pointee) => self.unit.array_type(pointee, true),
            _ => self.unit.array_type(object_ty, false),
        };
        let types = [described, self.unit.int_type(promoted)];
        let data = self.unit.ubsan_data(at, &types);
        let operand = Operand { value: steps, ty: index_ty };
        self.fire(fires, check, "__ubsan_handle_out_of_bounds", data, &[operand], span);
        let size = self.stride(ty, span);
        Some(self.checked_step((pointer, steps), (signed, size), false, at, span))
    }

    /// Whether `object` is an array that `-fsanitize=bounds` does not check, which is how gcc's
    /// `ubsan_instrument_bounds` finds an array a program may allocate past the end of.
    ///
    /// It is a member of an object that a pointer points at, and the last member of its
    /// structure, and each structure it is in up to the first thing that is not a member is the
    /// last member of the one around it. A member of a union is last wherever it is. Under
    /// `-fstrict-flex-arrays=` or `strict_flex_array` an array of a length the level does not
    /// take is checked even at the end, as with gcc.
    fn trailing(&self, object: ExprId) -> bool {
        let tast = self.tast();
        let types = self.types();
        if !matches!(tast[object].kind, ExprKind::Member { .. }) || !self.pointed(object) {
            return false;
        }
        let mut at = object;
        while let ExprKind::Member { base: holder, field } = tast[at].kind {
            let TypeKind::Record(id) = types.kind(types.canonical(tast[holder].ty)) else {
                return false;
            };
            let info = types.record_info(id);
            if info.kind == RecordKind::Struct && field as usize + 1 < info.fields.len() {
                return false;
            }
            let Some(member) = info.fields.get(field as usize) else { return false };
            if let TypeKind::Array { len: ArrayLen::Fixed(count), .. } =
                types.kind(types.canonical(member.ty))
            {
                let level = tast.strict_flex(id, member.name);
                let checked = match count {
                    0 => level == 3,
                    1 => level >= 2,
                    _ => level >= 1,
                };
                if checked {
                    return false;
                }
            }
            at = holder;
        }
        true
    }

    /// Whether the object `expr` names is reached through a pointer, past every member and every
    /// element of an array on the way to it.
    fn pointed(&self, expr: ExprId) -> bool {
        let tast = self.tast();
        match tast[expr].kind {
            ExprKind::Member { base, .. } => self.pointed(base),
            ExprKind::Subscript { base, .. } => match tast[base].kind {
                ExprKind::Convert { kind: Conversion::ArrayDecay, operand } => {
                    self.pointed(operand)
                }
                _ => true,
            },
            ExprKind::Unary { op: UnaryOp::Deref, .. } => true,
            _ => false,
        }
    }

    /// The member that counts the member `object` names, when `counted_by` says one does and it
    /// is an integer.
    fn counter(&self, object: ExprId) -> Option<Counted> {
        let ExprKind::Member { base: holder, field } = self.tast()[object].kind else {
            return None;
        };
        let record = self.types().canonical(self.tast()[holder].ty);
        let TypeKind::Record(id) = self.types().kind(record) else { return None };
        let fields = &self.types().record_info(id).fields;
        let name = fields.get(field as usize)?.name?;
        let counter = self.tast().counter(id, name)?;
        let path = self.field_path(id, counter)?;
        let mut record = id;
        let mut ty = None;
        for &index in &path {
            let found = self.types().record_info(record).fields.get(index as usize)?;
            if found.bits.is_some() {
                return None;
            }
            ty = Some(found.ty);
            if let TypeKind::Record(next) = self.types().kind(self.types().canonical(found.ty)) {
                record = next;
            }
        }
        let ty = ty?;
        self.int_kind(ty)?;
        Some(Counted { holder, field, counter: path, ty })
    }

    /// The indices that reach member `name` of a record, through anonymous members, with a named
    /// member of the record itself first, as the checker finds it.
    fn field_path(&self, record: RecordId, name: Symbol) -> Option<Vec<u32>> {
        let fields = &self.types().record_info(record).fields;
        if let Some(index) = fields.iter().position(|field| field.name == Some(name)) {
            return Some(vec![u32::try_from(index).ok()?]);
        }
        fields.iter().enumerate().filter(|(_, field)| field.name.is_none()).find_map(
            |(index, field)| {
                let TypeKind::Record(inner) = self.types().kind(self.types().canonical(field.ty))
                else {
                    return None;
                };
                let mut path = self.field_path(inner, name)?;
                path.insert(0, u32::try_from(index).ok()?);
                Some(path)
            },
        )
    }

    /// A member reached from the object at `place` through anonymous members, at the sum of their
    /// offsets. The members on the way are in a union as often as not, so the place is punned.
    fn nested_member(
        &mut self,
        place: Place,
        holder: ExprId,
        path: &[u32],
        ty: TypeId,
        span: Span,
    ) -> Place {
        let mut record = self.types().canonical(self.tast()[holder].ty);
        let mut byte = 0;
        for &index in path {
            let TypeKind::Record(id) = self.types().kind(record) else { break };
            let Some(found) = self.types().record_info(id).fields.get(index as usize).copied()
            else {
                break;
            };
            byte += found.offset;
            record = self.types().canonical(found.ty);
        }
        let addr = self.address_of(place, span);
        let addr = self.offset(addr, byte, span);
        Place { punned: true, restrict: place.restrict, ..Place::new(Where::Addr(addr), ty) }
    }

    /// The integer kind of a type the checks are made in, and [`None`] for any other type.
    fn int_kind(&self, ty: TypeId) -> Option<IntKind> {
        match self.types().kind(self.types().canonical(ty)) {
            TypeKind::Int(kind) => Some(kind),
            _ => None,
        }
    }

    /// The branch on `fires` to the block that does what `check` says, and the cursor in the
    /// block after it.
    ///
    /// A report and an abort both call the routine with the record and the operands, and the
    /// operation is done after a report the way it is done after no check at all, which is what
    /// recovering means. The abort's routine does not come back, but gcc does not say so and
    /// neither does this, so the two are the same code with different names in them.
    fn fire(
        &mut self,
        fires: Value,
        check: Check,
        routine: &str,
        data: Symbol,
        operands: &[Operand],
        span: Span,
    ) {
        self.fire_with(fires, check, routine, data, span, |body| {
            operands.iter().map(|&operand| body.handle(operand, span)).collect()
        });
    }

    /// [`Self::fire`] with the operands made by `operands` in the block that reports, for an
    /// operand that is not a value the way [`Self::handle`] passes one.
    fn fire_with(
        &mut self,
        fires: Value,
        check: Check,
        routine: &str,
        data: Symbol,
        span: Span,
        operands: impl FnOnce(&mut Self) -> Vec<Value>,
    ) {
        let report = self.new_block();
        let after = self.new_block();
        self.br_if(fires, report, after, span);
        self.ssa.seal(self.func, report);
        self.at = Some(report);
        if check == Check::Trap {
            self.build(span).inst(InstData::new(Opcode::Trap), &[]);
            self.build(span).unreachable();
            self.at = None;
        } else {
            let name =
                if check == Check::Abort { format!("{routine}_abort") } else { routine.into() };
            let callee = self.unit.names.intern(&name);
            let mut args = vec![self.global_addr(data, span)];
            args.extend(operands(self));
            let params: Vec<Type> = args.iter().map(|&arg| self.func[arg].ty).collect();
            let signature = self.func.add_signature(Signature::new().with_params(&params));
            self.build(span).call(callee, signature, &args);
            self.jump(after, span);
        }
        self.ssa.seal(self.func, after);
        self.at = Some(after);
    }

    /// An operand the way the runtime takes one, which gcc calls a value handle.
    ///
    /// A value that fits in a word is passed as the word, extended the way its type says. A wider
    /// one is put in a slot and the slot's address is passed, which is what the runtime reads one
    /// that is wider than a word through.
    fn handle(&mut self, operand: Operand, span: Span) -> Value {
        let value = operand.value;
        let address = self.address;
        if self.func[value].ty.bits() <= address.bits() {
            let signed = repr::is_signed(self.types(), self.target(), operand.ty);
            return self.widen(value, signed, address, span);
        }
        let size = repr::size_of(self.types(), self.target(), operand.ty);
        let align = u32::try_from(size).unwrap_or(16);
        let slot = self.scratch(size, align, span);
        let info = self.access(operand.ty);
        self.build(span).store(value, slot, info, Flags::NONE);
        slot
    }

    /// Where the operator of `lhs op rhs` is, which is where gcc says the check fired.
    ///
    /// The tree has a span for each operand and none for the operator, so it is the first byte
    /// between the two that is not a space or a closing parenthesis. When the two operands are
    /// not in that order in one file, which is an operation spelled inside a macro, the answer is
    /// where the whole expression starts, which is the use of the macro, and that is where gcc
    /// says it is as well.
    fn operator(&self, (lhs, rhs): (ExprId, ExprId), span: Span) -> BytePos {
        let sources = self.unit.sources;
        let from = sources.real(self.tast().expr_span(lhs).hi);
        let to = sources.real(self.tast().expr_span(rhs).lo);
        if from > to {
            return span.lo;
        }
        let Some(id) = sources.lookup_file(from) else { return span.lo };
        let file = sources.file(id);
        if !file.contains(to) {
            return span.lo;
        }
        let bytes = file.bytes();
        let start = (from - file.start) as usize;
        let end = ((to - file.start) as usize).min(bytes.len());
        bytes
            .get(start..end)
            .and_then(|between| {
                between.iter().position(|&byte| !byte.is_ascii_whitespace() && byte != b')')
            })
            .map_or(span.lo, |at| from + at as u32)
    }
}

impl Unit<'_> {
    /// The record a check hands the runtime: where the operator is and a descriptor for each type.
    ///
    /// gcc's layout is the file name, the line and the column as two words of four bytes, and a
    /// pointer to each descriptor. It is writable and in `.data` for the reason gcc gives, which
    /// is that the runtime writes into the location to say it has reported it already.
    pub(super) fn ubsan_data(&mut self, at: BytePos, types: &[Symbol]) -> Symbol {
        self.ubsan_record(at, types, &[])
    }

    /// The record of [`Self::ubsan_data`] with `tail` after the descriptors, one byte each, and
    /// padded to a whole number of words, as gcc lays out a record with fields of one byte at the
    /// end.
    fn ubsan_record(&mut self, at: BytePos, types: &[Symbol], tail: &[u8]) -> Symbol {
        let mut fields = vec![Field::Loc(Some(at))];
        fields.extend(types.iter().map(|&symbol| Field::Type(symbol)));
        fields.extend(tail.iter().map(|&byte| Field::Byte(byte)));
        self.ubsan_fields(&fields)
    }

    /// A record of `fields` in the order given, each at the alignment it has in C, and padded
    /// to a whole number of words, which is how gcc lays out the records of its checks.
    fn ubsan_fields(&mut self, fields: &[Field]) -> Symbol {
        let width = self.target.pointer_width / 8;
        let word = u64::from(width);
        let mut data = Vec::new();
        let mut used = 0;
        let pad = |data: &mut Vec<Datum>, used: &mut u64, align: u64| {
            let to = used.next_multiple_of(align);
            if to > *used {
                data.push(Datum::Zero(to - *used));
                *used = to;
            }
        };
        let mut places = 0;
        for &field in fields {
            match field {
                Field::Loc(None) => {
                    pad(&mut data, &mut used, word);
                    data.push(Datum::Zero(word + 8));
                    used += word + 8;
                }
                Field::Loc(Some(at)) => {
                    pad(&mut data, &mut used, word);
                    let (file, line, column) = match self.sources.presumed(at) {
                        Some(loc) => (loc.name.to_owned(), loc.line, loc.column),
                        None => (String::new(), 0, 0),
                    };
                    let mut bytes = file.into_bytes();
                    bytes.push(0);
                    let name = match places {
                        0 => format!(".Lubsan_file.{}", self.checks),
                        n => format!(".Lubsan_file.{}.{n}", self.checks),
                    };
                    places += 1;
                    let file = self.literal(bytes, 1, &name);
                    let reloc = Reloc { symbol: file, addend: 0, size: width };
                    data.push(Datum::Addr(self.module.add_reloc(reloc)));
                    for number in [line, column] {
                        let value =
                            self.module.add_imm(Imm::int(i128::from(number), Type::int(32)));
                        data.push(Datum::Scalar { ty: Type::int(32), value });
                    }
                    used += word + 8;
                }
                Field::Type(symbol) => {
                    pad(&mut data, &mut used, word);
                    let reloc = Reloc { symbol, addend: 0, size: width };
                    data.push(Datum::Addr(self.module.add_reloc(reloc)));
                    used += word;
                }
                Field::Byte(byte) => {
                    let value = self.module.add_imm(Imm::int(i128::from(byte), Type::int(8)));
                    data.push(Datum::Scalar { ty: Type::int(8), value });
                    used += 1;
                }
                Field::Int(number) => {
                    pad(&mut data, &mut used, 4);
                    let value = self.module.add_imm(Imm::int(i128::from(number), Type::int(32)));
                    data.push(Datum::Scalar { ty: Type::int(32), value });
                    used += 4;
                }
            }
        }
        pad(&mut data, &mut used, word);
        let symbol = self.names.intern(&format!(".Lubsan_data.{}", self.checks));
        self.checks += 1;
        let mut global = Global::new(symbol, used, width);
        global.linkage = IrLinkage::Internal;
        global.droppable = true;
        global.init = Some(self.module.push_data(&data));
        self.module.add_global(global);
        symbol
    }

    /// The descriptor of an integer type.
    pub(super) fn int_type(&mut self, kind: IntKind) -> Symbol {
        let bits = rucc_types::int_width(kind, self.target);
        let signed = kind.is_signed(self.target.char_is_signed);
        self.ubsan_type(&format!("'{}'", gnu_name(kind)), 0, int_info(bits, signed))
    }

    /// The descriptor of an array type, named the way gcc names one: the element, a star for each
    /// pointer it is, and each length in brackets, which is a star for a variable length, for none and
    /// for one that is not known.
    ///
    /// A typedef name of the element is its name and a record or an enumeration is its tag
    /// without the keyword, so `struct s a[4]` is `'s [4]'`. What it says about the type is what
    /// it says about the element once the arrays are taken off, and anything that is not an
    /// integer or one of the three standard floating types is unknown to the runtime.
    ///
    /// `pointed` says `ty` is what a pointer points to, which is named as an array of it of a
    /// length that is not known.
    fn array_type(&mut self, ty: TypeId, pointed: bool) -> Symbol {
        let types = self.types;
        let mut lengths = String::from(if pointed { "[*]" } else { "" });
        let mut element = ty;
        loop {
            match types.kind(element) {
                TypeKind::Typedef { underlying, .. }
                    if matches!(
                        types.kind(types.canonical(underlying)),
                        TypeKind::Array { .. }
                    ) =>
                {
                    element = underlying;
                }
                TypeKind::Array { elem, len } => {
                    // gcc has no last index for an array of none, and names it the way it names a
                    // variable length.
                    match len {
                        ArrayLen::Fixed(count) if count > 0 => {
                            lengths.push_str(&format!("[{count}]"));
                        }
                        _ => lengths.push_str("[*]"),
                    }
                    element = elem;
                }
                _ => break,
            }
        }
        let (kind, info) = match types.kind(types.canonical(element)) {
            TypeKind::Int(kind) => {
                let bits = rucc_types::int_width(kind, self.target);
                (0, int_info(bits, kind.is_signed(self.target.char_is_signed)))
            }
            TypeKind::Bool => (0, int_info(8, false)),
            TypeKind::Enum(_) => {
                let bits = 8 * repr::size_of(types, self.target, element);
                let signed = repr::is_signed(types, self.target, element);
                (0, int_info(u32::try_from(bits).unwrap_or(32), signed))
            }
            TypeKind::Float(FloatKind::Float) => (1, 32),
            TypeKind::Float(FloatKind::Double) => (1, 64),
            TypeKind::Float(FloatKind::LongDouble) => {
                let size = repr::size_of(types, self.target, element);
                let x86 = matches!(self.target.tuple.arch(), Arch::X86_64 | Arch::X86);
                (
                    1,
                    if size == 8 {
                        64
                    } else if x86 {
                        80
                    } else {
                        128
                    },
                )
            }
            _ => (0xffff, 0),
        };
        let mut stars = String::new();
        while let TypeKind::Pointer(pointee) = types.kind(types.canonical(element)) {
            stars.push('*');
            element = pointee;
        }
        let name = format!("'{} {stars}{lengths}'", self.gnu_type_name(element));
        self.ubsan_type(&name, kind, info)
    }

    /// The descriptor of the type a pointer points at, named the way gcc names one for a check
    /// of an access through a pointer: the qualifiers, `struct` or `union` for a record, the name,
    /// and a star for each pointer it is. An array is named as its element, and the descriptor
    /// says nothing more about the type, since gcc describes the pointer and not what it points
    /// at.
    fn pointee_type(&mut self, ty: TypeId) -> Symbol {
        let types = self.types;
        let mut element = ty;
        let mut stars = String::new();
        // The pointers come off first and the arrays after, as in gcc, which names a pointer
        // that is left inside an array as a type it does not know.
        for array in [false, true] {
            loop {
                match types.kind(element) {
                    TypeKind::Typedef { underlying, .. } => {
                        let below = types.kind(types.canonical(underlying));
                        if matches!(below, TypeKind::Array { .. }) != array
                            || !matches!(below, TypeKind::Array { .. } | TypeKind::Pointer(_))
                        {
                            break;
                        }
                        element = underlying;
                    }
                    TypeKind::Pointer(pointee) if !array => {
                        stars.push('*');
                        element = pointee;
                    }
                    TypeKind::Array { elem, .. } if array => element = elem,
                    _ => break,
                }
            }
        }
        let canonical = types.canonical(element);
        let quals = types.quals(element).with(types.quals(canonical));
        let mut name = String::from("'");
        for (qual, word) in [
            (Qualifiers::VOLATILE, "volatile "),
            (Qualifiers::CONST, "const "),
            (Qualifiers::RESTRICT, "restrict "),
        ] {
            if quals.has(qual) {
                name.push_str(word);
            }
        }
        let inner = match types.kind(canonical) {
            TypeKind::Atomic(inner) => {
                name.push_str("_Atomic ");
                types.canonical(inner)
            }
            _ => canonical,
        };
        if let TypeKind::Record(id) = types.kind(inner) {
            name.push_str(match types.record_info(id).kind {
                RecordKind::Union => "union ",
                _ => "struct ",
            });
        }
        name.push_str(&self.gnu_type_name(element));
        if !stars.is_empty() {
            name.push(' ');
            name.push_str(&stars);
        }
        name.push('\'');
        self.ubsan_type(&name, 0xffff, 0)
    }

    /// The name gcc gives a type that is not an array or a pointer in what a check is handed.
    fn gnu_type_name(&self, ty: TypeId) -> String {
        let types = self.types;
        let tag = match types.kind(ty) {
            TypeKind::Typedef { name, .. } => Some(name),
            TypeKind::Atomic(inner) => return self.gnu_type_name(inner),
            TypeKind::Int(kind) => return gnu_name(kind).to_owned(),
            TypeKind::Bool => return if self.c23 { "bool" } else { "_Bool" }.to_owned(),
            TypeKind::Void => return "void".to_owned(),
            TypeKind::Float(FloatKind::Float) => return "float".to_owned(),
            TypeKind::Float(FloatKind::Double) => return "double".to_owned(),
            TypeKind::Float(FloatKind::LongDouble) => return "long double".to_owned(),
            TypeKind::Record(record) => types.record_info(record).tag,
            TypeKind::Enum(id) => types.enum_info(id).tag,
            _ => None,
        };
        tag.map_or_else(|| "<unknown>".to_owned(), |name| self.names.resolve(name).to_owned())
    }

    /// The descriptor of `bool`, which gcc gives the name the dialect spells it with and describes
    /// as an unsigned integer of eight bits.
    fn bool_type(&mut self) -> Symbol {
        self.ubsan_type(if self.c23 { "'bool'" } else { "'_Bool'" }, 0, int_info(8, false))
    }

    /// The descriptor of a type, made once in a unit for each name.
    ///
    /// Two bytes that say what kind of type it is, which is 0 for an integer, 1 for a floating
    /// type and all ones for the rest, two that say more about it, and the name in quotes, which
    /// is the name gcc prints for the type and not the one the program wrote: `size_t` is
    /// `'long unsigned int'`.
    fn ubsan_type(&mut self, name: &str, kind: u32, info: u32) -> Symbol {
        // The same name can be on two descriptors, `'int'` of an integer and of a pointer's
        // target, so the key has what the descriptor says as well.
        let key = format!("{kind} {info} {name}");
        if let Some(&symbol) = self.checked_types.get(&key) {
            return symbol;
        }
        let mut data = Vec::new();
        for number in [kind, info] {
            let value = self.module.add_imm(Imm::int(i128::from(number), Type::int(16)));
            data.push(Datum::Scalar { ty: Type::int(16), value });
        }
        let mut bytes = name.as_bytes().to_vec();
        bytes.push(0);
        let size = 4 + bytes.len() as u64;
        data.push(Datum::Bytes(self.module.push_bytes(&bytes)));
        let symbol = self.names.intern(&format!(".Lubsan_type.{}", self.checked_types.len()));
        let mut global = Global::new(symbol, size, 2);
        global.linkage = IrLinkage::Internal;
        global.droppable = true;
        global.init = Some(self.module.push_data(&data));
        self.module.add_global(global);
        self.checked_types.insert(key, symbol);
        symbol
    }
}

/// The most negative value of a signed integer of the width of `ty`.
fn least(ty: Type) -> i128 {
    let bits = ty.bits().min(128);
    if bits == 128 { i128::MIN } else { -(1i128 << (bits - 1)) }
}

/// What a descriptor says about an integer type: how wide it is, as the power of two, and
/// whether it is signed.
fn int_info(bits: u32, signed: bool) -> u32 {
    (bits.trailing_zeros() << 1) | u32::from(signed)
}

/// What gcc calls an integer type, which is not always the shortest spelling of it.
fn gnu_name(kind: IntKind) -> &'static str {
    match kind {
        IntKind::Short => "short int",
        IntKind::UShort => "short unsigned int",
        IntKind::Long => "long int",
        IntKind::ULong => "long unsigned int",
        IntKind::LongLong => "long long int",
        IntKind::ULongLong => "long long unsigned int",
        IntKind::UInt128 => "__int128 unsigned",
        _ => kind.as_str(),
    }
}
