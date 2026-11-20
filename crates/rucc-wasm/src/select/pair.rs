//! The values that are held in two `i64`, which are an `i128` and a `long double`.
//!
//! This is section 8.6 of the WebAssembly notes. A pair has two locals, the low half first, and it
//! travels as two `i64` parameters. An addition, a subtraction, a comparison, a bit operation, an
//! extension and a truncation of an `i128` are written in place, with the carry from `i64.lt_u`.
//! The other operations on an `i128`, and every operation on a `long double`, are calls to the
//! compiler runtime with the names and the types that clang gives them, so that the runtime of
//! wasi-sdk and the one of rucc both serve. A runtime function that gives a pair writes it to the
//! buffer of the frame, whose address is its first parameter, and the caller reads the two halves
//! back from there.

use rucc_ir::{Extra, FloatPred, Inst, IntPred, Opcode, Type, Value};
use rucc_object::wasm::{FuncType, ValType};

use super::{Lower, Result, align_field, int_compare, int_op};
use crate::{emit, is_pair, valtype};

/// Whether an operation that gives a pair calls the runtime, and so needs the buffer of the frame.
pub(super) fn calls_runtime(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Mul
            | Opcode::SDiv
            | Opcode::UDiv
            | Opcode::SRem
            | Opcode::URem
            | Opcode::Shl
            | Opcode::LShr
            | Opcode::AShr
            | Opcode::FAdd
            | Opcode::FSub
            | Opcode::FMul
            | Opcode::FDiv
            | Opcode::FRem
            | Opcode::Fma
            | Opcode::FPExt
            | Opcode::FPToSI
            | Opcode::FPToUI
            | Opcode::SIToFP
            | Opcode::UIToFP
            | Opcode::SMulOverflow
            | Opcode::UMulOverflow
            | Opcode::Call
            | Opcode::CallIndirect
    )
}

/// The unsigned form of a comparison, which is how two low halves are compared.
fn unsigned(pred: IntPred) -> IntPred {
    match pred {
        IntPred::Slt => IntPred::Ult,
        IntPred::Sgt => IntPred::Ugt,
        IntPred::Sle => IntPred::Ule,
        IntPred::Sge => IntPred::Uge,
        other => other,
    }
}

/// The value types that a list of values travels as, with two `i64` for a pair.
fn travel(types: impl IntoIterator<Item = Type>) -> Result<Vec<ValType>> {
    let mut out = Vec::new();
    for ty in types {
        if is_pair(ty) {
            out.extend([ValType::I64, ValType::I64]);
        } else {
            out.push(valtype(ty)?);
        }
    }
    Ok(out)
}

impl Lower<'_, '_> {
    /// Push one half of a pair. A constant is written here, and anything else is read from the
    /// local of the half.
    pub(super) fn push_half(&mut self, value: Value, high: bool) -> Result<()> {
        if let Some((inst, _)) = self.def(value) {
            let data = &self.func[inst];
            if let (Opcode::IConst | Opcode::FConst, Extra::Imm(imm)) = (data.opcode, data.extra) {
                let bits = self.func[imm].bits();
                let half = if high { bits >> 64 } else { bits };
                self.code.i64_const(half as u64 as i64);
                return Ok(());
            }
        }
        let Some(&local) = self.local.get(&value) else {
            return Err(format!("a value of type {} has no local", self.ty(value)));
        };
        self.code.local_get(local + u32::from(high));
        Ok(())
    }

    fn scratch(&self) -> u32 {
        self.frame.scratch.expect("a function that calls the runtime for a pair has a buffer")
    }

    /// Push the address `offset` bytes into the buffer of the frame.
    pub(super) fn scratch_address(&mut self, offset: u32) {
        let at = self.scratch() + offset;
        let fp = self.frame_pointer();
        self.code.local_get(fp);
        if at != 0 {
            self.code.i32_const(at as i32);
            self.code.op(emit::I32_ADD);
        }
    }

