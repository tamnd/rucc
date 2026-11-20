//! The check of each arm64 record against the table of `7l` (07.3 of the rucc-plan9 plan).
//!
//! `oplook` in `7l/span.c` gives each operand a class with `aclass`, and takes the first entry of
//! `optab` for the opcode whose classes cover the classes of the operands. The middle register
//! has the class `C_REG`, or `C_FREG` for some opcodes, when it is there. When no entry covers the
//! classes, the link stops with "illegal combination". The translation checks each record that it
//! writes in the same way, with the table of `optab7.rs`. Thus an instruction that `7l` cannot
//! encode is an error of the compiler on the host, with the line of the listing, and not an error
//! of the link in the guest.
//!
//! Before `span`, `7l` changes some records. `ldobj` makes an `ADD` of a negative constant a
//! `SUB` of the positive constant, and the reverse, and it puts a floating constant of `FMOVS` and
//! `FMOVD` in memory when the instruction cannot hold it. `patch` makes the target of `B`, `BL`
//! and `RET` a branch when it names a symbol. `noop` makes each `RETURN` the release of the frame,
//! so the check does not look at `RETURN`. The check makes the other changes.
//!
//! The class of an operand that names a symbol depends on the address of the symbol, which only
//! the linker knows. The check finds each class that the operand can have, and the record must
//! have an entry for each of them.

use std::sync::LazyLock;

use rucc_base::hash::Map;
use rucc_object::plan9::{Addr7, Prog7, arm64 as a7};

use super::optab7 as t;

/// The records that `span` does not encode, or that `noop` replaces.
const PSEUDO: &[&str] = &[
    "TEXT", "DATA", "GLOBL", "HISTORY", "END", "NAME", "SIGNAME", "DYNT", "INIT", "NOP", "RETURN",
];

/// The opcodes whose middle register is a floating register, the `asregclass` of `7l`.
const FREG_MIDDLE: &[&str] = &[
    "FMADDD", "FMADDS", "FMSUBD", "FMSUBS", "FNMADDS", "FNMADDD", "FNMSUBS", "FNMSUBD", "MOVPS",
    "MOVPD", "MOVPQ",
];

/// The register that is zero, or the stack pointer.
const REGZERO: u8 = 31;

/// The classes that an offset from the static base can have, the `sextclass` of `7l`.
const EXTERN: &[&str] = &["C_SEXT1", "C_SEXT2", "C_SEXT4", "C_SEXT8", "C_SEXT16", "C_LEXT"];

/// The table of `7l`, ready for the check.
pub(crate) struct Checker {
    /// `cover[a][b]`: class `a` of an entry covers class `b` of an operand, the `cmp` of `7l`.
    cover: Vec<Vec<bool>>,
    /// The classes of the entries of each opcode name, after the ranges of `buildop`.
    entries: Map<&'static str, Vec<(u8, u8, u8)>>,
    /// The value of each class name.
    class: Map<&'static str, u8>,
}

/// The table, made once.
pub(crate) static CHECKER: LazyLock<Checker> = LazyLock::new(Checker::new);

/// The position of `name` in the classes, or `C_GOK`.
fn index(name: &str) -> usize {
    t::CLASSES.iter().position(|c| *c == name).unwrap_or(t::CLASSES.len() - 1)
}

