//! The few instructions of the vector extension the kernel writes by hand.
//!
//! The kernel saves and restores the vector registers, clears them at boot and copies user
//! memory with them, and all of that is setting the vector type, loading and storing whole
//! groups of byte elements, and filling a group with a constant. Those are here and nothing
//! else of the extension is. rucc writes no vector code of its own, so these only come from a
//! file or an `asm` statement, and the operands are read from the text here rather than through
//! [`crate::riscv64::read`], whose operands have no vector register or vector type in them.

use crate::riscv64::encode::{Error, Value};
use crate::riscv64::read::register;

/// Writes one vector instruction from its text, or `None` when the mnemonic is not one of these.
///
/// # Errors
///
/// When the mnemonic is one of these and the operands are not ones it takes.
pub fn vector(text: &str) -> Option<Result<u32, Error>> {
    let text = text.trim();
    let (mnemonic, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let mnemonic = mnemonic.to_ascii_lowercase();
    let operands: Vec<&str> = match rest.trim() {
        "" => Vec::new(),
        rest => rest.split(',').map(str::trim).collect(),
    };
    let word = match mnemonic.as_str() {
        "vsetvli" | "vsetivli" | "vsetvl" => set(&mnemonic, &operands),
        "vmv.v.v" | "vmv.v.x" | "vmv.v.i" => splat(&mnemonic, &operands),
        _ => {
            let (op, width) = memory(&mnemonic)?;
            access(op, width, &operands)
        }
    };
    let operands = operands.len();
    Some(word.ok_or(Error::Unwritten { mnemonic, operands }))
}

/// `vsetvli rd, rs1, e8, m8, ta, ma`, `vsetivli` with a five bit count in place of `rs1`, and
/// `vsetvl` with the type in a register.
fn set(mnemonic: &str, operands: &[&str]) -> Option<u32> {
    let [rd, from, ref kind @ ..] = *operands else { return None };
    let rd = gpr(rd)?;
    let (high, from, kind) = match mnemonic {
        "vsetvli" => (0, gpr(from)?, vtype(kind)?),
        "vsetivli" => {
            let count = from.parse::<u32>().ok().filter(|&count| count < 32)?;
            (3 << 30, count, vtype(kind)?)
        }
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
fn splat(mnemonic: &str, operands: &[&str]) -> Option<u32> {
    let [vd, from] = *operands else { return None };
    let vd = vreg(vd)?;
    let (f3, from) = match mnemonic {
        "vmv.v.v" => (0, vreg(from)?),
        "vmv.v.x" => (4, gpr(from)?),
        _ => {
            let imm = from.parse::<i32>().ok().filter(|imm| (-16..16).contains(imm))?;
            (3, u32::try_from(imm & 0x1f).ok()?)
        }
    };
    Some(0x17 << 26 | 1 << 25 | from << 15 | f3 << 12 | vd << 7 | 0x57)
}

/// The opcode and the width field of a unit stride load or store, `vle8.v` up to `vse64.v`.
fn memory(mnemonic: &str) -> Option<(u32, u32)> {
    let rest = mnemonic.strip_suffix(".v")?;
    let (op, bits) = match rest.strip_prefix("vle") {
        Some(bits) => (0x07, bits),
        None => (0x27, rest.strip_prefix("vse")?),
    };
    let width = match bits {
        "8" => 0,
        "16" => 5,
        "32" => 6,
        "64" => 7,
        _ => return None,
    };
    Some((op, width))
}

/// A unit stride load or store, `vle8.v v0, (a1)`, with `v0.t` after it when it is masked.
fn access(op: u32, width: u32, operands: &[&str]) -> Option<u32> {
    let (vd, at, unmasked) = match *operands {
        [vd, at] => (vd, at, 1),
        [vd, at, "v0.t"] => (vd, at, 0),
        _ => return None,
    };
    let base = at.strip_prefix('0').unwrap_or(at).trim_start();
    let base = gpr(base.strip_prefix('(')?.strip_suffix(')')?.trim())?;
    Some(unmasked << 25 | base << 15 | width << 12 | vreg(vd)? << 7 | op)
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
        ] {
            assert_eq!(vector(text), Some(Ok(word)), "{text}");
        }
        assert_eq!(vector("addi a0, a0, 1"), None);
        assert!(matches!(vector("vsetvli a0, a1, m8"), Some(Err(_))));
        assert!(matches!(vector("vsetivli a0, 32, e8"), Some(Err(_))));
        assert!(matches!(vector("vmv.v.i v0, 16"), Some(Err(_))));
        assert!(matches!(vector("vle8.v v32, (a0)"), Some(Err(_))));
    }
}
