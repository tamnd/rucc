//! The scalar `__builtin_wasm_*` functions of clang, each of which is one wasm instruction.
//!
//! This is section 11.5 of the WebAssembly notes. The front end knows the names on the wasm rows
//! only, from their rows in `rucc-gnu/features.toml`, and gives each call the type of clang 23. The
//! call reaches the backend as a call to the name, and the selector writes the instruction in its
//! place, as clang does. No object has a symbol of the name, so a call that the selector did not
//! see would not link. The builtins of the other groups (reference types, atomics, exceptions,
//! SIMD) have no row yet, so a call to one of them is a call to an undeclared function.

use rucc_ir::Value;
use rucc_object::wasm::ValType;
use rucc_target::wasm::Feature;

use super::{Lower, Result};

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

/// Whether a call to `name` is one that [`Lower::builtin`] writes in place.
pub(super) fn is_builtin(name: &str) -> bool {
    write(name).is_some()
}

impl Lower<'_, '_> {
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