    /// Read the pair that the runtime wrote to the buffer of the frame into the locals of
    /// `value`.
    pub(super) fn load_scratch(&mut self, value: Value) {
        let at = self.scratch();
        let fp = self.frame_pointer();
        let local = self.local[&value];
        for half in 0..2 {
            self.code.local_get(fp);
            self.code.mem(emit::I64_LOAD, 3, at + 8 * half);
            self.code.local_set(local + half);
        }
    }

    /// Store a pair to the address that `address` pushes, which is aligned to 8 at least.
    pub(super) fn store_pair(
        &mut self,
        value: Value,
        address: impl Fn(&mut Self) -> Result<()>,
    ) -> Result<()> {
        self.store_pair_at(value, 0, 8, address)
    }

    /// Store a pair `offset` bytes past the address that `address` pushes, which is aligned to
    /// `align`.
    pub(super) fn store_pair_at(
        &mut self,
        value: Value,
        offset: u32,
        align: u32,
        address: impl Fn(&mut Self) -> Result<()>,
    ) -> Result<()> {
        let field = align_field(align, 8);
        for half in 0..2 {
            address(self)?;
            self.push_half(value, half == 1)?;
            self.code.mem(emit::I64_STORE, field, offset + 8 * half);
        }
        Ok(())
    }

    /// Call the runtime function `name`, whose parameters after the address of the answer are
    /// `params`, with the arguments that `args` pushes, and put its answer in `result`.
    fn runtime(
        &mut self,
        name: &str,
        params: &[ValType],
        result: Value,
        args: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        let pair = is_pair(self.ty(result));
        let mut all = Vec::with_capacity(params.len() + 1);
        if pair {
            all.push(ValType::I32);
        }
        all.extend_from_slice(params);
        let results = if pair { Vec::new() } else { vec![valtype(self.ty(result))?] };
        let (symbol, _) = self.unit.libcall(name, FuncType { params: all, results });
        if pair {
            self.scratch_address(0);
        }
        args(self)?;
        self.code.call(symbol, false);
        if pair {
            self.load_scratch(result);
        } else {
            self.set(result);
        }
        Ok(())
    }

    /// Call the runtime function `name` with `args` as they are.
    fn runtime_on(&mut self, name: &str, args: &[Value], result: Value) -> Result<()> {
        let params = travel(args.iter().map(|&v| self.ty(v)))?;
        self.runtime(name, &params, result, |s| args.iter().try_for_each(|&v| s.push(v)))
    }

