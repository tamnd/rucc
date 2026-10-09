//! The scalar `__builtin_wasm_*` functions of clang, each of which is one wasm instruction.
//!
//! This is section 11.5 of the WebAssembly notes. The front end knows the names on the wasm rows
//! only, from their rows in `rucc-gnu/features.toml`, and gives each call the type of clang 23. The
//! call reaches the backend as a call to the name, and the selector writes the instruction in its
//! place, as clang does. No object has a symbol of the name, so a call that the selector did not
//! see would not link. The vector builtins of [`VECTOR`] are written the same way, with `-msimd128`,
//! and so are the calls of [`ELEMENTWISE`] and [`CONVERT`] that `rucc-lower` makes for the
//! generic builtins.
//! The builtins of the other groups (reference types, atomics, exceptions) have no row yet, so a
//! call to one of them is a call to an undeclared function.
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
    /// The SIMD instruction with this name, which needs this feature.
    Simd(&'static str, Feature),
    /// `i8x16.shuffle` of the first two arguments, with the next 16 arguments, which are
    /// constants, as the lanes that it picks.
    Shuffle,
}

/// The vector builtins of clang 23 that have a type of their own, each with the SIMD instruction
/// that it is. A name that starts with `relaxed_` is an instruction of relaxed SIMD.
///
/// `__builtin_wasm_shuffle_i8x16` is not here, because its 16 lanes are constants and not values
/// on the stack. It is [`Write::Shuffle`]. The other operations of `<wasm_simd128.h>` are plain C or generic builtins in clang, and not names of
/// their own.
const VECTOR: &[(&str, &str)] = &[
    ("abs_i8x16", "i8x16.abs"),
    ("abs_i16x8", "i16x8.abs"),
    ("abs_i32x4", "i32x4.abs"),
    ("abs_i64x2", "i64x2.abs"),
    ("abs_f32x4", "f32x4.abs"),
    ("abs_f64x2", "f64x2.abs"),
    ("all_true_i8x16", "i8x16.all_true"),
    ("bitmask_i8x16", "i8x16.bitmask"),
    ("all_true_i16x8", "i16x8.all_true"),
    ("bitmask_i16x8", "i16x8.bitmask"),
    ("all_true_i32x4", "i32x4.all_true"),
    ("bitmask_i32x4", "i32x4.bitmask"),
    ("all_true_i64x2", "i64x2.all_true"),
    ("bitmask_i64x2", "i64x2.bitmask"),
    ("any_true_v128", "v128.any_true"),
    ("bitselect", "v128.bitselect"),
    ("avgr_u_i8x16", "i8x16.avgr_u"),
    ("avgr_u_i16x8", "i16x8.avgr_u"),
    ("ceil_f32x4", "f32x4.ceil"),
    ("floor_f32x4", "f32x4.floor"),
    ("trunc_f32x4", "f32x4.trunc"),
    ("nearest_f32x4", "f32x4.nearest"),
    ("sqrt_f32x4", "f32x4.sqrt"),
    ("min_f32x4", "f32x4.min"),
    ("max_f32x4", "f32x4.max"),
    ("pmin_f32x4", "f32x4.pmin"),
    ("pmax_f32x4", "f32x4.pmax"),
    ("ceil_f64x2", "f64x2.ceil"),
    ("floor_f64x2", "f64x2.floor"),
    ("trunc_f64x2", "f64x2.trunc"),
    ("nearest_f64x2", "f64x2.nearest"),
    ("sqrt_f64x2", "f64x2.sqrt"),
    ("min_f64x2", "f64x2.min"),
    ("max_f64x2", "f64x2.max"),
    ("pmin_f64x2", "f64x2.pmin"),
    ("pmax_f64x2", "f64x2.pmax"),
    ("dot_s_i32x4_i16x8", "i32x4.dot_i16x8_s"),
    ("extadd_pairwise_i8x16_s_i16x8", "i16x8.extadd_pairwise_i8x16_s"),
    ("extadd_pairwise_i8x16_u_i16x8", "i16x8.extadd_pairwise_i8x16_u"),
    ("extadd_pairwise_i16x8_s_i32x4", "i32x4.extadd_pairwise_i16x8_s"),
    ("extadd_pairwise_i16x8_u_i32x4", "i32x4.extadd_pairwise_i16x8_u"),
    ("narrow_s_i8x16_i16x8", "i8x16.narrow_i16x8_s"),
    ("narrow_u_i8x16_i16x8", "i8x16.narrow_i16x8_u"),
    ("narrow_s_i16x8_i32x4", "i16x8.narrow_i32x4_s"),
    ("narrow_u_i16x8_i32x4", "i16x8.narrow_i32x4_u"),
    ("q15mulr_sat_s_i16x8", "i16x8.q15mulr_sat_s"),
    ("swizzle_i8x16", "i8x16.swizzle"),
    ("trunc_saturate_s_i32x4_f32x4", "i32x4.trunc_sat_f32x4_s"),
    ("trunc_saturate_u_i32x4_f32x4", "i32x4.trunc_sat_f32x4_u"),
    ("trunc_sat_s_zero_f64x2_i32x4", "i32x4.trunc_sat_f64x2_s_zero"),
    ("trunc_sat_u_zero_f64x2_i32x4", "i32x4.trunc_sat_f64x2_u_zero"),
    ("relaxed_swizzle_i8x16", "i8x16.relaxed_swizzle"),
    ("relaxed_trunc_s_i32x4_f32x4", "i32x4.relaxed_trunc_f32x4_s"),
    ("relaxed_trunc_u_i32x4_f32x4", "i32x4.relaxed_trunc_f32x4_u"),
    ("relaxed_trunc_s_zero_i32x4_f64x2", "i32x4.relaxed_trunc_f64x2_s_zero"),
    ("relaxed_trunc_u_zero_i32x4_f64x2", "i32x4.relaxed_trunc_f64x2_u_zero"),
    ("relaxed_madd_f32x4", "f32x4.relaxed_madd"),
    ("relaxed_nmadd_f32x4", "f32x4.relaxed_nmadd"),
    ("relaxed_min_f32x4", "f32x4.relaxed_min"),
    ("relaxed_max_f32x4", "f32x4.relaxed_max"),
    ("relaxed_madd_f64x2", "f64x2.relaxed_madd"),
    ("relaxed_nmadd_f64x2", "f64x2.relaxed_nmadd"),
    ("relaxed_min_f64x2", "f64x2.relaxed_min"),
    ("relaxed_max_f64x2", "f64x2.relaxed_max"),
    ("relaxed_laneselect_i8x16", "i8x16.relaxed_laneselect"),
    ("relaxed_laneselect_i16x8", "i16x8.relaxed_laneselect"),
    ("relaxed_laneselect_i32x4", "i32x4.relaxed_laneselect"),
    ("relaxed_laneselect_i64x2", "i64x2.relaxed_laneselect"),
    ("relaxed_q15mulr_s_i16x8", "i16x8.relaxed_q15mulr_s"),
    ("relaxed_dot_i8x16_i7x16_s_i16x8", "i16x8.relaxed_dot_i8x16_i7x16_s"),
    ("relaxed_dot_i8x16_i7x16_add_s_i32x4", "i32x4.relaxed_dot_i8x16_i7x16_add_s"),
];

