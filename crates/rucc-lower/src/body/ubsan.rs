//! The undefined behaviour checks of `-fsanitize=`, which gcc calls UBSan. See
//! [`crate::unit::Sanitize`] for why they are put in by the walk.
//!
//! A check is a comparison in front of the operation, a branch to a block that tells the runtime
//! and a branch back. The runtime is libubsan in user space and `lib/ubsan.c` in the kernel, and
//! both read what they are told the way gcc lays it out: a pointer to a record that says where the
//! operator is and what the types of its operands are called, then each operand as a word. So the
//! record is laid out the way gcc lays it out, field for field, and the names are gcc's names.

use rucc_ast::BinaryOp;
use rucc_base::Symbol;
use rucc_diag::{BytePos, Span};
use rucc_ir::{
    Datum, Flags, Global, Imm, InstData, IntPred, Linkage as IrLinkage, Opcode, Reloc, Signature,
    Type, Value,
};
use rucc_sema::{Const, Conversion, ExprId, ExprKind};
use rucc_tuple::Arch;
use rucc_types::{ArrayLen, FloatKind, IntKind, TypeId, TypeKind};

use super::{Body, MemInfo, Ordered, Place, Where};
use crate::repr;
use crate::unit::{Check, Unit};

/// One operand as the check sees it: the value and the C type it has.
#[derive(Clone, Copy)]
pub(super) struct Operand {
    pub(super) value: Value,
    pub(super) ty: TypeId,
}

/// How the length a subscript is checked against is found.
#[derive(Clone, Copy)]
enum Length {
    /// An array of a fixed length.
    Fixed(u64),
    /// A variable length array.
    Variable(rucc_types::VlaId),
    /// A member that another member counts.
    Counted(Counted),
}

/// A member that `counted_by` says another member counts: the object they are members of, the
/// member itself, and the member that counts it with its type.
#[derive(Clone, Copy)]
struct Counted {
    holder: ExprId,
    field: u32,
    counter: u32,
    ty: TypeId,
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
            BinaryOp::Div | BinaryOp::Rem if self.sanitize.divide.is_on() => {
                let check = self.sanitize.divide;
                let zero = {
                    let ty = self.func[rhs.value].ty;
                    let mut build = self.build(span);
                    let zero = build.iconst(ty, 0);
                    build.icmp(IntPred::Eq, rhs.value, zero)
                };
                let at = self.operator(written, span);
                let ty = self.unit.int_type(kind);
                let data = self.unit.ubsan_data(at, &[ty]);
                self.fire(zero, check, "__ubsan_handle_divrem_overflow", data, &[lhs, rhs], span);
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

    /// `base[index]` with the check of `-fsanitize=bounds-strict` in front of it, and [`None`]
    /// when the base is not an array of a length the check knows, which leaves the subscript to
    /// [`Self::element`].
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
        let index_ty = self.tast()[index].ty;
        let kind = self.int_kind(index_ty)?;
        let extra = u64::from(past);
        let length = match len {
            Some(ArrayLen::Fixed(count)) => {
                if let ExprKind::Const(id) = self.tast()[index].kind
                    && let Const::Int(value) = self.tast()[id]
                    && u64::try_from(value).is_ok_and(|value| value < count + extra)
                {
                    return None;
                }
                Length::Fixed(count)
            }
            Some(ArrayLen::Variable(vla)) => Length::Variable(vla),
            Some(ArrayLen::Unknown) | None => Length::Counted(self.counter(object)?),
            Some(ArrayLen::Star) => return None,
        };
        let check = self.sanitize.bounds;
        let address = self.address;
        let (pointer, bound) = match length {
            Length::Fixed(count) => {
                let place = self.place(object);
                let pointer = self.taken(place, span);
                (pointer, self.build(span).iconst(address, i128::from(count + extra)))
            }
            Length::Variable(vla) => {
                let place = self.place(object);
                let pointer = self.taken(place, span);
                let count = self.count(vla);
                let ty = self.tast()[self.tast().vla_size(vla)].ty;
                let signed = repr::is_signed(self.types(), self.target(), ty);
                (pointer, self.widen(count, signed, address, span))
            }
            Length::Counted(counted) => {
                let holder = self.place(counted.holder);
                let at = self.member_at(holder, counted.holder, counted.field, object_ty, span);
                let pointer = if decayed { self.taken(at, span) } else { self.read(at, span)? };
                let at = self.member_at(holder, counted.holder, counted.counter, counted.ty, span);
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
        let bound = if past && !matches!(length, Length::Fixed(_)) {
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
        Some(self.step(pointer, steps, signed, size, false, span))
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
        let at = fields.iter().position(|field| field.name == Some(counter))?;
        let ty = fields[at].ty;
        self.int_kind(ty)?;
        Some(Counted { holder, field, counter: u32::try_from(at).ok()?, ty })
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
            for &operand in operands {
                args.push(self.handle(operand, span));
            }
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
        let (file, line, column) = match self.sources.presumed(at) {
            Some(loc) => (loc.name.to_owned(), loc.line, loc.column),
            None => (String::new(), 0, 0),
        };
        let mut bytes = file.into_bytes();
        bytes.push(0);
        let name = format!(".Lubsan_file.{}", self.checks);
        let file = self.literal(bytes, 1, &name);

        let word = self.target.pointer_width / 8;
        let mut data =
            vec![Datum::Addr(self.module.add_reloc(Reloc { symbol: file, addend: 0, size: word }))];
        for number in [line, column] {
            let value = self.module.add_imm(Imm::int(i128::from(number), Type::int(32)));
            data.push(Datum::Scalar { ty: Type::int(32), value });
        }
        for &symbol in types {
            data.push(Datum::Addr(self.module.add_reloc(Reloc { symbol, addend: 0, size: word })));
        }
        let size = u64::from(word) * (1 + types.len() as u64) + 8;
        let symbol = self.names.intern(&format!(".Lubsan_data.{}", self.checks));
        self.checks += 1;
        let mut global = Global::new(symbol, size, word);
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
        let name = name.to_owned();
        if let Some(&symbol) = self.checked_types.get(&name) {
            return symbol;
        }
        let mut data = Vec::new();
        for number in [kind, info] {
            let value = self.module.add_imm(Imm::int(i128::from(number), Type::int(16)));
            data.push(Datum::Scalar { ty: Type::int(16), value });
        }
        let mut bytes = name.clone().into_bytes();
        bytes.push(0);
        let size = 4 + bytes.len() as u64;
        data.push(Datum::Bytes(self.module.push_bytes(&bytes)));
        let symbol = self.names.intern(&format!(".Lubsan_type.{}", self.checked_types.len()));
        let mut global = Global::new(symbol, size, 2);
        global.linkage = IrLinkage::Internal;
        global.droppable = true;
        global.init = Some(self.module.push_data(&data));
        self.module.add_global(global);
        self.checked_types.insert(name, symbol);
        symbol
    }
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