    /// An instruction that reads or gives a pair.
    #[allow(clippy::too_many_lines)]
    pub(super) fn pair(&mut self, inst: Inst, args: &[Value], results: &[Value]) -> Result<()> {
        let func = self.func;
        let data = &func[inst];
        let opcode = data.opcode;
        let arg = |i: usize| args[i];
        match opcode {
            Opcode::IConst | Opcode::FConst => {}
            Opcode::Add | Opcode::Sub => {
                let r = self.local[&results[0]];
                self.add_sub(opcode == Opcode::Sub, arg(0), arg(1), r)?;
            }
            Opcode::And | Opcode::Or | Opcode::Xor => {
                let op = match opcode {
                    Opcode::And => emit::I32_AND,
                    Opcode::Or => emit::I32_OR,
                    _ => emit::I32_XOR,
                };
                let r = self.local[&results[0]];
                for half in 0..2 {
                    self.push_half(arg(0), half == 1)?;
                    self.push_half(arg(1), half == 1)?;
                    self.code.op(int_op(op, true));
                    self.code.local_set(r + half);
                }
            }
            Opcode::Mul | Opcode::SDiv | Opcode::UDiv | Opcode::SRem | Opcode::URem => {
                let name = match opcode {
                    Opcode::Mul => "__multi3",
                    Opcode::SDiv => "__divti3",
                    Opcode::UDiv => "__udivti3",
                    Opcode::SRem => "__modti3",
                    _ => "__umodti3",
                };
                self.runtime_on(name, &args[..2], results[0])?;
            }
            Opcode::Shl | Opcode::LShr | Opcode::AShr if self.constant(arg(1)).is_some() => {
                let count = self.constant(arg(1)).unwrap_or_default();
                let r = self.local[&results[0]];
                self.shift_by(opcode, arg(0), (count & 127) as u32, r)?;
            }
            Opcode::Shl | Opcode::LShr | Opcode::AShr => {
                let name = match opcode {
                    Opcode::Shl => "__ashlti3",
                    Opcode::LShr => "__lshrti3",
                    _ => "__ashrti3",
                };
                let params = [ValType::I64, ValType::I64, ValType::I32];
                self.runtime(name, &params, results[0], |s| {
                    s.push(arg(0))?;
                    s.push_count(arg(1))
                })?;
            }
            Opcode::ICmp => {
                let Extra::IntPred(pred) = data.extra else { return Err("icmp".into()) };
                self.compare(pred, arg(0), arg(1))?;
                self.set(results[0]);
            }
            Opcode::Select => {
                let r = self.local[&results[0]];
                for half in 0..2 {
                    self.push_half(arg(1), half == 1)?;
                    self.push_half(arg(2), half == 1)?;
                    self.push_z(arg(0))?;
                    self.code.op(emit::SELECT);
                    self.code.local_set(r + half);
                }
            }
            Opcode::Trunc | Opcode::IntToPtr => {
                self.push_half(arg(0), false)?;
                if !self.wide(results[0]) {
                    self.code.op(emit::I32_WRAP_I64);
                }
                self.set(results[0]);
            }
            Opcode::SExt | Opcode::ZExt | Opcode::PtrToInt => {
                let signed = opcode == Opcode::SExt;
                if signed {
                    self.push_s(arg(0))?;
                } else {
                    self.push_z(arg(0))?;
                }
                self.resize(self.wide(arg(0)), true, signed);
                let r = self.local[&results[0]];
                self.code.local_set(r);
                if signed {
                    self.code.local_get(r);
                    self.code.i64_const(63);
                    self.code.op(int_op(emit::I32_SHR_S, true));
                } else {
                    self.code.i64_const(0);
                }
                self.code.local_set(r + 1);
            }
            Opcode::Bitcast | Opcode::Expect
                if is_pair(self.ty(arg(0))) && is_pair(self.ty(results[0])) =>
            {
                self.push(arg(0))?;
                self.set(results[0]);
            }
            Opcode::FNeg => {
                let r = self.local[&results[0]];
                self.push_half(arg(0), false)?;
                self.code.local_set(r);
                self.push_half(arg(0), true)?;
                self.code.i64_const(i64::MIN);
                self.code.op(int_op(emit::I32_XOR, true));
                self.code.local_set(r + 1);
            }
            Opcode::FAdd
            | Opcode::FSub
            | Opcode::FMul
            | Opcode::FDiv
            | Opcode::FRem
            | Opcode::Fma => {
                let name = match opcode {
                    Opcode::FAdd => "__addtf3",
                    Opcode::FSub => "__subtf3",
                    Opcode::FMul => "__multf3",
                    Opcode::FDiv => "__divtf3",
                    Opcode::FRem => "fmodl",
                    _ => "fmal",
                };
                self.runtime_on(name, args, results[0])?;
            }
            Opcode::FCmp => {
                let Extra::FloatPred(pred) = data.extra else { return Err("fcmp".into()) };
                self.fcmp_pair(pred, arg(0), arg(1))?;
                self.set(results[0]);
            }
            Opcode::FPExt | Opcode::FPTrunc => {
                let (from, to) = (self.ty(arg(0)), self.ty(results[0]));
                let name = match (valtype(from), valtype(to)) {
                    (Ok(ValType::F32), _) => "__extendsftf2",
                    (Ok(ValType::F64), _) => "__extenddftf2",
                    (_, Ok(ValType::F32)) => "__trunctfsf2",
                    (_, Ok(ValType::F64)) => "__trunctfdf2",
                    _ => return Err(format!("a conversion from {from} to {to}")),
                };
                self.runtime_on(name, &args[..1], results[0])?;
            }
            Opcode::FPToSI | Opcode::FPToUI => {
                let (from, to) = (self.ty(arg(0)), self.ty(results[0]));
                let uns = if opcode == Opcode::FPToUI { "uns" } else { "" };
                let name = if is_pair(from) {
                    let size = match to.bits() {
                        128 => "ti",
                        64 => "di",
                        _ => "si",
                    };
                    format!("__fix{uns}tf{size}")
                } else {
                    let size = if valtype(from)? == ValType::F32 { "sf" } else { "df" };
                    format!("__fix{uns}{size}ti")
                };
                self.runtime_on(&name, &args[..1], results[0])?;
            }
            Opcode::SIToFP | Opcode::UIToFP => {
                let (from, to) = (self.ty(arg(0)), self.ty(results[0]));
                let signed = opcode == Opcode::SIToFP;
                let uns = if signed { "" } else { "un" };
                if is_pair(to) && !is_pair(from) {
                    let wide = self.wide(arg(0));
                    let name = format!("__float{uns}{}tf", if wide { "di" } else { "si" });
                    let params = [if wide { ValType::I64 } else { ValType::I32 }];
                    self.runtime(&name, &params, results[0], |s| {
                        if signed { s.push_s(arg(0)) } else { s.push_z(arg(0)) }
                    })?;
                } else {
                    let size = match valtype(to) {
                        Ok(ValType::F32) => "sf",
                        Ok(ValType::F64) => "df",
                        _ => "tf",
                    };
                    self.runtime_on(&format!("__float{uns}ti{size}"), &args[..1], results[0])?;
                }
            }
            Opcode::Load | Opcode::AtomicLoad => {
                let align = self.mem_info(inst).map_or(16, |m| m.align);
                let field = align_field(align, 8);
                let r = self.local[&results[0]];
                for half in 0..2 {
                    self.push(arg(0))?;
                    self.code.mem(emit::I64_LOAD, field, 8 * half);
                    self.code.local_set(r + half);
                }
            }
            Opcode::Store | Opcode::AtomicStore => {
                let align = self.mem_info(inst).map_or(16, |m| m.align);
                self.store_pair_at(arg(0), 0, align, |s| s.push(arg(1)))?;
            }
            Opcode::VaArg => {
                let cursor = self.va_cursor(arg(0), 16)?;
                self.va_advance(arg(0), cursor, 16);
                let r = self.local[&results[0]];
                for half in 0..2 {
                    self.code.local_get(cursor);
                    self.code.mem(emit::I64_LOAD, 3, 8 * half);
                    self.code.local_set(r + half);
                }
            }
            Opcode::Ctlz | Opcode::Cttz | Opcode::Ctpop => {
                self.count(opcode, arg(0), results[0])?
            }
            Opcode::SAddOverflow
            | Opcode::UAddOverflow
            | Opcode::SSubOverflow
            | Opcode::USubOverflow
            | Opcode::SMulOverflow
            | Opcode::UMulOverflow => self.overflow_pair(opcode, arg(0), arg(1), results)?,
            other => {
                let ty = args.iter().chain(results).map(|&v| self.ty(v)).find(|&t| is_pair(t));
                let ty = ty.expect("an instruction with a pair");
                return Err(format!(
                    "the instruction {} on a {ty} is not translated yet",
                    other.name()
                ));
            }
        }
        Ok(())
    }

