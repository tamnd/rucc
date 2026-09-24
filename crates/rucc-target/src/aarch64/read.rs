//! Reading one line of AArch64 assembly in the syntax GNU as takes.
//!
//! This is the half of an assembler that turns `ldr x0, [sp, #16]` into a mnemonic and the
//! [`Value`]s [`encode`](crate::aarch64::encode) takes. It reads one instruction and nothing
//! around it: no labels, no directives, no comments and no expressions, since what writes those is
//! a line reader over this rather than this. What it is for now is the tests, which read every
//! line GNU as was given and check that the word is the same, and later the inline assembly and
//! the `.s` files the driver is handed for this machine.
//!
//! A name is a register, a condition or a barrier option before it is a symbol, the way GNU as
//! reads them, so a symbol spelled `lo` or `ish` has to be written some other way.

use std::fmt;

use crate::aarch64::encode::{
    Addr, Arrangement, Cond, Extend, Mode, Offset, Operator, Scalar, Shift, Value, Width,
};

/// One instruction, read.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Its mnemonic, in lower case.
    pub mnemonic: String,
    /// Its operands, in the order they were written.
    pub values: Vec<Value>,
    /// The symbol it names, when it names one. [`Value::Symbol`] and [`Offset::Symbol`] say where
    /// and which part of its address, and this says which symbol.
    pub symbol: Option<String>,
}

/// Why a line could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// The operand that could not be read, or the whole line.
    pub text: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot read `{}` as aarch64 assembly", self.text)
    }
}

impl std::error::Error for Error {}

fn error(text: &str) -> Error {
    Error { text: text.to_owned() }
}

/// Reads one instruction.
///
/// # Errors
///
/// When an operand is none of the things an instruction here takes. See [`Error`].
pub fn read(text: &str) -> Result<Line, Error> {
    let text = text.trim();
    let (mnemonic, rest) = match text.find(char::is_whitespace) {
        Some(at) => (&text[..at], text[at..].trim()),
        None => (text, ""),
    };
    if mnemonic.is_empty() {
        return Err(error(text));
    }
    let mut line = Line { mnemonic: mnemonic.to_ascii_lowercase(), values: Vec::new(), symbol: None };
    let pieces = split(rest);
    let mut at = 0;
    while at < pieces.len() {
        let piece = pieces[at];
        if piece.starts_with('[') {
            // A post-indexed address is written as two operands, the address and then the amount
            // it moves by, and it is one operand to the machine.
            let mut addr = address(piece, &mut line.symbol)?;
            if addr.mode == Mode::Offset && piece.ends_with(']') && at + 1 < pieces.len() {
                if let (Offset::Imm(0), Some(imm)) =
                    (addr.offset, pieces[at + 1].strip_prefix('#').and_then(number))
                {
                    addr.offset = Offset::Imm(imm);
                    addr.mode = Mode::Post;
                    at += 1;
                }
            }
            line.values.push(Value::Mem(addr));
        } else {
            line.values.push(operand(piece, &mut line.symbol)?);
        }
        at += 1;
    }
    Ok(line)
}

