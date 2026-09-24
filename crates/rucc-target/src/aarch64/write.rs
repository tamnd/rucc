//! Writing one AArch64 instruction as a line of assembly GNU as reads.
//!
//! The other direction from [`read`](crate::aarch64::read), and over the same [`Value`]s, so a
//! line this writes is a line that reads back as the instruction it was written from. That is what
//! `-S` needs: the listing is written from the operands the encoder was handed rather than from a
//! second description of them, so the two cannot come to disagree about what an instruction is.
//!
//! The spelling is GNU as's where there is a choice. A number is written in decimal, which is not
//! always what objdump prints but is always what the assembler reads.
//!
//! The one place the two assemblers disagree is how a line asks for part of a symbol's address.
//! GNU as puts an operator in front of the name, as in `:lo12:counter`, and Apple's assembler puts
//! one after it, as in `_counter@PAGEOFF`, and neither reads the other's. A bare name in `adrp`
//! means its page to GNU as and is an error to Apple's, which wants `@PAGE` said out loud.

use std::fmt::Write;

use crate::aarch64::encode::{
    Addr, Arrangement, Cond, Extend, Mode, Offset, Operator, Scalar, Shift, Value, Width,
};
use crate::aarch64::read::{BARRIERS, SYSTEM, system_field};

/// Which assembler a line is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spelling {
    /// GNU as, and anything that reads what it reads, which is every ELF and COFF target.
    Gnu,
    /// The assembler in Apple's toolchain, which is the one that writes Mach-O.
    Apple,
}

/// One instruction, as a line with no indentation and no newline.
///
/// The symbol is the name every [`Value::Symbol`] and [`Offset::Symbol`] in the operands stands
/// for, which is how [`read`](crate::aarch64::read) hands it back as well. An operand that names
/// one when there is none is written as `?`, which nothing reads.
#[must_use]
pub fn write(mnemonic: &str, values: &[Value], symbol: Option<&str>, spelling: Spelling) -> String {
    let named = Named { symbol, spelling, page: mnemonic == "adrp" };
    let mut line = mnemonic.to_owned();
    for (at, value) in values.iter().enumerate() {
        line.push_str(if at == 0 { " " } else { ", " });
        operand(&mut line, value, named);
    }
    line
}

/// What a symbol in an operand is written with.
#[derive(Clone, Copy)]
struct Named<'a> {
    symbol: Option<&'a str>,
    spelling: Spelling,
    /// Whether the instruction is `adrp`, where a bare name means the page it is on.
    page: bool,
}

fn operand(line: &mut String, value: &Value, named: Named<'_>) {
    match *value {
        Value::Gpr(width, number) => line.push_str(&gpr(width, number)),
        Value::Sp(Width::X) => line.push_str("sp"),
        Value::Sp(Width::W) => line.push_str("wsp"),
        Value::Fp(scalar, number) => {
            let letter = match scalar {
                Scalar::B => 'b',
                Scalar::H => 'h',
                Scalar::S => 's',
                Scalar::D => 'd',
                Scalar::Q => 'q',
            };
            let _ = write!(line, "{letter}{number}");
        }
        Value::Vector(arrangement, number) => {
            let lanes = match arrangement {
                Arrangement::B8 => "8b",
                Arrangement::B16 => "16b",
                Arrangement::D2 => "2d",
            };
            let _ = write!(line, "v{number}.{lanes}");
        }
        Value::Imm(imm) => {
            let _ = write!(line, "#{imm}");
        }
        // Debug rather than Display, which writes two as `2` and would read back as an integer.
        Value::Float(number) => {
            let _ = write!(line, "#{number:?}");
        }
        Value::Shift(shift, amount) => {
            let _ = write!(line, "{} #{amount}", shift_name(shift));
        }
        Value::Extend(extend, amount) => {
            line.push_str(extend_name(extend));
            if let Some(amount) = amount {
                let _ = write!(line, " #{amount}");
            }
        }
        Value::Cond(cond) => line.push_str(cond_name(cond)),
        Value::Mem(addr) => address(line, addr, named),
        Value::Symbol(operator) => reference(line, operator, named),
        Value::Barrier(option) => match BARRIERS.iter().find(|&&(_, known)| known == option) {
            Some((name, _)) => line.push_str(name),
            None => {
                let _ = write!(line, "#{option}");
            }
        },
        Value::System(field) => {
            match SYSTEM.iter().find(|&&(_, fields)| system_field(fields) == field) {
                Some((name, _)) => line.push_str(name),
                None => {
                    let (op0, op1) = ((field >> 14) + 2, (field >> 11) & 7);
                    let (crn, crm, op2) = ((field >> 7) & 15, (field >> 3) & 15, field & 7);
                    let _ = write!(line, "s{op0}_{op1}_c{crn}_c{crm}_{op2}");
                }
            }
        }
    }
}