    /// The count of a shift as an `i32`. Only the low bits count, so the high half of a pair is
    /// not read.
    fn push_count(&mut self, count: Value) -> Result<()> {
        if is_pair(self.ty(count)) {
            self.push_half(count, false)?;
            self.code.op(emit::I32_WRAP_I64);
            Ok(())
        } else {
            self.push_i32(count)
        }
    }

    /// A shift of a pair by a count that is known, into the locals from `r`, which is written in
    /// place as clang writes it. Below 64, each half takes the bits that leave the other half,
    /// and from 64 up, one half is the other half shifted and the other half is zero or the sign.
    fn shift_by(&mut self, opcode: Opcode, a: Value, count: u32, r: u32) -> Result<()> {
        let shl = int_op(emit::I32_SHL, true);
        let shr_u = int_op(emit::I32_SHR_U, true);
        let shr_s = int_op(emit::I32_SHR_S, true);
        let or = int_op(emit::I32_OR, true);
        let left = opcode == Opcode::Shl;
        // The half that the bits go to, and the half that they come from, as `high` flags.
        let (to, from) = if left { (true, false) } else { (false, true) };
        let (out, back) = if left { (shl, shr_u) } else { (shr_u, shl) };
        let fill = if opcode == Opcode::AShr { shr_s } else { out };
        let local = |high: bool| r + u32::from(high);
        if count == 0 {
            self.push(a)?;
            self.code.local_set(r + 1);
            self.code.local_set(r);
        } else if count < 64 {
            self.push_half(a, to)?;
            self.code.i64_const(i64::from(count));
            self.code.op(if to { fill } else { out });
            self.push_half(a, from)?;
            self.code.i64_const(i64::from(64 - count));
            self.code.op(back);
            self.code.op(or);
            self.code.local_set(local(to));
            self.push_half(a, from)?;
            self.code.i64_const(i64::from(count));
            self.code.op(if from { fill } else { out });
            self.code.local_set(local(from));
        } else {
            self.push_half(a, from)?;
            self.code.i64_const(i64::from(count - 64));
            self.code.op(if from { fill } else { out });
            self.code.local_set(local(to));
            if opcode == Opcode::AShr {
                self.push_half(a, true)?;
                self.code.i64_const(63);
                self.code.op(shr_s);
            } else {
                self.code.i64_const(0);
            }
            self.code.local_set(local(from));
        }
        Ok(())
    }