/// Class `a` of an entry covers class `b` of an operand: `cmp` in `7l/span.c`.
fn cmp(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let within = |lo: &str, hi: &str| (index(lo)..=index(hi)).contains(&index(b));
    match a {
        "C_RSP" => b == "C_REG",
        "C_REG" | "C_ADDCON0" => b == "C_ZCON",
        "C_ADDCON" => matches!(b, "C_ZCON" | "C_ADDCON0" | "C_ABCON"),
        "C_BITCON32" | "C_BITCON64" => matches!(b, "C_BITCON" | "C_ABCON" | "C_MBCON"),
        "C_BITCON" => matches!(b, "C_ABCON" | "C_MBCON"),
        "C_MOVCON" => matches!(b, "C_MBCON" | "C_ZCON" | "C_ADDCON0"),
        "C_LCON" => matches!(
            b,
            "C_ZCON"
                | "C_BITCON"
                | "C_BITCON32"
                | "C_BITCON64"
                | "C_ADDCON"
                | "C_ADDCON0"
                | "C_ABCON"
                | "C_MBCON"
                | "C_MOVCON"
        ),
        "C_VCON" => cmp("C_LCON", b),
        "C_LACON" => b == "C_AACON",
        "C_SEXT2" => b == "C_SEXT1",
        "C_SEXT4" => within("C_SEXT1", "C_SEXT2"),
        "C_SEXT8" => within("C_SEXT1", "C_SEXT4"),
        "C_SEXT16" => within("C_SEXT1", "C_SEXT8"),
        "C_LEXT" => within("C_SEXT1", "C_SEXT16"),
        "C_PPAUTO" => b == "C_PSAUTO",
        "C_UAUTO4K" => matches!(b, "C_PSAUTO" | "C_PPAUTO"),
        "C_UAUTO8K" => cmp("C_UAUTO4K", b),
        "C_UAUTO16K" => cmp("C_UAUTO8K", b),
        "C_UAUTO32K" => cmp("C_UAUTO16K", b),
        "C_UAUTO64K" => cmp("C_UAUTO32K", b),
        "C_NPAUTO" => cmp("C_NSAUTO", b),
        "C_LAUTO" => cmp("C_NPAUTO", b) || cmp("C_UAUTO64K", b),
        "C_PSOREG" => b == "C_ZOREG",
        "C_PPOREG" => matches!(b, "C_ZOREG" | "C_PSOREG"),
        "C_UOREG4K" => {
            matches!(b, "C_ZOREG" | "C_PSAUTO" | "C_PSOREG" | "C_PPAUTO" | "C_PPOREG")
        }
        "C_UOREG8K" => cmp("C_UOREG4K", b),
        "C_UOREG16K" => cmp("C_UOREG8K", b),
        "C_UOREG32K" => cmp("C_UOREG16K", b),
        "C_UOREG64K" => cmp("C_UOREG32K", b),
        "C_NPOREG" => cmp("C_NSOREG", b),
        "C_LOREG" => cmp("C_NPOREG", b) || cmp("C_UOREG64K", b),
        "C_LBRA" => b == "C_SBRA",
        _ => false,
    }
}

/// The index of the range of `constclass` in `7l` for the offset `l`.
fn constclass(l: i64) -> usize {
    match l {
        0 => 0,
        -256..0 => 1,
        -512..0 if l & 7 == 0 => 2,
        _ if l < 0 => 10,
        1..=255 => 3,
        _ if l <= 504 && l & 7 == 0 => 4,
        _ if l <= 4095 => 5,
        _ if l <= 8190 && l & 1 == 0 => 6,
        _ if l <= 16380 && l & 3 == 0 => 7,
        _ if l <= 32760 && l & 7 == 0 => 8,
        _ if l <= 65520 && l & 0xf == 0 => 9,
        _ => 10,
    }
}

/// The classes of an offset from a register, the `oregclass` of `7l`.
const OREG: [&str; 11] = [
    "C_ZOREG",
    "C_NSOREG",
    "C_NPOREG",
    "C_PSOREG",
    "C_PPOREG",
    "C_UOREG4K",
    "C_UOREG8K",
    "C_UOREG16K",
    "C_UOREG32K",
    "C_UOREG64K",
    "C_LOREG",
];

/// The constant fits the 12 bits of an `ADD`, shifted by 0 or 12: `isaddcon`.
fn isaddcon(v: i64) -> bool {
    if v < 0 {
        return false;
    }
    let v = if v & 0xfff == 0 { v >> 12 } else { v };
    v <= 0xfff
}

/// The number of ones in each element when `v` is a logical immediate of 64 bits, the `findmask`
/// of `7l`, which looks `v` up in the table of `bits.c`.
fn bitmask(v: u64) -> Option<u32> {
    if v == 0 || v == u64::MAX {
        return None;
    }
    for size in [2_u32, 4, 8, 16, 32, 64] {
        let mask = if size == 64 { u64::MAX } else { (1 << size) - 1 };
        let elem = v & mask;
        let mut whole = 0_u64;
        let mut at = 0;
        while at < 64 {
            whole |= elem << at;
            at += size;
        }
        if whole != v {
            continue;
        }
        let ones = elem.count_ones();
        if ones == 0 || ones == size {
            return None;
        }
        let run = (1_u64 << ones) - 1;
        let rotr = |x: u64, r: u32| match r {
            0 => x,
            _ => ((x >> r) | (x << (size - r))) & mask,
        };
        return (0..size).any(|r| rotr(elem, r) == run).then_some(ones);
    }
    None
}