fn gpr(width: Width, number: u8) -> String {
    match (width, number) {
        (Width::X, 31) => "xzr".to_owned(),
        (Width::W, 31) => "wzr".to_owned(),
        (Width::X, _) => format!("x{number}"),
        (Width::W, _) => format!("w{number}"),
    }
}

fn address(line: &mut String, addr: Addr, named: Named<'_>) {
    let base = if addr.base == 31 { "sp".to_owned() } else { format!("x{}", addr.base) };
    let _ = write!(line, "[{base}");
    match (addr.offset, addr.mode) {
        (Offset::Imm(0), Mode::Offset) => line.push(']'),
        (Offset::Imm(imm), Mode::Offset) => {
            let _ = write!(line, ", #{imm}]");
        }
        (Offset::Imm(imm), Mode::Pre) => {
            let _ = write!(line, ", #{imm}]!");
        }
        (Offset::Imm(imm), Mode::Post) => {
            let _ = write!(line, "], #{imm}");
        }
        (Offset::Reg { reg, extend, amount }, _) => {
            let width =
                if matches!(extend, Extend::Uxtw | Extend::Sxtw) { Width::W } else { Width::X };
            let _ = write!(line, ", {}", gpr(width, reg));
            match (extend, amount) {
                (Extend::Uxtx, None) => {}
                (Extend::Uxtx, Some(amount)) => {
                    let _ = write!(line, ", lsl #{amount}");
                }
                (extend, None) => {
                    let _ = write!(line, ", {}", extend_name(extend));
                }
                (extend, Some(amount)) => {
                    let _ = write!(line, ", {} #{amount}", extend_name(extend));
                }
            }
            line.push(']');
        }
        (Offset::Symbol(operator), _) => {
            line.push_str(", ");
            reference(line, operator, named);
            line.push(']');
        }
    }
}

fn reference(line: &mut String, operator: Operator, named: Named<'_>) {
    let symbol = named.symbol.unwrap_or("?");
    if named.spelling == Spelling::Apple {
        // The slot a thread-local's offset is in is a slot holding its descriptor's address on
        // Apple's platforms, which the code calls through. The two offsets from the thread pointer
        // have no suffix here, since Darwin has no such offsets, and are written the GNU way,
        // which Apple's assembler rejects rather than misreads.
        let suffix = match operator {
            Operator::Plain if named.page => Some("@PAGE"),
            Operator::Plain => Some(""),
            Operator::Lo12 => Some("@PAGEOFF"),
            Operator::Got => Some("@GOTPAGE"),
            Operator::GotLo12 => Some("@GOTPAGEOFF"),
            Operator::GotTprel => Some("@TLVPPAGE"),
            Operator::GotTprelLo12 => Some("@TLVPPAGEOFF"),
            Operator::TprelHi12 | Operator::TprelLo12Nc => None,
        };
        if let Some(suffix) = suffix {
            let _ = write!(line, "{symbol}{suffix}");
            return;
        }
    }
    let prefix = match operator {
        Operator::Plain => "",
        Operator::Lo12 => ":lo12:",
        Operator::Got => ":got:",
        Operator::GotLo12 => ":got_lo12:",
        Operator::GotTprel => ":gottprel:",
        Operator::GotTprelLo12 => ":gottprel_lo12:",
        Operator::TprelHi12 => ":tprel_hi12:",
        Operator::TprelLo12Nc => ":tprel_lo12_nc:",
    };
    let _ = write!(line, "{prefix}{symbol}");
}

fn shift_name(shift: Shift) -> &'static str {
    match shift {
        Shift::Lsl => "lsl",
        Shift::Lsr => "lsr",
        Shift::Asr => "asr",
        Shift::Ror => "ror",
    }
}

