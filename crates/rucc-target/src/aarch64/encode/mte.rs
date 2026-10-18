//! The memory tagging extension, which ARMv8.5 added and the kernel builds in once the assembler
//! takes `stgm`: kbuild turns `ARM64_MTE` on only when `.arch armv8.5-a+memtag` followed by
//! `stgm xzr, [x0]` assembles, so without these the kernel is configured without tagging at all
//! and lays `thread_struct` out without the field that keeps a task's tag checking mode.
//!
//! A tag is four bits kept for every sixteen bytes of memory and in the top byte of a pointer.
//! `irg`, `gmi`, `addg`, `subg` and `subp` work on the pointer, and the loads and stores, `ldg`,
//! `stg` and the rest, move the tag between a register and memory. `stgm`, `stzgm` and `ldgm` do
//! a whole block at once and only the kernel may run them.

use super::{At, Error, Mode, Offset, Value, Width};

impl At<'_> {
    /// One of the extension's instructions, or `None` for a mnemonic that is not one of them.
    pub(super) fn mte(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let word = match self.mnemonic {
            "irg" => match values {
                [d, n] => 0x9ac0_1000 | 31 << 16 | self.tagged_sp(n)? << 5 | self.tagged_sp(d)?,
                [d, n, m] => {
                    0x9ac0_1000
                        | self.tagged(m)? << 16
                        | self.tagged_sp(n)? << 5
                        | self.tagged_sp(d)?
                }
                _ => return Err(self.unwritten()),
            },
            "gmi" => match values {
                [d, n, m] => {
                    0x9ac0_1400
                        | self.tagged(m)? << 16
                        | self.tagged_sp(n)? << 5
                        | self.tagged(d)?
                }
                _ => return Err(self.unwritten()),
            },
            // The offset is a multiple of sixteen and the tag offset is four bits.
            "addg" | "subg" => match values {
                [d, n, Value::Imm(offset), Value::Imm(tag)] => {
                    let base = if self.mnemonic == "addg" { 0x9180_0000 } else { 0xd180_0000 };
                    if offset % 16 != 0 {
                        return Err(self.immediate(*offset));
                    }
                    let offset = self.number(offset / 16, 64)?;
                    let tag = self.number(*tag, 16)?;
                    base | offset << 16 | tag << 10 | self.tagged_sp(n)? << 5 | self.tagged_sp(d)?
                }
                _ => return Err(self.unwritten()),
            },
            "subp" | "subps" => match values {
                [d, n, m] => {
                    let base = if self.mnemonic == "subp" { 0x9ac0_0000 } else { 0xbac0_0000 };
                    base | self.tagged_sp(m)? << 16 | self.tagged_sp(n)? << 5 | self.tagged(d)?
                }
                _ => return Err(self.unwritten()),
            },
            // `subps` that keeps only the flags.
            "cmpp" => match values {
                [n, m] => 0xbac0_001f | self.tagged_sp(m)? << 16 | self.tagged_sp(n)? << 5,
                _ => return Err(self.unwritten()),
            },
            "stg" => self.tag_store(0b00, values)?,
            "stzg" => self.tag_store(0b01, values)?,
            "st2g" => self.tag_store(0b10, values)?,
            "stz2g" => self.tag_store(0b11, values)?,
            "ldg" => match values {
                [t, Value::Mem(addr)] if addr.mode == Mode::Offset => {
                    0xd960_0000
                        | self.granules(addr.offset, 9)? << 12
                        | self.tag_base(addr.base, *t)?
                }
                _ => return Err(self.unwritten()),
            },
            "stzgm" | "stgm" | "ldgm" => match values {
                [t, Value::Mem(addr)]
                    if addr.mode == Mode::Offset && addr.offset == Offset::Imm(0) =>
                {
                    let opc = match self.mnemonic {
                        "stzgm" => 0b00,
                        "stgm" => 0b10,
                        _ => 0b11,
                    };
                    0xd920_0000 | opc << 22 | self.tag_base(addr.base, *t)?
                }
                _ => return Err(self.unwritten()),
            },
            "stgp" => match values {
                [t1, t2, Value::Mem(addr)] => {
                    let index = match addr.mode {
                        Mode::Post => 0b01,
                        Mode::Offset => 0b10,
                        Mode::Pre => 0b11,
                    };
                    0x6800_0000
                        | index << 23
                        | self.granules(addr.offset, 7)? << 15
                        | self.tagged(t2)? << 10
                        | self.tag_base(addr.base, *t1)?
                }
                _ => return Err(self.unwritten()),
            },
            _ => return Ok(None),
        };
        Ok(Some(word))
    }

    /// `stg` and the three stores like it, whose register may be the stack pointer since what
    /// they store is the tag in a pointer, and the stack pointer is one.
    fn tag_store(self, opc: u32, values: &[Value]) -> Result<u32, Error> {
        let [t, Value::Mem(addr)] = values else {
            return Err(self.unwritten());
        };
        let index = match addr.mode {
            Mode::Post => 0b01,
            Mode::Offset => 0b10,
            Mode::Pre => 0b11,
        };
        Ok(0xd920_0000
            | opc << 22
            | self.granules(addr.offset, 9)? << 12
            | index << 10
            | u32::from(addr.base) << 5
            | self.tagged_sp(t)?)
    }

    /// The base of a tag access and the register it moves, which is never the stack pointer.
    fn tag_base(self, base: u8, t: Value) -> Result<u32, Error> {
        Ok(u32::from(base) << 5 | self.tagged(&t)?)
    }

    /// An offset in sixteen byte granules, as a signed field of `bits` bits.
    fn granules(self, offset: Offset, bits: u32) -> Result<u32, Error> {
        let Offset::Imm(bytes) = offset else {
            return Err(self.unwritten());
        };
        let half = 1i64 << (bits - 1);
        if bytes % 16 != 0 || !(-half..half).contains(&(bytes / 16)) {
            return Err(self.immediate(bytes));
        }
        Ok(((bytes / 16) as u32) & ((1 << bits) - 1))
    }

    /// A sixty four bit register where thirty one is the zero register. Tags live in pointers,
    /// so none of these instructions has a thirty two bit form.
    fn tagged(self, value: &Value) -> Result<u32, Error> {
        match self.zr(value)? {
            (Width::X, number) => Ok(number),
            (Width::W, _) => Err(self.register()),
        }
    }

    /// The same where thirty one is the stack pointer.
    fn tagged_sp(self, value: &Value) -> Result<u32, Error> {
        match self.sp(value)? {
            (Width::X, number) => Ok(number),
            (Width::W, _) => Err(self.register()),
        }
    }
}
