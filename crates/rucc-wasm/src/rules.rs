//! The selection rules of `rules/wasm32.rules`, as the table that the build script makes from
//! them, and what each `w.` head of the rules writes.
//!
//! Design: the WebAssembly notes, section 6.5 (decision D14). Each rule has a `spec`, and
//! `rucc-verify` checks it against `rules/wasm32.model` with an SMT solver, so nothing enters the
//! rule set that is not proved. The selector tries the table first for each instruction that
//! [`tried`] names, and walks the replacement of the rule that fires in pre-order, which is the
//! order of the code. An instruction that no rule matches goes through the code of `select.rs`.

use rucc_base::rules::{Node, Piece, Rule, Table};
use rucc_ir::Opcode;

use crate::emit;

/// The table that the build script makes from `rules/wasm32.rules`.
mod table {
    // The guards are written as the comparisons that the rules write, and the generated items are
    // `pub` for a crate that exports them, which this one does not.
    #![allow(clippy::manual_range_contains, dead_code, unreachable_pub)]

    include!(concat!(env!("OUT_DIR"), "/wasm32.rs"));
}

pub(crate) use table::TABLE;

/// What one head of a replacement writes, after its arguments are on the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Head {
    /// One instruction with this opcode.
    Op(u8),
    /// A sign extension from this many bits, which is `i32.extend8_s` or `i32.extend16_s`, or a
    /// pair of shifts without the sign-extension operations.
    SignExtend(u32),
    /// A mask that keeps this many bits, left out when the value is already clean.
    ZeroExtend(u32),
    /// The mask of the count of a shift of this many bits, which keeps the bits below the width.
    /// It is left out when the count is a constant below the width.
    Count(u32),
    /// Nothing: the value stays as it is and its bits above the narrow width are not defined.
    Low,
}

/// The opcodes whose instructions the rules can match. The table names each of them, so asking it
/// about another opcode only costs the time to find no rule.
pub(crate) fn tried(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Shl
            | Opcode::LShr
            | Opcode::AShr
            | Opcode::SDiv
            | Opcode::UDiv
            | Opcode::SRem
            | Opcode::URem
            | Opcode::ICmp
            | Opcode::Select
            | Opcode::SExt
            | Opcode::ZExt
            | Opcode::Trunc
            | Opcode::FAdd
            | Opcode::FSub
            | Opcode::FMul
            | Opcode::FDiv
            | Opcode::PtrAdd
    )
}

/// What the head `name` of a replacement writes, or nothing for a name that the model does not
/// have. The `i64` arithmetic is the `i32` opcode moved by [`emit::I64_FROM_I32`], and the `i64`
/// comparisons start at [`emit::I64_EQ`] in the order of the `i32` ones.
pub(crate) fn head(name: &str) -> Option<Head> {
    let name = name.strip_prefix("w.")?;
    let compare = |base: u8, pred: &str| -> Option<u8> {
        let at = ["eq", "ne", "lt_s", "lt_u", "gt_s", "gt_u", "le_s", "le_u", "ge_s", "ge_u"]
            .iter()
            .position(|&p| p == pred)?;
        Some(base + at as u8)
    };
    let arithmetic = |op: &str| -> Option<u8> {
        Some(match op {
            "add" => emit::I32_ADD,
            "sub" => emit::I32_SUB,
            "mul" => emit::I32_MUL,
            "div_s" => emit::I32_DIV_S,
            "div_u" => emit::I32_DIV_U,
            "rem_s" => emit::I32_REM_S,
            "rem_u" => emit::I32_REM_U,
            "and" => emit::I32_AND,
            "or" => emit::I32_OR,
            "xor" => emit::I32_XOR,
            "shl" => emit::I32_SHL,
            "shr_s" => emit::I32_SHR_S,
            "shr_u" => emit::I32_SHR_U,
            _ => return None,
        })
    };
    let float = |base: u8, op: &str| -> Option<u8> {
        let at = ["add", "sub", "mul", "div"].iter().position(|&p| p == op)?;
        Some(base + at as u8)
    };
    Some(match name {
        "select" => Head::Op(emit::SELECT),
        "i32_wrap_i64" => Head::Op(emit::I32_WRAP_I64),
        "i64_extend_i32_s" => Head::Op(emit::I64_EXTEND_I32_S),
        "i64_extend_i32_u" => Head::Op(emit::I64_EXTEND_I32_U),
        "i32_shl_8" | "i32_shl_16" => Head::Op(emit::I32_SHL),
        "i32_extend8_s" => Head::SignExtend(8),
        "i32_extend16_s" => Head::SignExtend(16),
        "zext_1" => Head::ZeroExtend(1),
        "zext_8" => Head::ZeroExtend(8),
        "zext_16" => Head::ZeroExtend(16),
        "count_8" => Head::Count(8),
        "count_16" => Head::Count(16),
        "low_1" | "low_8" | "low_16" => Head::Low,
        _ => {
            let (ty, op) = name.split_once('_')?;
            Head::Op(match ty {
                "i32" => arithmetic(op).or_else(|| compare(emit::I32_EQ, op))?,
                "i64" => arithmetic(op)
                    .map(|op| op + emit::I64_FROM_I32)
                    .or_else(|| compare(emit::I64_EQ, op))?,
                "f32" => float(emit::F32_ADD, op)?,
                "f64" => float(emit::F64_ADD, op)?,
                _ => return None,
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every head that a rule writes is one that the selector can write, so no rule that the
    /// verifier proved fails when it fires.
    #[test]
    fn every_head_of_the_rules_has_code() {
        for rule in TABLE.rules {
            for piece in rule.replacement {
                match piece {
                    Piece::App { head: name, .. } => {
                        assert!(head(name).is_some(), "line {}: no code for {name}", rule.line);
                    }
                    Piece::Var { .. } => {}
                    other => panic!("line {}: a replacement has {other:?}", rule.line),
                }
            }
        }
    }

    /// The opcodes, against the binary format of Wasm 3.0, section 5.4.
    #[test]
    fn the_heads_have_the_opcodes_of_the_specification() {
        for (name, opcode) in [
            ("w.i32_add", 0x6a),
            ("w.i32_shr_u", 0x76),
            ("w.i64_add", 0x7c),
            ("w.i64_rem_u", 0x82),
            ("w.i64_shr_u", 0x88),
            ("w.i32_eq", 0x46),
            ("w.i32_ge_u", 0x4f),
            ("w.i64_eq", 0x51),
            ("w.i64_ge_u", 0x5a),
            ("w.f32_add", 0x92),
            ("w.f32_div", 0x95),
            ("w.f64_div", 0xa3),
            ("w.select", 0x1b),
            ("w.i32_wrap_i64", 0xa7),
            ("w.i64_extend_i32_u", 0xad),
            ("w.i32_shl_8", 0x74),
        ] {
            assert_eq!(head(name), Some(Head::Op(opcode)), "{name}");
        }
        assert_eq!(head("w.i32_rotl"), None);
        assert_eq!(head("a64.add_rr_32"), None);
    }
}
