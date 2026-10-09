//! The scalar `__builtin_wasm_*` functions of clang, each of which is one wasm instruction.
//!
//! This is section 11.5 of the WebAssembly notes. The front end knows the names on the wasm rows
//! only, from their rows in `rucc-gnu/features.toml`, and gives each call the type of clang 23. The
//! call reaches the backend as a call to the name, and the selector writes the instruction in its
//! place, as clang does. No object has a symbol of the name, so a call that the selector did not
//! see would not link. The builtins of the other groups (reference types, atomics, exceptions,
//! SIMD) have no row yet, so a call to one of them is a call to an undeclared function.
//!
//! The maths functions of the C library that wasm has an instruction for are written the same way,
//! as clang does. That is `sqrt`, `ceil`, `floor`, `trunc`, `rint`, `nearbyint`, `fabs` and
//! `copysign`, and the `float` form of each. These have a symbol in the library, so the call stays
//! a call when the selector cannot write the instruction. See [`Lower::libm`].

use rucc_base::Symbol;
use rucc_ir::{Extra, Flags, Inst, SymbolRef, Value};
use rucc_object::wasm::{FuncType, ValType};
use rucc_target::wasm::Feature;

use super::{Lower, Result};
use crate::{emit, valtype};

/// What one builtin writes after its arguments are on the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Write {
    /// One instruction with this opcode.
    Op(u8),
    /// One instruction with this opcode behind the `0xfc` prefix, which needs the feature
    /// `nontrapping-fptoint`.
    Saturating(u32),
    /// `memory.size` or `memory.grow` of the memory with the index that the first argument gives.
    Memory(u8),
    /// `global.get` of the global with this name, which `wasm-ld` makes, and whether the global is
    /// mutable.
    Global(&'static str, bool),
}

/// What the builtin `name` writes, or nothing when `name` is not a scalar builtin of clang.
///
/// The opcodes are those of the Wasm 3.0 binary format, section 5.4. The 8 trapping conversions
/// start at `i32.trunc_f32_s` (0xa8) for `i32` and at `i64.trunc_f32_s` (0xae) for `i64`, in the
/// order f32 signed, f32 unsigned, f64 signed and f64 unsigned. The 8 saturating conversions are
/// `0xfc 0` to `0xfc 7` in the same order.
fn write(name: &str) -> Option<Write> {
    let name = name.strip_prefix("__builtin_wasm_")?;
    Some(match name {
        "memory_size" => Write::Memory(0x3f),
        "memory_grow" => Write::Memory(0x40),
        "min_f32" => Write::Op(0x96),
        "max_f32" => Write::Op(0x97),
        "min_f64" => Write::Op(0xa4),
        "max_f64" => Write::Op(0xa5),
        "tls_base" => Write::Global("__tls_base", true),
        "tls_size" => Write::Global("__tls_size", false),
        "tls_align" => Write::Global("__tls_align", false),
        _ => {
            let (saturating, rest) = match name.strip_prefix("trunc_saturate_") {
                Some(rest) => (true, rest),
                None => (false, name.strip_prefix("trunc_")?),
            };
            let at = [
                "s_i32_f32",
                "u_i32_f32",
                "s_i32_f64",
                "u_i32_f64",
                "s_i64_f32",
                "u_i64_f32",
                "s_i64_f64",
                "u_i64_f64",
            ]
            .iter()
            .position(|&form| form == rest)? as u8;
            if saturating {
                Write::Saturating(u32::from(at))
            } else if at < 4 {
                Write::Op(0xa8 + at)
            } else {
                Write::Op(0xae + at - 4)
            }
        }
    })
}

/// The instruction of the maths function `name` of the C library, the type of its operands and
/// its result, and the number of its operands. Nothing when wasm has no instruction for it.
///
/// `rint` and `nearbyint` are both `nearest`, because wasm has one rounding mode, which is to the
/// nearest even value, and no inexact flag. `round` is not here, because it rounds a half away
/// from zero, and no instruction does that.
fn libm(name: &str) -> Option<(u8, ValType, usize)> {
    Some(match name {
        "fabsf" => (0x8b, ValType::F32, 1),
        "ceilf" => (0x8d, ValType::F32, 1),
        "floorf" => (0x8e, ValType::F32, 1),
        "truncf" => (0x8f, ValType::F32, 1),
        "rintf" | "nearbyintf" => (0x90, ValType::F32, 1),
        "sqrtf" => (0x91, ValType::F32, 1),
        "copysignf" => (0x98, ValType::F32, 2),
        "fabs" => (0x99, ValType::F64, 1),
        "ceil" => (0x9b, ValType::F64, 1),
        "floor" => (0x9c, ValType::F64, 1),
        "trunc" => (0x9d, ValType::F64, 1),
        "rint" | "nearbyint" => (0x9e, ValType::F64, 1),
        "sqrt" => (0x9f, ValType::F64, 1),
        "copysign" => (0xa6, ValType::F64, 2),
        _ => return None,
    })
}