/// The field of the one 16-bit part of `v` that is not zero, `movcon` of `7l`.
fn movcon(v: u64) -> Option<u32> {
    (0..4).find(|s| v & !(0xffff_u64 << (16 * s)) == 0)
}

/// The class of a constant with no symbol.
fn constant(v: i64) -> &'static str {
    let u = u64::from_ne_bytes(v.to_ne_bytes());
    let bitcon = u >> 32 == 0 && bitmask(u).is_some_and(|s| s < 32);
    if v == 0 {
        return "C_ZCON";
    }
    if isaddcon(v) {
        return match () {
            () if bitcon => "C_ABCON",
            () if v <= 0xfff => "C_ADDCON0",
            () => "C_ADDCON",
        };
    }
    if movcon(u).is_some() || movcon(!u).is_some() {
        return if bitcon { "C_MBCON" } else { "C_MOVCON" };
    }
    if bitcon {
        "C_BITCON"
    } else if bitmask(u).is_some() {
        "C_BITCON64"
    } else if u >> 32 == 0 && bitmask(u | u << 32).is_some() {
        "C_BITCON32"
    } else {
        "C_LCON"
    }
}

/// The constant fits the immediate of `FMOV`: `chipfloat` of `7l`.
fn chipfloat(f: f64) -> bool {
    let bits = f.to_bits();
    let (l, h) = (bits & 0xffff_ffff, bits >> 32);
    if l != 0 || h & 0xffff != 0 {
        return false;
    }
    let n = h >> 16;
    matches!((n >> 6) & 0x1ff, 0x100 | 0xff)
}

