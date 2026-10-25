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
use rucc_sema::ExprId;
use rucc_types::{IntKind, TypeId, TypeKind};

use super::Body;
use crate::repr;
use crate::unit::{Check, Unit};

/// One operand as the check sees it: the value and the C type it has.
#[derive(Clone, Copy)]
pub(super) struct Operand {
    pub(super) value: Value,
    pub(super) ty: TypeId,
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
                let data = self.unit.ubsan_data(at, &[kind]);
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
                let data = self.unit.ubsan_data(at, &[kind, count]);
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
    pub(super) fn ubsan_data(&mut self, at: BytePos, kinds: &[IntKind]) -> Symbol {
        let (file, line, column) = match self.sources.presumed(at) {
            Some(loc) => (loc.name.to_owned(), loc.line, loc.column),
            None => (String::new(), 0, 0),
        };
        let mut bytes = file.into_bytes();
        bytes.push(0);
        let name = format!(".Lubsan_file.{}", self.checks);
        let file = self.literal(bytes, 1, &name);
        let types: Vec<Symbol> = kinds.iter().map(|&kind| self.ubsan_type(kind)).collect();

        let word = self.target.pointer_width / 8;
        let mut data =
            vec![Datum::Addr(self.module.add_reloc(Reloc { symbol: file, addend: 0, size: word }))];
        for number in [line, column] {
            let value = self.module.add_imm(Imm::int(i128::from(number), Type::int(32)));
            data.push(Datum::Scalar { ty: Type::int(32), value });
        }
        for symbol in types {
            data.push(Datum::Addr(self.module.add_reloc(Reloc { symbol, addend: 0, size: word })));
        }
        let size = u64::from(word) * (1 + kinds.len() as u64) + 8;
        let symbol = self.names.intern(&format!(".Lubsan_data.{}", self.checks));
        self.checks += 1;
        let mut global = Global::new(symbol, size, word);
        global.linkage = IrLinkage::Internal;
        global.droppable = true;
        global.init = Some(self.module.push_data(&data));
        self.module.add_global(global);
        symbol
    }

    /// The descriptor of an integer type, made once in a unit for each type.
    ///
    /// Two bytes that say it is an integer, two that say how wide it is and whether it is signed,
    /// and the name in quotes, which is the name gcc prints for the type and not the one the
    /// program wrote: `size_t` is `'long unsigned int'`.
    fn ubsan_type(&mut self, kind: IntKind) -> Symbol {
        let name = format!("'{}'", gnu_name(kind));
        if let Some(&symbol) = self.checked_types.get(&name) {
            return symbol;
        }
        let bits = rucc_types::int_width(kind, self.target);
        let signed = kind.is_signed(self.target.char_is_signed);
        let info = (bits.trailing_zeros() << 1) | u32::from(signed);
        let mut data = Vec::new();
        for number in [0, info] {
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
