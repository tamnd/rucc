//! The SIMD instructions of wasm, the `0xfd` prefix, as one table that the printer of `asm` and the
//! reader of `read` share.
//!
//! Design: document 19 of the WebAssembly notes, `19-simd.md`, step 1. The table has every
//! instruction of the SIMD proposal and of the relaxed SIMD proposal. Each row is the name in the
//! dialect of LLVM, the opcode after the prefix, and the [`Simd`] form, which says what the
//! immediates are and what the instruction takes from the stack and gives back. The names are the
//! ones that `llvm-mc` 23 reads, which are not always the names of the specification: the
//! extending loads are `i16x8.load8x8_s` and not `v128.load8x8_s`. Each row was checked against
//! the encoding that `llvm-mc` 23 gives for the name.

use rucc_object::wasm::ValType;

/// What the immediates of a SIMD instruction are, and what it does to the stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Simd {
    /// An address and a memory argument with this natural alignment, as a log2, and a `v128`.
    Load(u32),
    /// An address and a `v128`, and a memory argument with this natural alignment.
    Store(u32),
    /// An address and a `v128`, a memory argument and a lane, and a `v128`.
    LoadLane(u32),
    /// An address and a `v128`, a memory argument and a lane.
    StoreLane(u32),
    /// Sixteen bytes, and a `v128`.
    Const,
    /// Two `v128` and sixteen lane indices, and a `v128`.
    Shuffle,
    /// A scalar, and a `v128`.
    Splat,
    /// A `v128` and a lane, and the scalar of this type.
    Extract(ValType),
    /// A `v128`, a scalar and a lane, and a `v128`.
    Replace,
    /// A `v128`, and a `v128`.
    Unary,
    /// Two `v128`, and a `v128`.
    Binary,
    /// A `v128` and an `i32` count, and a `v128`.
    Shift,
    /// Three `v128`, and a `v128`.
    Ternary,
    /// A `v128`, and an `i32`.
    Test,
}

impl Simd {
    /// How many operands the instruction takes from the stack.
    pub(crate) fn pops(self) -> usize {
        match self {
            Simd::Const => 0,
            Simd::Load(_) | Simd::Splat | Simd::Extract(_) | Simd::Unary | Simd::Test => 1,
            Simd::Ternary => 3,
            _ => 2,
        }
    }

    /// What the instruction gives back, and nothing for a store.
    pub(crate) fn result(self) -> Option<ValType> {
        match self {
            Simd::Store(_) | Simd::StoreLane(_) => None,
            Simd::Extract(ty) => Some(ty),
            Simd::Test => Some(ValType::I32),
            _ => Some(ValType::V128),
        }
    }

    /// Whether the instruction has a memory argument.
    pub(crate) fn memory(self) -> Option<u32> {
        match self {
            Simd::Load(align) | Simd::Store(align) => Some(align),
            Simd::LoadLane(align) | Simd::StoreLane(align) => Some(align),
            _ => None,
        }
    }

    /// Whether the instruction has a lane index after the rest of its immediates.
    pub(crate) fn lane(self) -> bool {
        matches!(self, Simd::LoadLane(_) | Simd::StoreLane(_) | Simd::Extract(_) | Simd::Replace)
    }
}

/// The row of the instruction with this opcode after the prefix.
pub(crate) fn by_opcode(opcode: u32) -> Option<(&'static str, Simd)> {
    SIMD.iter().find(|&&(_, op, _)| op == opcode).map(|&(name, _, form)| (name, form))
}

/// The opcode and the form of the instruction with this name.
pub(crate) fn by_name(name: &str) -> Option<(u32, Simd)> {
    SIMD.iter().find(|&&(n, ..)| n == name).map(|&(_, op, form)| (op, form))
}