impl Checker {
    /// Make the table from `optab7.rs`, as `buildop` in `7l/span.c` does.
    fn new() -> Self {
        let n = t::CLASSES.len();
        let mut cover = vec![vec![false; n]; n];
        for (i, row) in cover.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = cmp(t::CLASSES[i], t::CLASSES[j]);
            }
        }
        let mut entries: Map<&str, Vec<(u8, u8, u8)>> = Map::default();
        for &(op, a1, a2, a3, _, _) in t::OPTAB {
            entries.entry(op).or_default().push((a1, a2, a3));
        }
        for &(op, base) in t::RANGES {
            if let Some(e) = entries.get(base).cloned() {
                entries.insert(op, e);
            }
        }
        let class = t::CLASSES
            .iter()
            .enumerate()
            .filter_map(|(i, c)| Some((*c, u8::try_from(i).ok()?)))
            .collect();
        Self { cover, entries, class }
    }

    /// The value of the class `name`.
    fn y(&self, name: &str) -> u8 {
        self.class.get(name).copied().unwrap_or(0)
    }

    /// The name of the class `c`.
    pub(crate) fn name(c: u8) -> &'static str {
        t::CLASSES.get(usize::from(c)).copied().unwrap_or("C_GOK")
    }

    /// Each class that the operand can have: `aclass` of `7l`.
    fn aclass(a: &Addr7) -> Vec<&'static str> {
        let one = |c: &'static str| vec![c];
        let symbol = matches!(a.name, a7::D_EXTERN | a7::D_STATIC);
        match a.ty {
            a7::D_NONE => one("C_NONE"),
            a7::D_REG => one("C_REG"),
            a7::D_VREG => one("C_VREG"),
            a7::D_SP => one("C_RSP"),
            a7::D_COND => one("C_COND"),
            a7::D_SHIFT => one("C_SHIFT"),
            a7::D_EXTREG => one("C_EXTREG"),
            a7::D_ROFF => one("C_ROFF"),
            a7::D_XPOST => one("C_XPOST"),
            a7::D_XPRE => one("C_XPRE"),
            a7::D_FREG => one("C_FREG"),
            a7::D_SPR => one("C_SPR"),
            a7::D_FCONST => one("C_FCON"),
            a7::D_BRANCH => one("C_SBRA"),
            a7::D_OREG if symbol => EXTERN.to_vec(),
            a7::D_OREG if a.name == a7::D_NONE => one(OREG[constclass(a.offset)]),
            a7::D_OCONST if symbol => one("C_LCON"),
            a7::D_CONST if symbol => vec!["C_AECON", "C_LCON"],
            a7::D_CONST if a.name == a7::D_NONE => {
                if a.reg != a7::NREG && a.reg != REGZERO {
                    one(if isaddcon(a.offset) { "C_AACON" } else { "C_LACON" })
                } else {
                    one(constant(a.offset))
                }
            }
            // The offset of `D_AUTO` and `D_PARAM` depends on the frame of `7l`, and the
            // translation does not make them.
            _ => one("C_GOK"),
        }
    }

    /// Check one record. Return the classes of its operands and why `7l` cannot encode it, or
    /// `None` when it can.
    pub(crate) fn check(&self, p: &Prog7) -> Option<(u8, u8, u8, &'static str)> {
        let full = a7::NAMES.get(usize::from(p.op)).copied()?;
        let mut name = full.strip_prefix('A').unwrap_or(full);
        if PSEUDO.contains(&name) {
            return None;
        }
        let mut from = p.from.clone();
        let mut to = p.to.clone();
        if from.ty == a7::D_CONST && from.name == a7::D_NONE && from.offset < 0 {
            let other = match name {
                "SUB" => "ADD",
                "SUBW" => "ADDW",
                "SUBS" => "ADDS",
                "SUBSW" => "ADDSW",
                "ADD" => "SUB",
                "ADDW" => "SUBW",
                "ADDS" => "SUBS",
                "ADDSW" => "SUBSW",
                _ => name,
            };
            if other != name {
                name = other;
                from.offset = from.offset.wrapping_neg();
            }
        }
        if name == "FCVTDS" && from.ty == a7::D_FCONST {
            name = "FMOVS";
        }
        if matches!(name, "FMOVS" | "FMOVD") && from.ty == a7::D_FCONST && !chipfloat(from.float) {
            from = Addr7 { ty: a7::D_OREG, name: a7::D_EXTERN, sym: Some("$f".into()), ..from };
        }
        if matches!(name, "B" | "BL" | "RET" | "ERET") && to.ty != a7::D_BRANCH && to.sym.is_some()
        {
            to.ty = a7::D_BRANCH;
        }
        let middle = if p.reg == a7::NREG {
            "C_NONE"
        } else if FREG_MIDDLE.contains(&name) {
            "C_FREG"
        } else {
            "C_REG"
        };
        let a2 = self.y(middle);
        let (froms, tos) = (Self::aclass(&from), Self::aclass(&to));
        let first = (self.y(froms[0]), a2, self.y(tos[0]));
        let Some(entries) = self.entries.get(name) else {
            return Some((first.0, first.1, first.2, "the instruction has no entry in optab"));
        };
        for f in &froms {
            for t in &tos {
                let (a1, a3) = (self.y(f), self.y(t));
                let covers = |&(e1, e2, e3): &(u8, u8, u8)| {
                    (e2 == a2 || self.cover[usize::from(e2)][usize::from(a2)])
                        && self.cover[usize::from(e1)][usize::from(a1)]
                        && self.cover[usize::from(e3)][usize::from(a3)]
                };
                if !entries.iter().any(covers) {
                    return Some((
                        a1,
                        a2,
                        a3,
                        "no entry of optab covers the classes of its operands",
                    ));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(name: &str) -> u16 {
        rucc_object::plan9::Machine::Arm64.opcode(&format!("A{name}")).unwrap()
    }

    fn reg(r: u8) -> Addr7 {
        Addr7::register(a7::D_REG, r)
    }

    fn oreg(r: u8, offset: i64) -> Addr7 {
        Addr7 { ty: a7::D_OREG, reg: r, offset, ..Addr7::default() }
    }

    fn sym(ty: u8, s: &str) -> Addr7 {
        Addr7 { ty, name: a7::D_EXTERN, sym: Some(s.into()), ..Addr7::default() }
    }

    fn prog(name: &str, from: Addr7, middle: u8, to: Addr7) -> Prog7 {
        let mut p = Prog7::new(op(name), 0, from, to);
        p.reg = middle;
        p
    }

    #[test]
    fn constants_get_the_classes_of_7l() {
        assert_eq!(constant(0), "C_ZCON");
        assert_eq!(constant(1), "C_ABCON");
        assert_eq!(constant(5), "C_ADDCON0");
        assert_eq!(constant(0x1000), "C_ABCON");
        assert_eq!(constant(0x5000), "C_ADDCON");
        assert_eq!(constant(0x1200_0000), "C_MOVCON");
        assert_eq!(constant(0xff00_0000), "C_MBCON");
        assert_eq!(constant(-1), "C_MOVCON");
        assert_eq!(constant(0x00ff_ff00), "C_BITCON");
        assert_eq!(constant(0x5555_5555_5555_5555), "C_BITCON64");
        assert_eq!(constant(0x0f0f_0f0f), "C_BITCON32");
        assert_eq!(constant(0x8000_0001), "C_BITCON32");
        assert_eq!(constant(0x1234_5678), "C_LCON");
        assert_eq!(OREG[constclass(-8)], "C_NSOREG");
        assert_eq!(OREG[constclass(4096)], "C_UOREG8K");
        assert_eq!(OREG[constclass(4097)], "C_LOREG");
        assert!(chipfloat(1.0) && chipfloat(0.5) && !chipfloat(0.1) && !chipfloat(0.0));
    }

    #[test]
    fn the_cover_is_the_cmp_of_7l() {
        assert!(cmp("C_LCON", "C_MOVCON"));
        assert!(cmp("C_VCON", "C_ZCON"));
        assert!(cmp("C_LAUTO", "C_NSAUTO"));
        assert!(cmp("C_LAUTO", "C_PPAUTO"));
        assert!(cmp("C_UOREG64K", "C_UOREG4K"));
        assert!(cmp("C_LEXT", "C_SEXT1"));
        assert!(!cmp("C_SEXT1", "C_LEXT"));
        assert!(!cmp("C_ZCON", "C_REG"));
        assert!(!cmp("C_LCON", "C_AECON"));
    }

    #[test]
    fn common_instructions_pass_and_bad_ones_fail() {
        let c = &*CHECKER;
        let ok = |p: Prog7| assert!(c.check(&p).is_none(), "{p:?}: {:?}", c.check(&p));
        ok(prog("MOV", Addr7::constant(1), a7::NREG, reg(0)));
        ok(prog("MOV", Addr7::constant(0x1234_5678_9abc), a7::NREG, reg(0)));
        ok(prog("ADD", Addr7::constant(-16), a7::NREG, reg(1)));
        ok(prog("ADD", reg(2), 1, reg(3)));
        ok(prog("MOV", oreg(31, 8), a7::NREG, reg(0)));
        ok(prog("MOV", oreg(1, 100_000), a7::NREG, reg(0)));
        ok(prog("MOVW", reg(0), a7::NREG, oreg(31, 12)));
        ok(prog("MOV", sym(a7::D_OREG, "x"), a7::NREG, reg(0)));
        ok(prog("MOV", reg(0), a7::NREG, sym(a7::D_OREG, "x")));
        ok(prog("MOV", sym(a7::D_CONST, "x"), a7::NREG, reg(0)));
        ok(prog("BL", Addr7::default(), a7::NREG, sym(a7::D_OREG, "print")));
        ok(prog("B", Addr7::default(), a7::NREG, sym(a7::D_OREG, "f")));
        let mut f = Addr7::of(a7::D_FCONST);
        f.float = 0.1;
        ok(prog("FMOVD", f.clone(), a7::NREG, Addr7::register(a7::D_FREG, 0)));
        f.float = 2.0;
        ok(prog("FMOVD", f, a7::NREG, Addr7::register(a7::D_FREG, 0)));
        ok(prog("RETURN", Addr7::default(), a7::NREG, Addr7::default()));
        let bad = prog("ADD", reg(0), 1, oreg(2, 0));
        let (_, _, a3, why) = c.check(&bad).expect("ADD cannot write memory");
        assert_eq!(Checker::name(a3), "C_ZOREG");
        assert_eq!(why, "no entry of optab covers the classes of its operands");
        assert!(c.check(&prog("MOV", Addr7::constant(1), a7::NREG, oreg(0, 0))).is_some());
    }
}