    /// The sum or the difference of two pairs, into the locals from `r`. The carry of the low
    /// halves is a sum below an operand, and the borrow is a first operand below the second.
    fn add_sub(&mut self, sub: bool, a: Value, b: Value, r: u32) -> Result<()> {
        let op = int_op(if sub { emit::I32_SUB } else { emit::I32_ADD }, true);
        self.push_half(a, false)?;
        self.push_half(b, false)?;
        self.code.op(op);
        self.code.local_set(r);
        self.push_half(a, true)?;
        self.push_half(b, true)?;
        self.code.op(op);
        if sub {
            self.push_half(a, false)?;
            self.push_half(b, false)?;
        } else {
            self.code.local_get(r);
            self.push_half(a, false)?;
        }
        self.code.op(emit::I64_LT_U);
        self.code.op(emit::I64_EXTEND_I32_U);
        self.code.op(op);
        self.code.local_set(r + 1);
        Ok(())
    }

    /// Compare two pairs. Two pairs are equal when both halves are. For an order, the high
    /// halves decide, unless they are equal, and then the low halves decide as unsigned numbers.
    fn compare(&mut self, pred: IntPred, a: Value, b: Value) -> Result<()> {
        if matches!(pred, IntPred::Eq | IntPred::Ne) {
            for high in [false, true] {
                self.push_half(a, high)?;
                self.push_half(b, high)?;
                self.code.op(int_op(emit::I32_XOR, true));
            }
            self.code.op(int_op(emit::I32_OR, true));
            if pred == IntPred::Eq {
                self.code.op(emit::I64_EQZ);
            } else {
                self.code.i64_const(0);
                self.code.op(emit::I64_NE);
            }
            return Ok(());
        }
        self.push_half(a, false)?;
        self.push_half(b, false)?;
        self.code.op(int_compare(unsigned(pred), true));
        self.push_half(a, true)?;
        self.push_half(b, true)?;
        self.code.op(int_compare(pred, true));
        self.push_half(a, true)?;
        self.push_half(b, true)?;
        self.code.op(emit::I64_EQ);
        self.code.op(emit::SELECT);
        Ok(())
    }

