//! The instructions of the vector extension the kernel writes by hand.
//!
//! The kernel saves and restores the vector registers, clears them at boot and copies user
//! memory with them, and all of that is setting the vector type, loading and storing whole
//! groups of byte elements, filling a group with a constant, and the few integer operations
//! `lib/xor.S` and its kind run over a group. Its crypto code, in `lib/crypto/riscv` and
//! `arch/riscv/crypto`, adds strided, segment and indexed loads and stores, slides, the rotates
//! and reversals of Zvbb and Zvkb, and the AES, SHA-2, GHASH, SM3 and SM4 rounds of the vector
//! crypto extensions. Those are here and the rest of the extension is not. rucc writes no vector
//! code of its own, so these only come from a file or an `asm` statement, and the operands are
//! read from the text here rather than through [`crate::riscv64::read`], whose operands have no
//! vector register or vector type in them.

use crate::riscv64::encode::{Error, Value};
use crate::riscv64::read::register;

/// Writes one vector instruction from its text, or `None` when the mnemonic is not one of these.
///
/// # Errors
///
/// When the mnemonic is one of these and the operands are not ones it takes.
pub fn vector(text: &str) -> Option<Result<u32, Error>> {
    vector_with(text, &mut |text| text.trim().parse().ok())
}

/// [`vector`], with what a constant operand comes to worked out by `constant`, since the kernel
/// writes `32 - 16` and a symbol a `.set` keeps counting where a number goes.
///
/// # Errors
///
/// When the mnemonic is one of these and the operands are not ones it takes.
pub fn vector_with(
    text: &str,
    constant: &mut dyn FnMut(&str) -> Option<i64>,
) -> Option<Result<u32, Error>> {
    let text = text.trim();
    let (mnemonic, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let mnemonic = mnemonic.to_ascii_lowercase();
    let operands: Vec<&str> = match rest.trim() {
        "" => Vec::new(),
        rest => rest.split(',').map(str::trim).collect(),
    };
    let word = match mnemonic.as_str() {
        "vsetvli" | "vsetivli" | "vsetvl" => set(&mnemonic, &operands, constant),
        "vmv.v.v" | "vmv.v.x" | "vmv.v.i" => splat(&mnemonic, &operands, constant),
        "vmerge.vvm" | "vmerge.vxm" | "vmerge.vim" => merge(&mnemonic, &operands, constant),
        _ => {
            if let Some((funct6, f3, from)) = arith(&mnemonic) {
                operate(funct6, f3, from, &operands, constant)
            } else if let Some((funct6, vs1)) = unary(&mnemonic) {
                single(funct6, vs1, &operands)
            } else if let Some(op) = crypto(&mnemonic) {
                cipher(op, &operands, constant)
            } else {
                access(memory(&mnemonic)?, &operands)
            }
        }
    };
    let operands = operands.len();
    Some(word.ok_or(Error::Unwritten { mnemonic, operands }))
}

/// `vsetvli rd, rs1, e8, m8, ta, ma`, `vsetivli` with a five bit count in place of `rs1`, and
/// `vsetvl` with the type in a register.
fn set(
    mnemonic: &str,
    operands: &[&str],
    constant: &mut dyn FnMut(&str) -> Option<i64>,
) -> Option<u32> {
    let [rd, from, ref kind @ ..] = *operands else { return None };
    let rd = gpr(rd)?;
    let (high, from, kind) = match mnemonic {
        "vsetvli" => (0, gpr(from)?, vtype(kind)?),
        "vsetivli" => (3 << 30, unsigned(from, 5, constant)?, vtype(kind)?),
        _ => match *kind {
            [rs2] => (1 << 31, gpr(from)?, gpr(rs2)?),
            _ => return None,
        },
    };
    Some(high | kind << 20 | from << 15 | 7 << 12 | rd << 7 | 0x57)
}

/// The vector type: the element width, the group, and whether the tail and the masked off
/// elements are left alone or may be anything. The width is the only one gas requires.
fn vtype(words: &[&str]) -> Option<u32> {
    let mut words = words.iter().copied().peekable();
    let sew = match words.next()? {
        "e8" => 0,
        "e16" => 1,
        "e32" => 2,
        "e64" => 3,
        _ => return None,
    };
    let group = |word| {
        Some(match word {
            "m1" => 0,
            "m2" => 1,
            "m4" => 2,
            "m8" => 3,
            "mf8" => 5,
            "mf4" => 6,
            "mf2" => 7,
            _ => return None,
        })
    };
    let lmul = words.next_if(|&word| group(word).is_some()).and_then(group).unwrap_or(0);
    let ta = match words.next_if(|&word| word == "ta" || word == "tu") {
        Some(word) => u32::from(word == "ta"),
        None => 0,
    };
    let ma = match words.next_if(|&word| word == "ma" || word == "mu") {
        Some(word) => u32::from(word == "ma"),
        None => 0,
    };
    words.next().is_none().then_some(ma << 7 | ta << 6 | sew << 3 | lmul)
}

/// `vmv.v.v`, `vmv.v.x` and `vmv.v.i`, which fill a group from a group, a register or a five
/// bit constant.
fn splat(
    mnemonic: &str,
    operands: &[&str],
    constant: &mut dyn FnMut(&str) -> Option<i64>,
) -> Option<u32> {
    let [vd, from] = *operands else { return None };
    let vd = vreg(vd)?;
    let (f3, from) = match mnemonic {
        "vmv.v.v" => (0, vreg(from)?),
        "vmv.v.x" => (4, gpr(from)?),
        _ => (3, signed(from, constant)?),
    };
    Some(0x17 << 26 | 1 << 25 | from << 15 | f3 << 12 | vd << 7 | 0x57)
}

/// `vmerge.vvm vd, vs2, vs1, v0`, which takes each element from the last operand where the mask
/// in `v0` has a one and from `vs2` where it has a zero, with a register or a five bit constant
/// in the `.vxm` and `.vim` forms. `vmv.v.v` is the same instruction unmasked.
fn merge(
    mnemonic: &str,
    operands: &[&str],
    constant: &mut dyn FnMut(&str) -> Option<i64>,
) -> Option<u32> {
    let [vd, vs2, last, "v0"] = *operands else { return None };
    let (f3, last) = match mnemonic {
        "vmerge.vvm" => (0, vreg(last)?),
        "vmerge.vxm" => (4, gpr(last)?),
        _ => (3, signed(last, constant)?),
    };
    Some(0x17 << 26 | vreg(vs2)? << 20 | last << 15 | f3 << 12 | vreg(vd)? << 7 | 0x57)
}

/// Where the second source of an operation on a group comes from.
#[derive(Clone, Copy)]
enum From {
    Group,
    Register,
    /// A five bit constant from -16 to 15.
    Signed,
    /// A five bit constant from 0 to 31.
    Unsigned,
    /// A six bit constant, whose top bit is the bottom bit of the function. Only `vror.vi`.
    Six,
}

/// The function and the operand kind of an integer operation on a group with `.vv`, `.vx` or
/// `.vi` after it, for the ones the kernel writes: the arithmetic and logic its crypto code
/// does, the shifts and the gather of its RAID-6 code, the slides, and the rotates and shifts of
/// the Zvbb and Zvkb extensions.
fn arith(mnemonic: &str) -> Option<(u32, u32, From)> {
    let (op, kind) = mnemonic.split_once('.')?;
    // The function, whether there is a `.vv` form, and the constant of the `.vi` form, if any.
    // Every one of these has a `.vx` form.
    let (funct6, vv, vi) = match op {
        "vadd" => (0b00_0000, true, Some(From::Signed)),
        "vandn" => (0b00_0001, true, None),
        "vsub" => (0b00_0010, true, None),
        "vand" => (0b00_1001, true, Some(From::Signed)),
        "vor" => (0b00_1010, true, Some(From::Signed)),
        "vxor" => (0b00_1011, true, Some(From::Signed)),
        "vrgather" => (0b00_1100, true, Some(From::Unsigned)),
        "vslideup" => (0b00_1110, false, Some(From::Unsigned)),
        "vslidedown" => (0b00_1111, false, Some(From::Unsigned)),
        "vror" => (0b01_0100, true, Some(From::Six)),
        "vrol" => (0b01_0101, true, None),
        "vsll" => (0b10_0101, true, Some(From::Unsigned)),
        "vsrl" => (0b10_1000, true, Some(From::Unsigned)),
        "vsra" => (0b10_1001, true, Some(From::Unsigned)),
        "vwsll" => (0b11_0101, true, Some(From::Unsigned)),
        _ => return None,
    };
    match kind {
        "vv" if vv => Some((funct6, 0, From::Group)),
        "vx" => Some((funct6, 4, From::Register)),
        "vi" => Some((funct6, 3, vi?)),
        _ => None,
    }
}

/// `vxor.vv vd, vs2, vs1`, with a register or a constant for the last in the `.vx` and `.vi`
/// forms, and `v0.t` after it when it is masked.
fn operate(
    funct6: u32,
    f3: u32,
    from: From,
    operands: &[&str],
    constant: &mut dyn FnMut(&str) -> Option<i64>,
) -> Option<u32> {
    let (vd, vs2, last, unmasked) = match *operands {
        [vd, vs2, last] => (vd, vs2, last, 1),
        [vd, vs2, last, "v0.t"] => (vd, vs2, last, 0),
        _ => return None,
    };
    let (funct6, last) = match from {
        From::Group => (funct6, vreg(last)?),
        From::Register => (funct6, gpr(last)?),
        From::Signed => (funct6, signed(last, constant)?),
        From::Unsigned => (funct6, unsigned(last, 5, constant)?),
        From::Six => {
            let amount = unsigned(last, 6, constant)?;
            (funct6 | amount >> 5, amount & 0x1f)
        }
    };
    let (vd, vs2) = (vreg(vd)?, vreg(vs2)?);
    Some(funct6 << 26 | unmasked << 25 | vs2 << 20 | last << 15 | f3 << 12 | vd << 7 | 0x57)
}

/// The function and the number in the `vs1` field of an operation on one group: the widening
/// moves, the bit and byte reversals of Zvbb and Zvkb, and `viota.m` and `vid.v`.
fn unary(mnemonic: &str) -> Option<(u32, u32)> {
    Some(match mnemonic {
        "vzext.vf8" => (0b01_0010, 0b0_0010),
        "vsext.vf8" => (0b01_0010, 0b0_0011),
        "vzext.vf4" => (0b01_0010, 0b0_0100),
        "vsext.vf4" => (0b01_0010, 0b0_0101),
        "vzext.vf2" => (0b01_0010, 0b0_0110),
        "vsext.vf2" => (0b01_0010, 0b0_0111),
        "vbrev8.v" => (0b01_0010, 0b0_1000),
        "vrev8.v" => (0b01_0010, 0b0_1001),
        "vbrev.v" => (0b01_0010, 0b0_1010),
        "vclz.v" => (0b01_0010, 0b0_1100),
        "vctz.v" => (0b01_0010, 0b0_1101),
        "vcpop.v" => (0b01_0010, 0b0_1110),
        "viota.m" => (0b01_0100, 0b1_0000),
        "vid.v" => (0b01_0100, 0b1_0001),
        _ => return None,
    })
}

/// `vrev8.v vd, vs2`, and `vid.v vd` with no source, with `v0.t` after either when it is masked.
fn single(funct6: u32, vs1: u32, operands: &[&str]) -> Option<u32> {
    let (operands, unmasked) = match operands.split_last() {
        Some((&"v0.t", operands)) => (operands, 0),
        _ => (operands, 1),
    };
    let (vd, vs2) = match (operands, vs1) {
        (&[vd], 0b1_0001) => (vd, 0),
        (&[vd, vs2], vs1) if vs1 != 0b1_0001 => (vd, vreg(vs2)?),
        _ => return None,
    };
    Some(funct6 << 26 | unmasked << 25 | vs2 << 20 | vs1 << 15 | 2 << 12 | vreg(vd)? << 7 | 0x57)
}

/// How an instruction of the vector crypto extensions takes its last operand.
#[derive(Clone, Copy)]
enum Crypto {
    /// `vghsh.vv vd, vs2, vs1`.
    Pair(u32),
    /// `vsm3c.vi vd, vs2, uimm`, a five bit constant.
    Constant(u32),
    /// `vaesef.vs vd, vs2`, with the operation in the `vs1` field.
    Fixed(u32, u32),
}

/// The instructions of Zvkned, Zvknha and Zvknhb, Zvkg, Zvksed and Zvksh, which are the AES,
/// SHA-2, GHASH, SM4 and SM3 rounds the kernel's crypto code is written with. The `.vv` form of
/// a round takes each element group's key from the same group of `vs2`, and the `.vs` form takes
/// every one from the first.
fn crypto(mnemonic: &str) -> Option<Crypto> {
    let (op, kind) = mnemonic.split_once('.')?;
    let fixed = |vs1| match kind {
        "vv" => Some(Crypto::Fixed(0b10_1000, vs1)),
        "vs" => Some(Crypto::Fixed(0b10_1001, vs1)),
        _ => None,
    };
    match (op, kind) {
        ("vghsh", "vv") => Some(Crypto::Pair(0b10_1100)),
        ("vsha2ms", "vv") => Some(Crypto::Pair(0b10_1101)),
        ("vsha2ch", "vv") => Some(Crypto::Pair(0b10_1110)),
        ("vsha2cl", "vv") => Some(Crypto::Pair(0b10_1111)),
        ("vsm3me", "vv") => Some(Crypto::Pair(0b10_0000)),
        ("vsm4k", "vi") => Some(Crypto::Constant(0b10_0001)),
        ("vaeskf1", "vi") => Some(Crypto::Constant(0b10_0010)),
        ("vaeskf2", "vi") => Some(Crypto::Constant(0b10_1010)),
        ("vsm3c", "vi") => Some(Crypto::Constant(0b10_1011)),
        ("vaesdm", _) => fixed(0b0_0000),
        ("vaesdf", _) => fixed(0b0_0001),
        ("vaesem", _) => fixed(0b0_0010),
        ("vaesef", _) => fixed(0b0_0011),
        ("vaesz", "vs") => fixed(0b0_0111),
        ("vsm4r", _) => fixed(0b1_0000),
        ("vgmul", "vv") => fixed(0b1_0001),
        _ => None,
    }
}

/// One of [`crypto`], none of which can be masked.
fn cipher(
    op: Crypto,
    operands: &[&str],
    constant: &mut dyn FnMut(&str) -> Option<i64>,
) -> Option<u32> {
    let (funct6, vd, vs2, vs1) = match (op, operands) {
        (Crypto::Pair(funct6), &[vd, vs2, vs1]) => (funct6, vd, vs2, vreg(vs1)?),
        (Crypto::Constant(funct6), &[vd, vs2, imm]) => {
            (funct6, vd, vs2, unsigned(imm, 5, constant)?)
        }
        (Crypto::Fixed(funct6, vs1), &[vd, vs2]) => (funct6, vd, vs2, vs1),
        _ => return None,
    };
    Some(funct6 << 26 | 1 << 25 | vreg(vs2)? << 20 | vs1 << 15 | 2 << 12 | vreg(vd)? << 7 | 0x77)
}

/// What the third operand of a load or a store is.
#[derive(Clone, Copy, PartialEq)]
enum Third {
    /// None: unit stride, element after element.
    Nothing,
    /// A register with the distance between one element, or one segment, and the next.
    Stride,
    /// A group with the offset of each element.
    Index,
}

/// The fields of a load or a store: the opcode, how many fields a segment has less one, the
/// addressing mode, the width, and what the third operand is.
struct Access {
    op: u32,
    nf: u32,
    mop: u32,
    width: u32,
    third: Third,
}

/// A load or a store, from `vle8.v` up: unit stride, strided (`vlse32.v`), segments of either
/// (`vlseg4e32.v`, `vlsseg8e32.v`), and indexed, unordered or ordered (`vluxei8.v`, `vloxei8.v`).
/// The width of an indexed one is the width of the offsets.
fn memory(mnemonic: &str) -> Option<Access> {
    let rest = mnemonic.strip_suffix(".v")?;
    let (op, rest) = match rest.strip_prefix("vl") {
        Some(rest) => (0x07, rest),
        None => (0x27, rest.strip_prefix("vs")?),
    };
    let (fields, mop, bits, third) = if let Some(rest) = rest.strip_prefix("sseg") {
        let (fields, bits) = rest.split_once('e')?;
        (fields, 2, bits, Third::Stride)
    } else if let Some(rest) = rest.strip_prefix("seg") {
        let (fields, bits) = rest.split_once('e')?;
        (fields, 0, bits, Third::Nothing)
    } else if let Some(bits) = rest.strip_prefix("se") {
        ("1", 2, bits, Third::Stride)
    } else if let Some(bits) = rest.strip_prefix("uxei") {
        ("1", 1, bits, Third::Index)
    } else if let Some(bits) = rest.strip_prefix("oxei") {
        ("1", 3, bits, Third::Index)
    } else {
        ("1", 0, rest.strip_prefix('e')?, Third::Nothing)
    };
    let fields = fields.parse::<u32>().ok().filter(|fields| (1..=8).contains(fields))?;
    let width = match bits {
        "8" => 0,
        "16" => 5,
        "32" => 6,
        "64" => 7,
        _ => return None,
    };
    Some(Access { op, nf: fields - 1, mop, width, third })
}

/// A load or a store, `vle8.v v0, (a1)`, with the stride or the offsets after the address when
/// it has them and `v0.t` last when it is masked.
fn access(access: Access, operands: &[&str]) -> Option<u32> {
    let (operands, unmasked) = match operands.split_last() {
        Some((&"v0.t", operands)) => (operands, 0),
        _ => (operands, 1),
    };
    let (vd, at, third) = match (operands, access.third) {
        (&[vd, at], Third::Nothing) => (vd, at, 0),
        (&[vd, at, stride], Third::Stride) => (vd, at, gpr(stride)?),
        (&[vd, at, index], Third::Index) => (vd, at, vreg(index)?),
        _ => return None,
    };
    let base = at.strip_prefix('0').unwrap_or(at).trim_start();
    let base = gpr(base.strip_prefix('(')?.strip_suffix(')')?.trim())?;
    let Access { op, nf, mop, width, .. } = access;
    Some(
        nf << 29
            | mop << 26
            | unmasked << 25
            | third << 20
            | base << 15
            | width << 12
            | vreg(vd)? << 7
            | op,
    )
}

/// A five bit constant from -16 to 15, as the field holds it.
fn signed(text: &str, constant: &mut dyn FnMut(&str) -> Option<i64>) -> Option<u32> {
    let value = constant(text).filter(|value| (-16..16).contains(value))?;
    u32::try_from(value & 0x1f).ok()
}

/// A constant from 0 up to but not including `1 << bits`.
fn unsigned(text: &str, bits: u32, constant: &mut dyn FnMut(&str) -> Option<i64>) -> Option<u32> {
    let value = constant(text).filter(|value| (0..1 << bits).contains(value))?;
    u32::try_from(value).ok()
}

fn gpr(text: &str) -> Option<u32> {
    match register(text)? {
        Value::Gpr(number) => Some(u32::from(number)),
        _ => None,
    }
}

fn vreg(text: &str) -> Option<u32> {
    let digits = text.strip_prefix('v')?;
    if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) {
        return None;
    }
    digits.parse::<u32>().ok().filter(|&number| number < 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crypto_code_s_vector_instructions_are_the_words_gas_writes() {
        // `riscv64-linux-gnu-as -march=rv64gcv_zvkned_zvknhb_zvkg_zvksed_zvksh_zvbb`, 2.42.
        for (text, word) in [
            ("vadd.vv v1, v2, v3", 0x0221_80d7),
            ("vadd.vi v1, v2, -5", 0x022d_b0d7),
            ("vslideup.vi v16, v20, 4", 0x3b42_3857),
            ("vslideup.vx v16, v20, a0", 0x3b45_4857),
            ("vslidedown.vi v8, v9, 31", 0x3e9f_b457),
            ("vslidedown.vx v24, v20, t1", 0x3f43_4c57),
            ("vror.vi v1, v2, 16", 0x5228_30d7),
            ("vror.vi v1, v2, 63", 0x562f_b0d7),
            ("vror.vv v1, v2, v3", 0x5221_80d7),
            ("vror.vx v1, v2, a1", 0x5225_c0d7),
            ("vrol.vv v1, v2, v3", 0x5621_80d7),
            ("vandn.vv v1, v2, v3", 0x0621_80d7),
            ("vandn.vx v1, v2, a2", 0x0626_40d7),
            ("vwsll.vv v8, v2, v4", 0xd622_0457),
            ("vwsll.vi v8, v2, 31", 0xd62f_b457),
            ("vrev8.v v4, v5", 0x4a54_a257),
            ("vbrev8.v v12, v8", 0x4a84_2657),
            ("vbrev.v v1, v2", 0x4a25_20d7),
            ("vzext.vf2 v2, v0", 0x4a03_2157),
            ("vzext.vf4 v2, v1, v0.t", 0x4812_2157),
            ("vsext.vf2 v2, v4", 0x4a43_a157),
            ("vid.v v12", 0x5208_a657),
            ("viota.m v20, v0, v0.t", 0x5008_2a57),
            ("vmerge.vvm v1, v2, v3, v0", 0x5c21_80d7),
            ("vmerge.vxm v1, v2, a0, v0", 0x5c25_40d7),
            ("vmerge.vim v1, v2, -1, v0", 0x5c2f_b0d7),
            ("vle64.v v4, (a0)", 0x0205_7207),
            ("vlse32.v v1, (a2), t0", 0x0a56_6087),
            ("vsse32.v v1, (a2), t0", 0x0a56_60a7),
            ("vlsseg8e32.v v16, (a1), t2", 0xea75_e807),
            ("vssseg8e32.v v24, (a3), t2", 0xea76_ec27),
            ("vlseg4e32.v v8, (a0)", 0x6205_6407),
            ("vsseg2e64.v v8, (a0), v0.t", 0x2005_7427),
            ("vluxei8.v v1, (a0), v3", 0x0635_0087),
            ("vsuxei8.v v1, (a0), v3", 0x0635_00a7),
            ("vloxei32.v v1, (a0), v4", 0x0e45_6087),
            ("vsoxei64.v v1, (a0), v4, v0.t", 0x0c45_70a7),
            ("vaesef.vs v1, v2", 0xa621_a0f7),
            ("vaesef.vv v1, v2", 0xa221_a0f7),
            ("vaesem.vs v1, v2", 0xa621_20f7),
            ("vaesdf.vs v1, v2", 0xa620_a0f7),
            ("vaesdm.vs v1, v11", 0xa6b0_20f7),
            ("vaesdm.vv v1, v11", 0xa2b0_20f7),
            ("vaesz.vs v4, v1", 0xa613_a277),
            ("vaeskf1.vi v1, v2, 3", 0x8a21_a0f7),
            ("vaeskf2.vi v1, v2, 7", 0xaa23_a0f7),
            ("vgmul.vv v1, v8", 0xa288_a0f7),
            ("vghsh.vv v1, v2, v3", 0xb221_a0f7),
            ("vsha2ms.vv v1, v2, v3", 0xb621_a0f7),
            ("vsha2ch.vv v4, v5, v6", 0xba53_2277),
            ("vsha2cl.vv v4, v5, v6", 0xbe53_2277),
            ("vsm3me.vv v1, v2, v3", 0x8221_a0f7),
            ("vsm3c.vi v1, v2, 31", 0xae2f_a0f7),
            ("vsm4k.vi v1, v1, 7", 0x8613_a0f7),
            ("vsm4r.vs v1, v2", 0xa628_20f7),
            ("vsm4r.vv v1, v2", 0xa228_20f7),
        ] {
            assert_eq!(vector(text), Some(Ok(word)), "{text}");
        }
    }

    #[test]
    fn a_constant_operand_is_whatever_the_caller_works_it_out_to() {
        // `vror.vi \d0, \d0, 32 - 16` in the ChaCha code, worked out by the assembler.
        let mut sum = |text: &str| match text {
            "32 - 16" => Some(16),
            _ => text.parse().ok(),
        };
        assert_eq!(vector_with("vror.vi v1, v2, 32 - 16", &mut sum), Some(Ok(0x5228_30d7)));
        assert!(matches!(vector("vror.vi v1, v2, 32 - 16"), Some(Err(_))));
    }

    #[test]
    fn the_vector_instructions_the_kernel_writes_are_the_words_llvm_mc_writes() {
        for (text, word) in [
            ("vsetvli t1, x0, e8, m8, ta, ma", 0x0c30_7357),
            ("vsetvli a0, a1, e32, m1, tu, mu", 0x0105_f557),
            ("vsetvli a0, a1, e16", 0x0085_f557),
            ("vsetvli a0, a1, e64, mf2, ta, ma", 0x0df5_f557),
            ("vsetivli a0, 16, e8, m1, ta, ma", 0xcc08_7557),
            ("vsetvl a0, a1, a2", 0x80c5_f557),
            ("vsetvl x0, a2, a1", 0x80b6_7057),
            ("vmv.v.i v0, 0", 0x5e00_3057),
            ("vmv.v.i v8, -1", 0x5e0f_b457),
            ("vmv.v.x v16, a0", 0x5e05_4857),
            ("vmv.v.v v24, v8", 0x5e04_0c57),
            ("vle8.v v0, (a1)", 0x0205_8007),
            ("vse8.v v8, (a0)", 0x0205_0427),
            ("vle16.v v1, 0(a2)", 0x0206_5087),
            ("vle32.v v2, (a3), v0.t", 0x0006_e107),
            ("vse64.v v3, (a4)", 0x0207_71a7),
            ("vxor.vv v16, v0, v8", 0x2e04_0857),
            ("vxor.vv v0, v0, v8", 0x2e04_0057),
            ("vadd.vx v4, v8, a0", 0x0285_4257),
            ("vand.vi v1, v2, -3", 0x262e_b0d7),
            ("vor.vv v3, v4, v5, v0.t", 0x2842_81d7),
            ("vsub.vx v6, v7, t0", 0x0a72_c357),
            ("vsll.vi v1, v2, 7", 0x9623_b0d7),
            ("vsll.vx v1, v2, a0", 0x9625_40d7),
            ("vsrl.vi v3, v4, 1", 0xa240_b1d7),
            ("vsra.vi v5, v6, 7", 0xa663_b2d7),
            ("vsra.vv v5, v6, v7", 0xa663_82d7),
            ("vrgather.vv v8, v9, v10", 0x3295_0457),
        ] {
            assert_eq!(vector(text), Some(Ok(word)), "{text}");
        }
        assert_eq!(vector("addi a0, a0, 1"), None);
        assert!(matches!(vector("vror.vi v1, v2, 64"), Some(Err(_))));
        assert!(matches!(vector("vslideup.vi v1, v2, -1"), Some(Err(_))));
        assert!(matches!(vector("vmerge.vvm v1, v2, v3, v1"), Some(Err(_))));
        assert!(matches!(vector("vaesef.vs v1, v2, v0.t"), Some(Err(_))));
        assert!(matches!(vector("vlse32.v v1, (a0)"), Some(Err(_))));
        assert_eq!(vector("vlseg9e32.v v1, (a0)"), None);
        assert_eq!(vector("vslideup.vv v1, v2, v3"), None);
        assert_eq!(vector("vaesz.vv v1, v2"), None);
        assert!(matches!(vector("vsetvli a0, a1, m8"), Some(Err(_))));
        assert!(matches!(vector("vsetivli a0, 32, e8"), Some(Err(_))));
        assert!(matches!(vector("vmv.v.i v0, 16"), Some(Err(_))));
        assert!(matches!(vector("vle8.v v32, (a0)"), Some(Err(_))));
        assert!(matches!(vector("vxor.vv v0, v1, a0"), Some(Err(_))));
        assert_eq!(vector("vsub.vi v0, v1, 1"), None);
    }
}
