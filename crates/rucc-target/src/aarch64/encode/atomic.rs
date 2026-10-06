//! The large system extension's atomics, which ARMv8.1 added and every arm64 kernel writes in
//! inline assembly once it has checked the CPU has them: `ldadd` and the seven other operations
//! that read a word and write it back changed, `swp`, `cas`, `casp`, and the `st` forms that are an
//! operation whose old value goes to the zero register.
//!
//! Each mnemonic is an operation, then `a` for acquire, `l` for release or `al` for both, then `b`
//! or `h` for a byte or a half word. Without either the width is the register's.

use super::{At, Error, Mode, Offset, Value, Width};

/// What a mnemonic of this family asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Atomic {
    kind: Kind,
    acquire: bool,
    release: bool,
    /// `b` or `h`, as the two bits of `size`, or `None` for the register's width.
    size: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A read, an operation and a write, as `o3` and `opc`, with the old value kept or not.
    Op {
        o3: u32,
        opc: u32,
        keep: bool,
    },
    Cas,
    Casp,
}

/// The eight operations, in the order of their `opc`.
const OPS: [&str; 8] = ["add", "clr", "eor", "set", "smax", "smin", "umax", "umin"];

/// The mnemonic read as one of this family, or `None` for any other.
pub(super) fn wanted(m: &str) -> Option<Atomic> {
    let (kind, rest) = if let Some(rest) = m.strip_prefix("casp") {
        (Kind::Casp, rest)
    } else if let Some(rest) = m.strip_prefix("cas") {
        (Kind::Cas, rest)
    } else if let Some(rest) = m.strip_prefix("swp") {
        (Kind::Op { o3: 1, opc: 0, keep: true }, rest)
    } else {
        let (keep, after) = match m.get(..2)? {
            "ld" => (true, &m[2..]),
            "st" => (false, &m[2..]),
            _ => return None,
        };
        let (opc, op) = OPS.iter().enumerate().find(|(_, op)| after.starts_with(**op))?;
        (Kind::Op { o3: 0, opc: opc as u32, keep }, &after[op.len()..])
    };
    let (rest, size) = match rest.as_bytes().last() {
        Some(b'b') if kind != Kind::Casp => (&rest[..rest.len() - 1], Some(0b00)),
        Some(b'h') if kind != Kind::Casp => (&rest[..rest.len() - 1], Some(0b01)),
        _ => (rest, None),
    };
    let (acquire, release) = match rest {
        "" => (false, false),
        "a" => (true, false),
        "l" => (false, true),
        "al" => (true, true),
        _ => return None,
    };
    // A store keeps nothing, so there is nothing for an acquire to order.
    if acquire && matches!(kind, Kind::Op { keep: false, .. }) {
        return None;
    }
    Some(Atomic { kind, acquire, release, size })
}

impl At<'_> {
    /// One of the family, which [`wanted`] has already read the mnemonic of.
    pub(super) fn atomic(self, atomic: Atomic, values: &[Value]) -> Result<u32, Error> {
        let [regs @ .., Value::Mem(addr)] = values else {
            return Err(self.unwritten());
        };
        if addr.offset != Offset::Imm(0) || addr.mode != Mode::Offset {
            return Err(self.unwritten());
        }
        let regs = regs.iter().map(|reg| self.zr(reg)).collect::<Result<Vec<_>, _>>()?;
        let Some(&(width, _)) = regs.first() else {
            return Err(self.unwritten());
        };
        if regs.iter().any(|&(other, _)| other != width) {
            return Err(self.register());
        }
        if atomic.size.is_some() && width != Width::W {
            return Err(self.register());
        }
        let size = atomic.size.unwrap_or(if width == Width::X { 0b11 } else { 0b10 });
        let rn = u32::from(addr.base) << 5;
        let acquire = u32::from(atomic.acquire);
        let release = u32::from(atomic.release);
        match (atomic.kind, regs.as_slice()) {
            (Kind::Op { o3, opc, keep: true }, &[(_, rs), (_, rt)]) => Ok(size << 30
                | 0x3820_0000
                | acquire << 23
                | release << 22
                | rs << 16
                | o3 << 15
                | opc << 12
                | rn
                | rt),
            (Kind::Op { o3, opc, keep: false }, &[(_, rs)]) => Ok(size << 30
                | 0x3820_0000
                | release << 22
                | rs << 16
                | o3 << 15
                | opc << 12
                | rn
                | 31),
            (Kind::Cas, &[(_, rs), (_, rt)]) => {
                Ok(size << 30 | 0x08a0_7c00 | acquire << 22 | rs << 16 | release << 15 | rn | rt)
            }
            // Two pairs, each an even register and the one after it, and only the first of each
            // is in the word.
            (Kind::Casp, &[(_, rs), (_, rs1), (_, rt), (_, rt1)]) => {
                if rs % 2 != 0 || rt % 2 != 0 || rs1 != rs + 1 || rt1 != rt + 1 {
                    return Err(self.register());
                }
                let sz = u32::from(width == Width::X);
                Ok(sz << 30 | 0x0820_7c00 | acquire << 22 | rs << 16 | release << 15 | rn | rt)
            }
            _ => Err(self.unwritten()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mnemonic_is_an_operation_an_order_and_a_width() {
        let ldaddalb = wanted("ldaddalb").expect("one of them");
        assert_eq!((ldaddalb.acquire, ldaddalb.release, ldaddalb.size), (true, true, Some(0)));
        assert!(wanted("staddlh").is_some() && wanted("caspal").is_some());
        // Not this family: the exclusive and ordered loads, a store with nothing to acquire, and a
        // pair with a width suffix, which does not exist.
        for m in ["ldaxr", "ldar", "stlr", "stxr", "ldp", "stadda", "caspb", "ldaddx", "ld", "st"] {
            assert_eq!(wanted(m), None, "{m}");
        }
    }
}
