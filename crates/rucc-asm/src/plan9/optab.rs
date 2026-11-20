//! The check of each amd64 record against the tables of `6l` (07.3 of the rucc-plan9 plan).
//!
//! `doasm` in `6l/span.c` gives each operand a class with `oclass`, and looks for a pair of the
//! `ytab` of the opcode whose classes cover the two classes. When there is none, it tries
//! `ymovtab`. When that fails too, the link stops with "doasm: notfound". The translation checks
//! each record that it writes in the same way, with the tables of `optab6.rs`. Thus an
//! instruction that `6l` cannot encode is an error of the compiler on the host, with the line of
//! the listing, and not an error of the link in the guest.
//!
//! Before `span`, `6l` changes some operands. `ldobj` puts a floating constant of the single and
//! double precision instructions in memory, and `patch` makes the target of `CALL`, `RET` and
//! `JMPF` a branch when it names a symbol. `dostkoff` makes a `RET` with a constant source, which
//! is a tail call, an `ADJSP` and a `JMP` to its target. The check makes the same changes. The
//! other changes of `dostkoff` are to the offsets of the stack operands, which do not change
//! their classes.

use std::sync::LazyLock;

use rucc_base::hash::Map;
use rucc_object::plan9::{Addr6, Prog6, amd64 as a6};

use super::optab6 as t;

/// The opcodes whose floating constant `ldobj` puts in memory.
const FLOAT_OPS: &[&str] = &[
    "FMOVF", "FADDF", "FSUBF", "FSUBRF", "FMULF", "FDIVF", "FDIVRF", "FCOMF", "FCOMFP", "MOVSS",
    "ADDSS", "SUBSS", "MULSS", "DIVSS", "COMISS", "UCOMISS", "FMOVD", "FADDD", "FSUBD", "FSUBRD",
    "FMULD", "FDIVD", "FDIVRD", "FCOMD", "FCOMDP", "MOVSD", "ADDSD", "SUBSD", "MULSD", "DIVSD",
    "COMISD", "UCOMISD",
];

/// The records that `span` does not encode.
const PSEUDO: &[&str] =
    &["TEXT", "DATA", "GLOBL", "HISTORY", "END", "NAME", "SIGNAME", "DYNT", "INIT", "MODE"];

/// The tables of `6l`, ready for the check.
pub(crate) struct Checker {
    /// `cover[a][b]`: class `a` is covered by class `b`, the `ycover` of `6l`.
    cover: Vec<Vec<bool>>,
    /// The pairs of the `ytab` of each opcode name.
    pairs: Map<&'static str, &'static [t::Pair]>,
    /// The pairs of `ymovtab` of each opcode name.
    movs: Map<&'static str, Vec<(u8, u8)>>,
    /// The value of each class name.
    class: Map<&'static str, u8>,
}

/// The tables, made once.
pub(crate) static CHECKER: LazyLock<Checker> = LazyLock::new(Checker::new);

impl Checker {
    /// Make the tables from `optab6.rs`, as `instinit` in `6l/obj.c` does.
    fn new() -> Self {
        let n = t::CLASSES.len();
        let mut cover = vec![vec![false; n]; n];
        for (i, row) in cover.iter_mut().enumerate() {
            row[i] = true;
        }
        for &(a, b) in t::COVER {
            cover[usize::from(a)][usize::from(b)] = true;
        }
        let ytabs: Map<&str, &[t::Pair]> = t::YTABS.iter().copied().collect();
        let pairs = t::OPTAB
            .iter()
            .filter(|o| !o.1.is_empty())
            .filter_map(|o| Some((o.0, *ytabs.get(o.1)?)))
            .collect();
        let mut movs: Map<&str, Vec<(u8, u8)>> = Map::default();
        for &(op, a, b) in t::YMOVTAB {
            movs.entry(op).or_default().push((a, b));
        }
        let class = t::CLASSES
            .iter()
            .enumerate()
            .filter_map(|(i, c)| Some((*c, u8::try_from(i).ok()?)))
            .collect();
        Self { cover, pairs, movs, class }
    }