/// The operands, split at the commas that are not inside an address.
fn split(text: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (at, c) in text.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                pieces.push(text[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    if !text[start..].trim().is_empty() {
        pieces.push(text[start..].trim());
    }
    pieces
}

/// One operand that is not an address.
fn operand(piece: &str, symbol: &mut Option<String>) -> Result<Value, Error> {
    let lower = piece.to_ascii_lowercase();
    if let Some(value) = register(&lower) {
        return Ok(value);
    }
    if let Some(imm) = lower.strip_prefix('#') {
        if let Some(value) = number(imm) {
            return Ok(Value::Imm(value));
        }
        if let Ok(value) = imm.parse::<f64>() {
            return Ok(Value::Float(value));
        }
        if imm.starts_with(':') {
            return reference(piece.trim_start_matches('#'), symbol).map(Value::Symbol);
        }
        return Err(error(piece));
    }
    if let Some(value) = modifier(&lower)? {
        return Ok(value);
    }
    if let Some(cond) = Cond::named(&lower) {
        return Ok(Value::Cond(cond));
    }
    if let Some(option) = barrier(&lower) {
        return Ok(Value::Barrier(option));
    }
    if let Some(field) = system(&lower) {
        return Ok(Value::System(field));
    }
    reference(piece, symbol).map(Value::Symbol)
}

/// A shift or an extension, with its amount.
fn modifier(lower: &str) -> Result<Option<Value>, Error> {
    let (name, amount) = match lower.split_once(char::is_whitespace) {
        Some((name, amount)) => (name, Some(amount.trim())),
        None => (lower, None),
    };
    let amount = match amount {
        Some(amount) => {
            let digits = amount.strip_prefix('#').unwrap_or(amount);
            let value = number(digits).and_then(|n| u8::try_from(n).ok());
            Some(value.ok_or_else(|| error(lower))?)
        }
        None => None,
    };
    let shift = match name {
        "lsl" => Some(Shift::Lsl),
        "lsr" => Some(Shift::Lsr),
        "asr" => Some(Shift::Asr),
        "ror" => Some(Shift::Ror),
        _ => None,
    };
    if let Some(shift) = shift {
        return amount.map(|amount| Some(Value::Shift(shift, amount))).ok_or_else(|| error(lower));
    }
    Ok(extension(name).map(|extend| Value::Extend(extend, amount)))
}

fn extension(name: &str) -> Option<Extend> {
    Some(match name {
        "uxtb" => Extend::Uxtb,
        "uxth" => Extend::Uxth,
        "uxtw" => Extend::Uxtw,
        "uxtx" => Extend::Uxtx,
        "sxtb" => Extend::Sxtb,
        "sxth" => Extend::Sxth,
        "sxtw" => Extend::Sxtw,
        "sxtx" => Extend::Sxtx,
        _ => return None,
    })
}

/// A register, by any of the names GNU as gives it.
fn register(name: &str) -> Option<Value> {
    match name {
        "sp" => return Some(Value::Sp(Width::X)),
        "wsp" => return Some(Value::Sp(Width::W)),
        "xzr" => return Some(Value::Gpr(Width::X, 31)),
        "wzr" => return Some(Value::Gpr(Width::W, 31)),
        "fp" => return Some(Value::Gpr(Width::X, 29)),
        "lr" => return Some(Value::Gpr(Width::X, 30)),
        _ => {}
    }
    if let Some((number, lanes)) = name.strip_prefix('v').and_then(|rest| rest.split_once('.')) {
        let arrangement = match lanes {
            "8b" => Arrangement::B8,
            "16b" => Arrangement::B16,
            "2d" => Arrangement::D2,
            _ => return None,
        };
        return Some(Value::Vector(arrangement, numbered(number, 32)?));
    }
    let (first, number) = name.split_at(name.char_indices().nth(1)?.0);
    Some(match first {
        "x" => Value::Gpr(Width::X, numbered(number, 31)?),
        "w" => Value::Gpr(Width::W, numbered(number, 31)?),
        "b" => Value::Fp(Scalar::B, numbered(number, 32)?),
        "h" => Value::Fp(Scalar::H, numbered(number, 32)?),
        "s" => Value::Fp(Scalar::S, numbered(number, 32)?),
        "d" => Value::Fp(Scalar::D, numbered(number, 32)?),
        "q" => Value::Fp(Scalar::Q, numbered(number, 32)?),
        _ => return None,
    })
}

/// A register number below a limit, written in decimal with no leading zero.
fn numbered(text: &str, below: u8) -> Option<u8> {
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    text.parse::<u8>().ok().filter(|&number| number < below)
}

/// A whole number, in decimal or in hexadecimal after `0x`, with a sign or without.
fn number(text: &str) -> Option<i64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, text),
    };
    let magnitude = match digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u64>().ok()?,
    };
    // A number past the largest signed one is its bits, the way GNU as reads
    // `#0xffffffffffffffff`.
    let value = magnitude as i64;
    Some(if negative { value.wrapping_neg() } else { value })
}

/// A symbol with or without an operator in front of it, saving its name.
fn reference(text: &str, symbol: &mut Option<String>) -> Result<Operator, Error> {
    let (operator, name) = match text.strip_prefix(':') {
        Some(rest) => {
            let (operator, name) = rest.split_once(':').ok_or_else(|| error(text))?;
            let operator = match operator.to_ascii_lowercase().as_str() {
                "lo12" => Operator::Lo12,
                "got" => Operator::Got,
                "got_lo12" => Operator::GotLo12,
                "tprel_hi12" => Operator::TprelHi12,
                "tprel_lo12_nc" => Operator::TprelLo12Nc,
                _ => return Err(error(text)),
            };
            (operator, name)
        }
        None => (Operator::Plain, text),
    };
    let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || "_.$".contains(c))
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "_.$".contains(c));
    if !valid {
        return Err(error(text));
    }
    *symbol = Some(name.to_owned());
    Ok(operator)
}

/// A barrier option, as the number the encoding gives it.
fn barrier(name: &str) -> Option<u8> {
    Some(match name {
        "oshld" => 1,
        "oshst" => 2,
        "osh" => 3,
        "nshld" => 5,
        "nshst" => 6,
        "nsh" => 7,
        "ishld" => 9,
        "ishst" => 10,
        "ish" => 11,
        "ld" => 13,
        "st" => 14,
        "sy" => 15,
        _ => return None,
    })
}