/// The names that `rucc-lower` calls for a `__builtin_elementwise_*` builtin on a vector of
/// sixteen bytes, each with the SIMD instruction that it is. These are the names that clang 13
/// gave the builtins, before clang wrote the header with the generic builtins. They have no row in
/// `features.toml`, so a program cannot call one of them unless it declares it.
const ELEMENTWISE: &[(&str, &str)] = &[
    ("min_s_i8x16", "i8x16.min_s"),
    ("min_u_i8x16", "i8x16.min_u"),
    ("max_s_i8x16", "i8x16.max_s"),
    ("max_u_i8x16", "i8x16.max_u"),
    ("min_s_i16x8", "i16x8.min_s"),
    ("min_u_i16x8", "i16x8.min_u"),
    ("max_s_i16x8", "i16x8.max_s"),
    ("max_u_i16x8", "i16x8.max_u"),
    ("min_s_i32x4", "i32x4.min_s"),
    ("min_u_i32x4", "i32x4.min_u"),
    ("max_s_i32x4", "i32x4.max_s"),
    ("max_u_i32x4", "i32x4.max_u"),
    ("add_sat_s_i8x16", "i8x16.add_sat_s"),
    ("add_sat_u_i8x16", "i8x16.add_sat_u"),
    ("sub_sat_s_i8x16", "i8x16.sub_sat_s"),
    ("sub_sat_u_i8x16", "i8x16.sub_sat_u"),
    ("add_sat_s_i16x8", "i16x8.add_sat_s"),
    ("add_sat_u_i16x8", "i16x8.add_sat_u"),
    ("sub_sat_s_i16x8", "i16x8.sub_sat_s"),
    ("sub_sat_u_i16x8", "i16x8.sub_sat_u"),
    ("popcnt_i8x16", "i8x16.popcnt"),
];

/// The calls that `rucc-lower` makes for `__builtin_convertvector` with `-msimd128`, each with the
/// SIMD instruction that it is. The names have the shape of the names of [`VECTOR`]: the operation,
/// the sign, the operand and the answer.
const CONVERT: &[(&str, &str)] = &[
    ("convert_s_i32x4_f32x4", "f32x4.convert_i32x4_s"),
    ("convert_u_i32x4_f32x4", "f32x4.convert_i32x4_u"),
    ("convert_low_s_i32x4_f64x2", "f64x2.convert_low_i32x4_s"),
    ("convert_low_u_i32x4_f64x2", "f64x2.convert_low_i32x4_u"),
    ("promote_low_f32x4_f64x2", "f64x2.promote_low_f32x4"),
    ("demote_zero_f64x2_f32x4", "f32x4.demote_f64x2_zero"),
    ("extend_low_s_i8x16_i16x8", "i16x8.extend_low_i8x16_s"),
    ("extend_low_u_i8x16_i16x8", "i16x8.extend_low_i8x16_u"),
    ("extend_low_s_i16x8_i32x4", "i32x4.extend_low_i16x8_s"),
    ("extend_low_u_i16x8_i32x4", "i32x4.extend_low_i16x8_u"),
    ("extend_low_s_i32x4_i64x2", "i64x2.extend_low_i32x4_s"),
    ("extend_low_u_i32x4_i64x2", "i64x2.extend_low_i32x4_u"),
];