    /// Compare two `long double` values by the comparisons of the runtime. `__eqtf2`, `__netf2`,
    /// `__lttf2` and `__letf2` give -1, 0 or 1 by the order, and 1 when the values are not
    /// ordered. `__gttf2` and `__getf2` give -1 when the values are not ordered. `__unordtf2`
    /// gives a value other than zero when they are not ordered.
    fn fcmp_pair(&mut self, pred: FloatPred, a: Value, b: Value) -> Result<()> {
        let test = |s: &mut Self, name: &str, pred: IntPred| -> Result<()> {
            let ty = FuncType { params: vec![ValType::I64; 4], results: vec![ValType::I32] };
            let (symbol, _) = s.unit.libcall(name, ty);
            s.push(a)?;
            s.push(b)?;
            s.code.call(symbol, false);
            s.code.i32_const(0);
            s.code.op(int_compare(pred, false));
            Ok(())
        };
        match pred {
            FloatPred::False => self.code.i32_const(0),
            FloatPred::True => self.code.i32_const(1),
            FloatPred::Oeq => test(self, "__eqtf2", IntPred::Eq)?,
            FloatPred::Une => test(self, "__netf2", IntPred::Ne)?,
            FloatPred::Olt => test(self, "__lttf2", IntPred::Slt)?,
            FloatPred::Ole => test(self, "__letf2", IntPred::Sle)?,
            FloatPred::Ogt => test(self, "__gttf2", IntPred::Sgt)?,
            FloatPred::Oge => test(self, "__getf2", IntPred::Sge)?,
            FloatPred::Ult => test(self, "__getf2", IntPred::Slt)?,
            FloatPred::Ule => test(self, "__gttf2", IntPred::Sle)?,
            FloatPred::Ugt => test(self, "__letf2", IntPred::Sgt)?,
            FloatPred::Uge => test(self, "__lttf2", IntPred::Sge)?,
            FloatPred::Uno => test(self, "__unordtf2", IntPred::Ne)?,
            FloatPred::Ord => test(self, "__unordtf2", IntPred::Eq)?,
            FloatPred::Ueq => {
                test(self, "__eqtf2", IntPred::Eq)?;
                test(self, "__unordtf2", IntPred::Ne)?;
                self.code.op(emit::I32_OR);
            }
            FloatPred::One => {
                test(self, "__eqtf2", IntPred::Ne)?;
                test(self, "__unordtf2", IntPred::Eq)?;
                self.code.op(emit::I32_AND);
            }
        }
        Ok(())
    }

    /// The leading zeros, the trailing zeros or the bits that are set, of a pair. The halves are
    /// counted as `i64` and the counts are put together.
    fn count(&mut self, opcode: Opcode, a: Value, result: Value) -> Result<()> {
        let clz = int_op(emit::I32_CLZ, true);
        let ctz = int_op(emit::I32_CTZ, true);
        let popcnt = int_op(emit::I32_POPCNT, true);
        match opcode {
            Opcode::Ctlz | Opcode::Cttz => {
                // The count of the half that is read first, plus 64 when that half is zero.
                let (first, op) = if opcode == Opcode::Ctlz { (true, clz) } else { (false, ctz) };
                self.push_half(a, !first)?;
                self.code.op(op);
                self.code.i64_const(64);
                self.code.op(int_op(emit::I32_ADD, true));
                self.push_half(a, first)?;
                self.code.op(op);
                self.push_half(a, first)?;
                self.code.op(emit::I64_EQZ);
                self.code.op(emit::SELECT);
            }
            _ => {
                self.push_half(a, false)?;
                self.code.op(popcnt);
                self.push_half(a, true)?;
                self.code.op(popcnt);
                self.code.op(int_op(emit::I32_ADD, true));
            }
        }
        if is_pair(self.ty(result)) {
            let r = self.local[&result];
            self.code.local_set(r);
            self.code.i64_const(0);
            self.code.local_set(r + 1);
        } else {
            if !self.wide(result) {
                self.code.op(emit::I32_WRAP_I64);
            }
            self.set(result);
        }
        Ok(())
    }