/// A system register, as the fifteen bits `mrs` and `msr` carry for it.
///
/// The ones a compiler reads by name, and any of them written the generic way, as
/// `s3_3_c13_c0_2`.
fn system(name: &str) -> Option<u16> {
    let (op0, op1, crn, crm, op2) = match name {
        "nzcv" => (3, 3, 4, 2, 0),
        "fpcr" => (3, 3, 4, 4, 0),
        "fpsr" => (3, 3, 4, 4, 1),
        "tpidr_el0" => (3, 3, 13, 0, 2),
        "tpidrro_el0" => (3, 3, 13, 0, 3),
        "cntfrq_el0" => (3, 3, 14, 0, 0),
        "cntvct_el0" => (3, 3, 14, 0, 2),
        "dczid_el0" => (3, 3, 0, 0, 7),
        "ctr_el0" => (3, 3, 0, 0, 1),
        _ => {
            let mut parts = name.strip_prefix('s')?.split('_');
            let mut next = |prefix: &str, below: u16| {
                let part = parts.next()?;
                part.strip_prefix(prefix)?.parse::<u16>().ok().filter(|&n| n < below)
            };
            let fields = (next("", 4)?, next("", 8)?, next("c", 16)?, next("c", 16)?, next("", 8)?);
            if parts.next().is_some() || fields.0 < 2 {
                return None;
            }
            fields
        }
    };
    Some((op0 - 2) << 14 | op1 << 11 | crn << 7 | crm << 3 | op2)
}

/// An address, `[x0]`, `[x0, #8]`, `[x0, #8]!`, `[x0, x1, lsl #3]` or `[x0, :lo12:name]`.
fn address(piece: &str, symbol: &mut Option<String>) -> Result<Addr, Error> {
    let (inner, mode) = if let Some(inner) = piece.strip_suffix("]!") {
        (inner, Mode::Pre)
    } else {
        (piece.strip_suffix(']').ok_or_else(|| error(piece))?, Mode::Offset)
    };
    let inner = inner.strip_prefix('[').ok_or_else(|| error(piece))?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    let base = match register(&parts[0].to_ascii_lowercase()) {
        Some(Value::Sp(Width::X)) => 31,
        Some(Value::Gpr(Width::X, number)) if number < 31 => number,
        _ => return Err(error(piece)),
    };
    let offset = match &parts[1..] {
        [] => Offset::Imm(0),
        [imm] if imm.starts_with("#:") || imm.starts_with(':') => {
            let text = imm.strip_prefix('#').unwrap_or(imm);
            Offset::Symbol(reference(text, symbol)?)
        }
        [imm] if imm.starts_with('#') => {
            Offset::Imm(imm.strip_prefix('#').and_then(number).ok_or_else(|| error(piece))?)
        }
        [index, rest @ ..] => {
            let (width, reg) = match register(&index.to_ascii_lowercase()) {
                Some(Value::Gpr(width, number)) if number < 31 => (width, number),
                _ => return Err(error(piece)),
            };
            let (extend, amount) = match rest {
                [] => (Extend::Uxtx, None),
                [modifier_text] => match modifier(&modifier_text.to_ascii_lowercase())? {
                    Some(Value::Shift(Shift::Lsl, amount)) => (Extend::Uxtx, Some(amount)),
                    Some(Value::Extend(extend, amount)) => (extend, amount),
                    _ => return Err(error(piece)),
                },
                _ => return Err(error(piece)),
            };
            // The register has to be the width the extension reads, which for a shift left or
            // for nothing is the whole of it.
            let reads_w = matches!(extend, Extend::Uxtw | Extend::Sxtw);
            if reads_w != (width == Width::W) {
                return Err(error(piece));
            }
            Offset::Reg { reg, extend, amount }
        }
    };
    if mode == Mode::Pre && !matches!(offset, Offset::Imm(_)) {
        return Err(error(piece));
    }
    Ok(Addr { base, offset, mode })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_one_operand_however_it_is_written() {
        let line = read("ldr x0, [x1], #16").unwrap();
        assert_eq!(
            line.values,
            [
                Value::Gpr(Width::X, 0),
                Value::Mem(Addr { base: 1, offset: Offset::Imm(16), mode: Mode::Post })
            ]
        );
        let line = read("str x30, [sp, #-16]!").unwrap();
        assert_eq!(
            line.values[1],
            Value::Mem(Addr { base: 31, offset: Offset::Imm(-16), mode: Mode::Pre })
        );
    }

    #[test]
    fn a_symbol_keeps_its_name() {
        let line = read("ldr x0, [x0, :got_lo12:environ]").unwrap();
        assert_eq!(line.symbol.as_deref(), Some("environ"));
        let line = read("bl printf").unwrap();
        assert_eq!(line.values, [Value::Symbol(Operator::Plain)]);
        assert_eq!(line.symbol.as_deref(), Some("printf"));
    }

    #[test]
    fn what_is_not_a_register_is_refused() {
        assert!(read("ldr x0, [xzr]").is_err());
        assert!(read("ldr x0, [x1, w2]").is_err());
        assert!(read("add x0, x1, #zz").is_err());
        assert_eq!(system("s3_3_c13_c0_2"), system("tpidr_el0"));
    }
}