    /// The value of the class `name`.
    fn y(&self, name: &str) -> u8 {
        self.class.get(name).copied().unwrap_or(0)
    }

    /// The name of the class `y`.
    pub(crate) fn name(y: u8) -> &'static str {
        t::CLASSES.get(usize::from(y)).copied().unwrap_or("Yxxx")
    }

    /// The class of an operand, as `oclass` in `6l/span.c` gives it, in 64-bit mode.
    pub(crate) fn oclass(&self, a: &Addr6) -> u8 {
        let k = a.ty;
        if k >= a6::D_INDIR || a.index != a6::D_NONE {
            if a.index != a6::D_NONE && a.scale == 0 {
                if k == a6::D_ADDR {
                    return match a.index {
                        a6::D_EXTERN | a6::D_STATIC => self.y("Yi32"),
                        a6::D_AUTO | a6::D_PARAM => self.y("Yiauto"),
                        _ => self.y("Yxxx"),
                    };
                }
                return self.y("Ycol");
            }
            return self.y("Ym");
        }
        const SEG: [&str; 11] =
            ["Ycs", "Yss", "Yds", "Yes", "Yfs", "Ygs", "Ygdtr", "Yidtr", "Yldtr", "Ymsw", "Ytask"];
        let (bpb, r15b, r15, bh, task) = (5, 15, 31, 35, a6::D_CS + 10);
        let name = match k {
            a6::D_AL => "Yal",
            a6::D_AX => "Yax",
            2 | 3 => "Yrb",
            _ if (bpb..=r15b).contains(&k) || (a6::D_AH..=bh).contains(&k) => "Yrb",
            a6::D_CL => "Ycl",
            a6::D_CX => "Ycx",
            a6::D_DX | a6::D_BX => "Yrx",
            _ if (a6::D_SP..=r15).contains(&k) => "Yrl",
            a6::D_F0 => "Yf0",
            _ if (a6::D_F0 + 1..a6::D_F0 + 8).contains(&k) => "Yrf",
            _ if (a6::D_M0..a6::D_M0 + 8).contains(&k) => "Ymr",
            _ if (a6::D_X0..a6::D_X0 + 16).contains(&k) => "Yxr",
            a6::D_NONE => "Ynone",
            _ if (a6::D_CS..=task).contains(&k) => SEG[usize::from(k - a6::D_CS)],
            _ if (a6::D_CR..=a6::D_CR + 8).contains(&k) => {
                return self.y(&format!("Ycr{}", k - a6::D_CR));
            }
            _ if (a6::D_DR..a6::D_DR + 8).contains(&k) => {
                return self.y(&format!("Ydr{}", k - a6::D_DR));
            }
            _ if (a6::D_TR..a6::D_TR + 8).contains(&k) => {
                return self.y(&format!("Ytr{}", k - a6::D_TR));
            }
            a6::D_EXTERN | a6::D_STATIC | a6::D_AUTO | a6::D_PARAM => "Ym",
            a6::D_CONST | a6::D_ADDR => {
                if a.sym.is_some() {
                    "Yi32"
                } else {
                    match a.offset {
                        0 => "Yi0",
                        1 => "Yi1",
                        -128..=127 => "Yi8",
                        v if i32::try_from(v).is_ok() => "Ys32",
                        v if v >> 32 == 0 => "Yi32",
                        _ => "Yi64",
                    }
                }
            }
            a6::D_BRANCH => "Ybr",
            _ => "Yxxx",
        };
        self.y(name)
    }

    /// Check one record. Return the classes of its operands and why `6l` cannot encode it, or
    /// `None` when it can.
    pub(crate) fn check(&self, p: &Prog6) -> Option<(u8, u8, &'static str)> {
        let full = rucc_object::plan9::amd64::NAMES.get(usize::from(p.op)).copied()?;
        let mut name = full.strip_prefix('A').unwrap_or(full);
        if PSEUDO.contains(&name) {
            return None;
        }
        let mut from = p.from.clone();
        let mut to = p.to.clone();
        if from.ty == a6::D_FCONST && FLOAT_OPS.contains(&name) {
            from = Addr6::symbol(a6::D_EXTERN, "$float", 0);
        }
        if matches!(name, "CALL" | "RET" | "JMPF") && to.sym.is_some() {
            to.ty = a6::D_BRANCH;
        }
        if name == "RET" && from.ty == a6::D_CONST {
            // `dostkoff` makes this tail call an `ADJSP` and a `JMP` to the target.
            name = "JMP";
            from = Addr6::default();
        }
        let (ft, tt) = (self.oclass(&from), self.oclass(&to));
        let covers = |a: u8, b: u8| {
            self.cover[usize::from(ft)][usize::from(a)]
                && self.cover[usize::from(tt)][usize::from(b)]
        };
        let Some(pairs) = self.pairs.get(name) else {
            return Some((ft, tt, "the instruction has no ytab"));
        };
        if pairs.iter().any(|&(a, b, _, _)| covers(a, b)) {
            return None;
        }
        if self.movs.get(name).is_some_and(|m| m.iter().any(|&(a, b)| covers(a, b))) {
            return None;
        }
        Some((ft, tt, "no pair of its ytab or of ymovtab covers the classes of its operands"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(k: u8) -> Addr6 {
        Addr6::of(k)
    }

    fn op(name: &str) -> u16 {
        rucc_object::plan9::Machine::Amd64.opcode(&format!("A{name}")).unwrap()
    }

    fn prog(name: &str, from: Addr6, to: Addr6) -> Prog6 {
        Prog6::new(op(name), 0, from, to)
    }

    #[test]
    fn operands_get_the_classes_of_6l() {
        let c = &*CHECKER;
        let name = |a: &Addr6| Checker::name(c.oclass(a));
        assert_eq!(name(&reg(a6::D_AX)), "Yax");
        assert_eq!(name(&reg(a6::D_SP)), "Yrl");
        assert_eq!(name(&reg(a6::D_X0 + 3)), "Yxr");
        assert_eq!(name(&Addr6::constant(0)), "Yi0");
        assert_eq!(name(&Addr6::constant(-5)), "Yi8");
        assert_eq!(name(&Addr6::constant(100_000)), "Ys32");
        assert_eq!(name(&Addr6::constant(0xffff_ffff)), "Yi32");
        assert_eq!(name(&Addr6::constant(1 << 40)), "Yi64");
        assert_eq!(name(&reg(a6::D_INDIR + a6::D_SP)), "Ym");
        assert_eq!(name(&reg(4)), "Yxxx", "SPB has no class, as in 6l");
    }

    #[test]
    fn common_instructions_pass_and_bad_ones_fail() {
        let c = &*CHECKER;
        assert!(c.check(&prog("MOVQ", Addr6::constant(1), reg(a6::D_AX))).is_none());
        assert!(c.check(&prog("MOVQ", Addr6::constant(1 << 40), reg(a6::D_AX))).is_none());
        assert!(c.check(&prog("ADDQ", Addr6::constant(8), reg(a6::D_SP))).is_none());
        let print = Addr6::symbol(a6::D_EXTERN, "print", 0);
        assert!(c.check(&prog("CALL", Addr6::default(), print.clone())).is_none());
        assert!(c.check(&prog("RET", Addr6::default(), Addr6::default())).is_none());
        let tail = prog("RET", Addr6::constant(0), print);
        assert!(c.check(&tail).is_none(), "a tail call is a JMP");
        let store = prog("MOVQ", Addr6::constant(1 << 40), reg(a6::D_INDIR + a6::D_AX));
        assert!(c.check(&store).is_some(), "a 64-bit constant cannot go to memory");
        assert!(c.check(&prog("ADDQ", reg(a6::D_AX), Addr6::constant(1))).is_some());
    }
}