/// What the builtin `name` writes, or nothing when `name` is not a builtin of clang.
///
/// The opcodes are those of the Wasm 3.0 binary format, section 5.4. The 8 trapping conversions
/// start at `i32.trunc_f32_s` (0xa8) for `i32` and at `i64.trunc_f32_s` (0xae) for `i64`, in the
/// order f32 signed, f32 unsigned, f64 signed and f64 unsigned. The 8 saturating conversions are
/// `0xfc 0` to `0xfc 7` in the same order.
fn write(name: &str) -> Option<Write> {
    let name = name.strip_prefix("__builtin_wasm_")?;
    if let Some(&(_, op)) =
        VECTOR.iter().chain(ELEMENTWISE).chain(CONVERT).find(|&&(form, _)| form == name)
    {
        let feature = match name.starts_with("relaxed_") {
            true => Feature::RelaxedSimd,
            false => Feature::Simd128,
        };
        return Some(Write::Simd(op, feature));
    }
    Some(match name {
        "shuffle_i8x16" => Write::Shuffle,
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
    /// What the call `inst` writes, or nothing when it is not a call to a builtin.
    fn written(&self, inst: Inst) -> Option<Write> {
        let Extra::Call(info) = self.func[inst].extra else { return None };
        write(self.unit.names.resolve(self.func[info].callee?))
    }

    /// Whether the call `inst` is a builtin whose code pushes each argument once, in order, and
    /// then writes one instruction.
    pub(super) fn builtin_pushes_once(&self, inst: Inst) -> bool {
        matches!(
            self.written(inst),
            Some(Write::Op(_) | Write::Saturating(_) | Write::Simd(..) | Write::Shuffle)
        )
    }

    /// Whether the call `inst` is `i8x16.shuffle`, which pushes its two vectors and writes its
    /// 16 lanes as immediates.
    pub(super) fn shuffle_builtin(&self, inst: Inst) -> bool {
        matches!(self.written(inst), Some(Write::Shuffle))
    }

    /// Whether `value` is the answer of `all_true` or `any_true`, which is 0 or 1.
    pub(super) fn boolean_builtin(&self, value: Value) -> bool {
        let Some((inst, _)) = self.def(value) else { return false };
        matches!(self.written(inst),
            Some(Write::Simd(op, _)) if op.ends_with(".all_true") || op.ends_with(".any_true"))
    }

    /// Whether the call `inst` is a vector builtin, which has no effect, reads no memory and
    /// cannot trap.
    pub(super) fn simd_builtin(&self, inst: Inst) -> bool {
        matches!(self.written(inst), Some(Write::Simd(..) | Write::Shuffle))
    }

    /// The extending load that does the load of 8 bytes and the call `inst` together, when the
    /// call is the extend of the low half of a vector: `i16x8.load8x8_s` for
    /// `i16x8.extend_low_i8x16_s`, up to `i64x2.load32x2_u`.
    pub(super) fn extending_load(&self, inst: Inst) -> Option<String> {
        let Some(Write::Simd(op, _)) = self.written(inst) else { return None };
        let (shape, rest) = op.split_once('.')?;
        let sign = rest.strip_prefix("extend_low_")?.rsplit('_').next()?;
        let (lane, count) = shape.strip_prefix('i')?.split_once('x')?;
        let half = lane.parse::<u32>().ok()? / 2;
        Some(format!("{shape}.load{half}x{count}_{sign}"))
    }

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
            Write::Simd(op, feature) => {
                // clang refuses the call in the same words.
                if !self.unit.features.has(feature) {
                    return Err(format!("`{name}` needs target feature {}", feature.name()));
                }
                for &arg in args {
                    self.push(arg)?;
                }
                self.simd(op)?;
            }
            Write::Shuffle => {
                // clang refuses the call in the same words.
                if !self.unit.features.has(Feature::Simd128) {
                    return Err(format!("`{name}` needs target feature simd128"));
                }
                let &[a, b, ref lanes @ ..] = args else { return Err(format!("`{name}`")) };
                if lanes.len() != 16 {
                    return Err(format!("`{name}` with {} lanes", lanes.len()));
                }
                let Some(lanes) =
                    lanes.iter().map(|&lane| self.constant(lane)).collect::<Option<Vec<_>>>()
                else {
                    return Err(format!("argument to '{name}' must be a constant integer"));
                };
                self.push(a)?;
                self.push(b)?;
                self.simd("i8x16.shuffle")?;
                // A lane past the 32 bytes of the two operands is a lane that clang leaves
                // undefined, and it writes 0 there, as this does.
                for lane in lanes {
                    self.code.op(u8::try_from(lane).ok().filter(|&lane| lane < 32).unwrap_or(0));
                }
            }
        }
        Ok(())
    }
}