/// Every SIMD instruction, in the order of its opcode.
pub(crate) const SIMD: [(&str, u32, Simd); 256] = {
    use Simd::{
        Binary, Const, Extract, Load, LoadLane, Replace, Shift, Shuffle, Splat, Store, StoreLane,
        Ternary, Test, Unary,
    };
    use ValType::{F32, F64, I32, I64};
    [
        ("v128.load", 0x00, Load(4)),
        ("i16x8.load8x8_s", 0x01, Load(3)),
        ("i16x8.load8x8_u", 0x02, Load(3)),
        ("i32x4.load16x4_s", 0x03, Load(3)),
        ("i32x4.load16x4_u", 0x04, Load(3)),
        ("i64x2.load32x2_s", 0x05, Load(3)),
        ("i64x2.load32x2_u", 0x06, Load(3)),
        ("v128.load8_splat", 0x07, Load(0)),
        ("v128.load16_splat", 0x08, Load(1)),
        ("v128.load32_splat", 0x09, Load(2)),
        ("v128.load64_splat", 0x0a, Load(3)),
        ("v128.store", 0x0b, Store(4)),
        ("v128.const", 0x0c, Const),
        ("i8x16.shuffle", 0x0d, Shuffle),
        ("i8x16.swizzle", 0x0e, Binary),
        ("i8x16.splat", 0x0f, Splat),
        ("i16x8.splat", 0x10, Splat),
        ("i32x4.splat", 0x11, Splat),
        ("i64x2.splat", 0x12, Splat),
        ("f32x4.splat", 0x13, Splat),
        ("f64x2.splat", 0x14, Splat),
        ("i8x16.extract_lane_s", 0x15, Extract(I32)),
        ("i8x16.extract_lane_u", 0x16, Extract(I32)),
        ("i8x16.replace_lane", 0x17, Replace),
        ("i16x8.extract_lane_s", 0x18, Extract(I32)),
        ("i16x8.extract_lane_u", 0x19, Extract(I32)),
        ("i16x8.replace_lane", 0x1a, Replace),
        ("i32x4.extract_lane", 0x1b, Extract(I32)),
        ("i32x4.replace_lane", 0x1c, Replace),
        ("i64x2.extract_lane", 0x1d, Extract(I64)),
        ("i64x2.replace_lane", 0x1e, Replace),
        ("f32x4.extract_lane", 0x1f, Extract(F32)),
        ("f32x4.replace_lane", 0x20, Replace),
        ("f64x2.extract_lane", 0x21, Extract(F64)),
        ("f64x2.replace_lane", 0x22, Replace),
        ("i8x16.eq", 0x23, Binary),
        ("i8x16.ne", 0x24, Binary),
        ("i8x16.lt_s", 0x25, Binary),
        ("i8x16.lt_u", 0x26, Binary),
        ("i8x16.gt_s", 0x27, Binary),
        ("i8x16.gt_u", 0x28, Binary),
        ("i8x16.le_s", 0x29, Binary),
        ("i8x16.le_u", 0x2a, Binary),
        ("i8x16.ge_s", 0x2b, Binary),
        ("i8x16.ge_u", 0x2c, Binary),
        ("i16x8.eq", 0x2d, Binary),
        ("i16x8.ne", 0x2e, Binary),
        ("i16x8.lt_s", 0x2f, Binary),
        ("i16x8.lt_u", 0x30, Binary),
        ("i16x8.gt_s", 0x31, Binary),
        ("i16x8.gt_u", 0x32, Binary),
        ("i16x8.le_s", 0x33, Binary),
        ("i16x8.le_u", 0x34, Binary),
        ("i16x8.ge_s", 0x35, Binary),
        ("i16x8.ge_u", 0x36, Binary),
        ("i32x4.eq", 0x37, Binary),
        ("i32x4.ne", 0x38, Binary),
        ("i32x4.lt_s", 0x39, Binary),
        ("i32x4.lt_u", 0x3a, Binary),
        ("i32x4.gt_s", 0x3b, Binary),
        ("i32x4.gt_u", 0x3c, Binary),
        ("i32x4.le_s", 0x3d, Binary),
        ("i32x4.le_u", 0x3e, Binary),
        ("i32x4.ge_s", 0x3f, Binary),
        ("i32x4.ge_u", 0x40, Binary),
        ("f32x4.eq", 0x41, Binary),
        ("f32x4.ne", 0x42, Binary),
        ("f32x4.lt", 0x43, Binary),
        ("f32x4.gt", 0x44, Binary),
        ("f32x4.le", 0x45, Binary),
        ("f32x4.ge", 0x46, Binary),
        ("f64x2.eq", 0x47, Binary),
        ("f64x2.ne", 0x48, Binary),
        ("f64x2.lt", 0x49, Binary),
        ("f64x2.gt", 0x4a, Binary),
        ("f64x2.le", 0x4b, Binary),
        ("f64x2.ge", 0x4c, Binary),
        ("v128.not", 0x4d, Unary),
        ("v128.and", 0x4e, Binary),
        ("v128.andnot", 0x4f, Binary),
        ("v128.or", 0x50, Binary),
        ("v128.xor", 0x51, Binary),
        ("v128.bitselect", 0x52, Ternary),
        ("v128.any_true", 0x53, Test),
        ("v128.load8_lane", 0x54, LoadLane(0)),
        ("v128.load16_lane", 0x55, LoadLane(1)),
        ("v128.load32_lane", 0x56, LoadLane(2)),
        ("v128.load64_lane", 0x57, LoadLane(3)),
        ("v128.store8_lane", 0x58, StoreLane(0)),
        ("v128.store16_lane", 0x59, StoreLane(1)),
        ("v128.store32_lane", 0x5a, StoreLane(2)),
        ("v128.store64_lane", 0x5b, StoreLane(3)),
        ("v128.load32_zero", 0x5c, Load(2)),
        ("v128.load64_zero", 0x5d, Load(3)),
        ("f32x4.demote_f64x2_zero", 0x5e, Unary),
        ("f64x2.promote_low_f32x4", 0x5f, Unary),
        ("i8x16.abs", 0x60, Unary),
        ("i8x16.neg", 0x61, Unary),
        ("i8x16.popcnt", 0x62, Unary),
        ("i8x16.all_true", 0x63, Test),
        ("i8x16.bitmask", 0x64, Test),
        ("i8x16.narrow_i16x8_s", 0x65, Binary),
        ("i8x16.narrow_i16x8_u", 0x66, Binary),
        ("f32x4.ceil", 0x67, Unary),
        ("f32x4.floor", 0x68, Unary),
        ("f32x4.trunc", 0x69, Unary),
        ("f32x4.nearest", 0x6a, Unary),
        ("i8x16.shl", 0x6b, Shift),
        ("i8x16.shr_s", 0x6c, Shift),
        ("i8x16.shr_u", 0x6d, Shift),
        ("i8x16.add", 0x6e, Binary),
        ("i8x16.add_sat_s", 0x6f, Binary),
        ("i8x16.add_sat_u", 0x70, Binary),
        ("i8x16.sub", 0x71, Binary),
        ("i8x16.sub_sat_s", 0x72, Binary),
        ("i8x16.sub_sat_u", 0x73, Binary),
        ("f64x2.ceil", 0x74, Unary),
        ("f64x2.floor", 0x75, Unary),
        ("i8x16.min_s", 0x76, Binary),
        ("i8x16.min_u", 0x77, Binary),
        ("i8x16.max_s", 0x78, Binary),
        ("i8x16.max_u", 0x79, Binary),
        ("f64x2.trunc", 0x7a, Unary),
        ("i8x16.avgr_u", 0x7b, Binary),
        ("i16x8.extadd_pairwise_i8x16_s", 0x7c, Unary),
        ("i16x8.extadd_pairwise_i8x16_u", 0x7d, Unary),
        ("i32x4.extadd_pairwise_i16x8_s", 0x7e, Unary),
        ("i32x4.extadd_pairwise_i16x8_u", 0x7f, Unary),
        ("i16x8.abs", 0x80, Unary),
        ("i16x8.neg", 0x81, Unary),
        ("i16x8.q15mulr_sat_s", 0x82, Binary),
        ("i16x8.all_true", 0x83, Test),
        ("i16x8.bitmask", 0x84, Test),
        ("i16x8.narrow_i32x4_s", 0x85, Binary),
        ("i16x8.narrow_i32x4_u", 0x86, Binary),
        ("i16x8.extend_low_i8x16_s", 0x87, Unary),
        ("i16x8.extend_high_i8x16_s", 0x88, Unary),
        ("i16x8.extend_low_i8x16_u", 0x89, Unary),
        ("i16x8.extend_high_i8x16_u", 0x8a, Unary),
        ("i16x8.shl", 0x8b, Shift),
        ("i16x8.shr_s", 0x8c, Shift),
        ("i16x8.shr_u", 0x8d, Shift),
        ("i16x8.add", 0x8e, Binary),
        ("i16x8.add_sat_s", 0x8f, Binary),
        ("i16x8.add_sat_u", 0x90, Binary),
        ("i16x8.sub", 0x91, Binary),
        ("i16x8.sub_sat_s", 0x92, Binary),
        ("i16x8.sub_sat_u", 0x93, Binary),
        ("f64x2.nearest", 0x94, Unary),
        ("i16x8.mul", 0x95, Binary),
        ("i16x8.min_s", 0x96, Binary),
        ("i16x8.min_u", 0x97, Binary),
        ("i16x8.max_s", 0x98, Binary),
        ("i16x8.max_u", 0x99, Binary),
        ("i16x8.avgr_u", 0x9b, Binary),
        ("i16x8.extmul_low_i8x16_s", 0x9c, Binary),
        ("i16x8.extmul_high_i8x16_s", 0x9d, Binary),
        ("i16x8.extmul_low_i8x16_u", 0x9e, Binary),
        ("i16x8.extmul_high_i8x16_u", 0x9f, Binary),
        ("i32x4.abs", 0xa0, Unary),
        ("i32x4.neg", 0xa1, Unary),
        ("i32x4.all_true", 0xa3, Test),
        ("i32x4.bitmask", 0xa4, Test),
        ("i32x4.extend_low_i16x8_s", 0xa7, Unary),
        ("i32x4.extend_high_i16x8_s", 0xa8, Unary),
        ("i32x4.extend_low_i16x8_u", 0xa9, Unary),
        ("i32x4.extend_high_i16x8_u", 0xaa, Unary),
        ("i32x4.shl", 0xab, Shift),
        ("i32x4.shr_s", 0xac, Shift),
        ("i32x4.shr_u", 0xad, Shift),
        ("i32x4.add", 0xae, Binary),
        ("i32x4.sub", 0xb1, Binary),
        ("i32x4.mul", 0xb5, Binary),
        ("i32x4.min_s", 0xb6, Binary),
        ("i32x4.min_u", 0xb7, Binary),
        ("i32x4.max_s", 0xb8, Binary),
        ("i32x4.max_u", 0xb9, Binary),
        ("i32x4.dot_i16x8_s", 0xba, Binary),
        ("i32x4.extmul_low_i16x8_s", 0xbc, Binary),
        ("i32x4.extmul_high_i16x8_s", 0xbd, Binary),
        ("i32x4.extmul_low_i16x8_u", 0xbe, Binary),
        ("i32x4.extmul_high_i16x8_u", 0xbf, Binary),
        ("i64x2.abs", 0xc0, Unary),
        ("i64x2.neg", 0xc1, Unary),
        ("i64x2.all_true", 0xc3, Test),
        ("i64x2.bitmask", 0xc4, Test),
        ("i64x2.extend_low_i32x4_s", 0xc7, Unary),
        ("i64x2.extend_high_i32x4_s", 0xc8, Unary),
        ("i64x2.extend_low_i32x4_u", 0xc9, Unary),
        ("i64x2.extend_high_i32x4_u", 0xca, Unary),
        ("i64x2.shl", 0xcb, Shift),
        ("i64x2.shr_s", 0xcc, Shift),
        ("i64x2.shr_u", 0xcd, Shift),
        ("i64x2.add", 0xce, Binary),
        ("i64x2.sub", 0xd1, Binary),
        ("i64x2.mul", 0xd5, Binary),
        ("i64x2.eq", 0xd6, Binary),
        ("i64x2.ne", 0xd7, Binary),
        ("i64x2.lt_s", 0xd8, Binary),
        ("i64x2.gt_s", 0xd9, Binary),
        ("i64x2.le_s", 0xda, Binary),
        ("i64x2.ge_s", 0xdb, Binary),
        ("i64x2.extmul_low_i32x4_s", 0xdc, Binary),
        ("i64x2.extmul_high_i32x4_s", 0xdd, Binary),
        ("i64x2.extmul_low_i32x4_u", 0xde, Binary),
        ("i64x2.extmul_high_i32x4_u", 0xdf, Binary),
        ("f32x4.abs", 0xe0, Unary),
        ("f32x4.neg", 0xe1, Unary),
        ("f32x4.sqrt", 0xe3, Unary),
        ("f32x4.add", 0xe4, Binary),
        ("f32x4.sub", 0xe5, Binary),
        ("f32x4.mul", 0xe6, Binary),
        ("f32x4.div", 0xe7, Binary),
        ("f32x4.min", 0xe8, Binary),
        ("f32x4.max", 0xe9, Binary),
        ("f32x4.pmin", 0xea, Binary),
        ("f32x4.pmax", 0xeb, Binary),
        ("f64x2.abs", 0xec, Unary),
        ("f64x2.neg", 0xed, Unary),
        ("f64x2.sqrt", 0xef, Unary),
        ("f64x2.add", 0xf0, Binary),
        ("f64x2.sub", 0xf1, Binary),
        ("f64x2.mul", 0xf2, Binary),
        ("f64x2.div", 0xf3, Binary),
        ("f64x2.min", 0xf4, Binary),
        ("f64x2.max", 0xf5, Binary),
        ("f64x2.pmin", 0xf6, Binary),
        ("f64x2.pmax", 0xf7, Binary),
        ("i32x4.trunc_sat_f32x4_s", 0xf8, Unary),
        ("i32x4.trunc_sat_f32x4_u", 0xf9, Unary),
        ("f32x4.convert_i32x4_s", 0xfa, Unary),
        ("f32x4.convert_i32x4_u", 0xfb, Unary),
        ("i32x4.trunc_sat_f64x2_s_zero", 0xfc, Unary),
        ("i32x4.trunc_sat_f64x2_u_zero", 0xfd, Unary),
        ("f64x2.convert_low_i32x4_s", 0xfe, Unary),
        ("f64x2.convert_low_i32x4_u", 0xff, Unary),
        ("i8x16.relaxed_swizzle", 0x100, Binary),
        ("i32x4.relaxed_trunc_f32x4_s", 0x101, Unary),
        ("i32x4.relaxed_trunc_f32x4_u", 0x102, Unary),
        ("i32x4.relaxed_trunc_f64x2_s_zero", 0x103, Unary),
        ("i32x4.relaxed_trunc_f64x2_u_zero", 0x104, Unary),
        ("f32x4.relaxed_madd", 0x105, Ternary),
        ("f32x4.relaxed_nmadd", 0x106, Ternary),
        ("f64x2.relaxed_madd", 0x107, Ternary),
        ("f64x2.relaxed_nmadd", 0x108, Ternary),
        ("i8x16.relaxed_laneselect", 0x109, Ternary),
        ("i16x8.relaxed_laneselect", 0x10a, Ternary),
        ("i32x4.relaxed_laneselect", 0x10b, Ternary),
        ("i64x2.relaxed_laneselect", 0x10c, Ternary),
        ("f32x4.relaxed_min", 0x10d, Binary),
        ("f32x4.relaxed_max", 0x10e, Binary),
        ("f64x2.relaxed_min", 0x10f, Binary),
        ("f64x2.relaxed_max", 0x110, Binary),
        ("i16x8.relaxed_q15mulr_s", 0x111, Binary),
        ("i16x8.relaxed_dot_i8x16_i7x16_s", 0x112, Binary),
        ("i32x4.relaxed_dot_i8x16_i7x16_add_s", 0x113, Ternary),
    ]
};

#[cfg(test)]
mod tests {
    use super::{SIMD, Simd, by_name, by_opcode};

    #[test]
    fn the_opcodes_go_up_and_the_names_are_different() {
        for pair in SIMD.windows(2) {
            assert!(pair[0].1 < pair[1].1, "{} and {}", pair[0].0, pair[1].0);
        }
        let mut names: Vec<&str> = SIMD.iter().map(|row| row.0).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SIMD.len());
    }

    #[test]
    fn a_row_is_found_by_its_name_and_by_its_opcode() {
        assert_eq!(by_name("i32x4.add"), Some((0xae, Simd::Binary)));
        assert_eq!(by_opcode(0x0d), Some(("i8x16.shuffle", Simd::Shuffle)));
        assert_eq!(by_opcode(0x9a), None);
        assert_eq!(by_opcode(0x113).map(|row| row.0), Some("i32x4.relaxed_dot_i8x16_i7x16_add_s"));
    }
}