/// Whether a call to `name` is one that [`Lower::builtin`] writes in place.
pub(super) fn is_builtin(name: &str) -> bool {
    write(name).is_some()
}

impl Lower<'_, '_> {
    /// The opcode of the instruction that the call `inst` to a maths function is written as, or
    /// nothing when it stays a call or when it is the checked `sqrt` of [`Lower::maths`].
    pub(super) fn libm(&self, inst: Inst) -> Option<u8> {
        self.maths(inst).and_then(|(op, checked)| (!checked).then_some(op))
    }

    /// The opcode of the instruction that the call `inst` to a maths function is written as, and
    /// whether the answer of the instruction must be checked, or nothing when it stays a call.
    ///
    /// It stays a call when the call does not have [`Flags::LIBRARY`], when the module defines the
    /// function, and when the call does not have the type of the C library's function. Under `-fmath-errno`, the answer of `sqrt` and `sqrtf` is checked,
    /// because the instruction does not set `errno`. [`Lower::checked_sqrt`] writes it.
    pub(super) fn maths(&self, inst: Inst) -> Option<(u8, bool)> {
        let Extra::Call(info) = self.func[inst].extra else { return None };
        let info = self.func[info];
        let callee = info.callee?;
        let name = self.unit.names.resolve(callee);
        let (op, ty, count) = libm(name)?;
        if !self.func[inst].flags.contains(Flags::LIBRARY)
            || matches!(self.unit.ir.lookup(callee),
                Some(SymbolRef::Func(f)) if !self.unit.ir[f].is_declaration())
        {
            return None;
        }
        let sig = &self.func[info.signature];
        let typed = |params: &[rucc_ir::Param], count| {
            params.len() == count && params.iter().all(|p| valtype(p.ty) == Ok(ty))
        };
        let fits = !sig.variadic
            && typed(&sig.params, count)
            && typed(&sig.returns, 1)
            && self.args(inst).len() == count
            && self.args(inst).iter().all(|&arg| valtype(self.ty(arg)) == Ok(ty));
        fits.then_some((op, self.unit.math_errno && name.starts_with("sqrt")))
    }

    /// The square root `op` of `arg` that calls the function `callee` only when the answer is a
    /// NaN, which leaves the answer on the stack. This is what clang writes under `-fmath-errno`.
    /// The answer is a NaN when the operand is a NaN or less than zero, and only the function
    /// sets `errno` for those.
    pub(super) fn checked_sqrt(&mut self, op: u8, arg: Value, callee: Symbol) -> Result<()> {
        let ty = valtype(self.ty(arg))?;
        let eq = if ty == ValType::F32 { 0x5b } else { 0x61 };
        let operand = self.new_local(ty);
        let answer = self.new_local(ty);
        self.push(arg)?;
        self.code.local_set(operand);
        self.code.open(emit::BLOCK, None);
        self.code.local_get(operand);
        self.code.op(op);
        self.code.local_tee(answer);
        self.code.local_get(answer);
        self.code.op(eq);
        self.code.br_if(0);
        self.code.local_get(operand);
        let (symbol, _) = self.unit.function(callee).or_else(|_| {
            let name = self.unit.name(callee);
            Ok::<_, String>(
                self.unit.libcall(&name, FuncType { params: vec![ty], results: vec![ty] }),
            )
        })?;
        self.code.call(symbol, false);
        self.code.local_set(answer);
        self.code.end(true);
        self.code.local_get(answer);
        Ok(())
    }

    /// The instruction of the builtin `name` in place of a call, which leaves the answer on the
    /// stack.
    pub(super) fn builtin(&mut self, name: &str, args: &[Value]) -> Result<()> {
        let Some(write) = write(name) else { return Err(format!("the call to `{name}`")) };
        match write {
            Write::Op(op) => {
                for &arg in args {
                    self.push(arg)?;
                }
                self.code.op(op);
            }
            Write::Saturating(op) => {
                // clang refuses the call in the same words.
                if !self.unit.features.has(Feature::NontrappingFptoint) {
                    return Err(format!("`{name}` needs target feature nontrapping-fptoint"));
                }
                self.push(args[0])?;
                self.code.prefixed(op);
            }
            Write::Memory(op) => {
                // A module of wasm32-wasip1 has one memory, and clang takes only the constant 0.
                if args.first().and_then(|&index| self.constant(index)) != Some(0) {
                    return Err(format!("the memory index of `{name}` is not the constant 0"));
                }
                for &arg in &args[1..] {
                    self.push_i32(arg)?;
                }
                self.code.op(op);
                self.code.op(0);
            }
            Write::Global("__tls_base", _) if self.unit.thread_context => {
                let base = self.unit.context_call("__wasm_get_tls_base");
                self.code.call(base, false);
            }
            Write::Global(global, mutable) => {
                let symbol = self.unit.linker_global(global, ValType::I32, mutable);
                self.code.global_get(symbol);
            }
        }
        Ok(())
    }
}