fn extend_name(extend: Extend) -> &'static str {
    match extend {
        Extend::Uxtb => "uxtb",
        Extend::Uxth => "uxth",
        Extend::Uxtw => "uxtw",
        Extend::Uxtx => "uxtx",
        Extend::Sxtb => "sxtb",
        Extend::Sxth => "sxth",
        Extend::Sxtw => "sxtw",
        Extend::Sxtx => "sxtx",
    }
}

/// The name GNU as reads a condition by, which for the carry pair is `hs` and `lo`.
#[must_use]
pub fn cond_name(cond: Cond) -> &'static str {
    match cond {
        Cond::Eq => "eq",
        Cond::Ne => "ne",
        Cond::Hs => "hs",
        Cond::Lo => "lo",
        Cond::Mi => "mi",
        Cond::Pl => "pl",
        Cond::Vs => "vs",
        Cond::Vc => "vc",
        Cond::Hi => "hi",
        Cond::Ls => "ls",
        Cond::Ge => "ge",
        Cond::Lt => "lt",
        Cond::Gt => "gt",
        Cond::Le => "le",
        Cond::Al => "al",
        Cond::Nv => "nv",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aarch64::read;

    #[test]
    fn every_line_gnu_as_was_given_reads_back_the_same_once_written() {
        let mut wrong = Vec::new();
        for line in include_str!("golden.txt").lines() {
            let Some(text) = line.split('\t').nth(1).filter(|_| !line.starts_with('#')) else {
                continue;
            };
            let first = read(text).expect("golden.txt reads");
            let written =
                write(&first.mnemonic, &first.values, first.symbol.as_deref(), Spelling::Gnu);
            match read(&written) {
                Ok(again) if again == first => {}
                Ok(again) => wrong.push(format!("{text} was written as {written}: {again:?}")),
                Err(e) => wrong.push(format!("{text} was written as {written}: {e}")),
            }
        }
        assert!(wrong.is_empty(), "{} lines differ:\n{}", wrong.len(), wrong.join("\n"));
    }

    #[test]
    fn a_line_is_spelled_the_way_gnu_as_spells_it() {
        let line = |text: &str| {
            let read = read(text).unwrap();
            write(&read.mnemonic, &read.values, read.symbol.as_deref(), Spelling::Gnu)
        };
        assert_eq!(line("ldr x0, [sp, #0x10]"), "ldr x0, [sp, #16]");
        assert_eq!(line("str x19, [sp, #-16]!"), "str x19, [sp, #-16]!");
        assert_eq!(line("ldr x19, [sp], #16"), "ldr x19, [sp], #16");
        assert_eq!(line("add x0, x0, :lo12:counter"), "add x0, x0, :lo12:counter");
        assert_eq!(line("fmov d0, #2.0"), "fmov d0, #2.0");
        assert_eq!(line("mrs x0, tpidr_el0"), "mrs x0, tpidr_el0");
        assert_eq!(line("dmb ish"), "dmb ish");
        assert_eq!(line("ldr w0, [x1, w2, sxtw #2]"), "ldr w0, [x1, w2, sxtw #2]");
    }

    #[test]
    fn apple_asks_for_part_of_an_address_after_the_name_rather_than_before_it() {
        let apple = |text: &str| {
            let read = read(text).unwrap();
            write(&read.mnemonic, &read.values, Some("_counter"), Spelling::Apple)
        };
        assert_eq!(apple("adrp x0, counter"), "adrp x0, _counter@PAGE");
        assert_eq!(apple("add x0, x0, :lo12:counter"), "add x0, x0, _counter@PAGEOFF");
        assert_eq!(apple("adrp x0, :got:counter"), "adrp x0, _counter@GOTPAGE");
        assert_eq!(apple("ldr x0, [x0, :got_lo12:counter]"), "ldr x0, [x0, _counter@GOTPAGEOFF]");
        assert_eq!(apple("bl counter"), "bl _counter");
        assert_eq!(apple("adr x0, counter"), "adr x0, _counter");
        assert_eq!(apple("adrp x0, :gottprel:counter"), "adrp x0, _counter@TLVPPAGE");
        let slot = "ldr x0, [x0, :gottprel_lo12:counter]";
        assert_eq!(apple(slot), "ldr x0, [x0, _counter@TLVPPAGEOFF]");
    }
}