    /// An operation on two `i128` with a flag that says whether it overflowed.
    fn overflow_pair(
        &mut self,
        opcode: Opcode,
        a: Value,
        b: Value,
        results: &[Value],
    ) -> Result<()> {
        let (result, flag) = (results[0], results[1]);
        let r = self.local[&result];
        match opcode {
            Opcode::SAddOverflow
            | Opcode::UAddOverflow
            | Opcode::SSubOverflow
            | Opcode::USubOverflow => {
                let sub = matches!(opcode, Opcode::SSubOverflow | Opcode::USubOverflow);
                self.add_sub(sub, a, b, r)?;
                match opcode {
                    Opcode::UAddOverflow => self.compare(IntPred::Ult, result, a)?,
                    Opcode::USubOverflow => self.compare(IntPred::Ult, a, b)?,
                    _ => {
                        // As for 64 bits, from the sign of the high halves: (a ^ r) & (b ^ r) for
                        // a sum and (a ^ r) & (a ^ b) for a difference.
                        let other = if sub { (a, b) } else { (b, result) };
                        self.push_half(a, true)?;
                        self.push_half(result, true)?;
                        self.code.op(int_op(emit::I32_XOR, true));
                        self.push_half(other.0, true)?;
                        self.push_half(other.1, true)?;
                        self.code.op(int_op(emit::I32_XOR, true));
                        self.code.op(int_op(emit::I32_AND, true));
                        self.code.i64_const(0);
                        self.code.op(emit::I64_LT_S);
                    }
                }
            }
            Opcode::SMulOverflow => {
                // `__muloti4` writes the flag to an `int` whose address is its last parameter.
                let params = [ValType::I64, ValType::I64, ValType::I64, ValType::I64, ValType::I32];
                self.runtime("__muloti4", &params, result, |s| {
                    s.push(a)?;
                    s.push(b)?;
                    s.scratch_address(16);
                    Ok(())
                })?;
                let fp = self.frame_pointer();
                self.code.local_get(fp);
                self.code.mem(emit::I32_LOAD, 2, self.scratch() + 16);
                self.code.i32_const(0);
                self.code.op(emit::I32_NE);
            }
            _ => {
                // The product, and then a != 0 && r / a != b, with no division when a is zero.
                self.runtime_on("__multi3", &[a, b], result)?;
                self.push_half(a, false)?;
                self.push_half(a, true)?;
                self.code.op(int_op(emit::I32_OR, true));
                self.code.op(emit::I64_EQZ);
                self.code.open(emit::IF, Some(ValType::I32));
                self.code.i32_const(0);
                self.code.op(emit::ELSE);
                let ty = FuncType {
                    params: vec![
                        ValType::I32,
                        ValType::I64,
                        ValType::I64,
                        ValType::I64,
                        ValType::I64,
                    ],
                    results: Vec::new(),
                };
                let (udiv, _) = self.unit.libcall("__udivti3", ty);
                self.scratch_address(0);
                self.push(result)?;
                self.push(a)?;
                self.code.call(udiv, false);
                let (fp, at) = (self.frame_pointer(), self.scratch());
                for half in 0..2 {
                    self.code.local_get(fp);
                    self.code.mem(emit::I64_LOAD, 3, at + 8 * half);
                    self.push_half(b, half == 1)?;
                    self.code.op(int_op(emit::I32_XOR, true));
                }
                self.code.op(int_op(emit::I32_OR, true));
                self.code.i64_const(0);
                self.code.op(emit::I64_NE);
                self.code.op(emit::END);
            }
        }
        self.set(flag);
        Ok(())
    }
}
