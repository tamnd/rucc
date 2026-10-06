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
    /// What is added to the symbol's address, which is the `8` in `:lo12:table+8`.
    pub addend: i64,
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
    // `bne` is the older spelling of `b.ne`, from before the dot, and gcc still writes it in places.
    // Nothing else that starts with `b` has a condition after it, `bl` and `bic` among them.
    let mut mnemonic = mnemonic.to_ascii_lowercase();
    if mnemonic.len() == 3 && mnemonic.starts_with('b') && Cond::named(&mnemonic[1..]).is_some() {
        mnemonic.insert(1, '.');
    }
    // A landing pad says which branches may land on it with a name and not an operand, and is the
    // hint with that number.
    if mnemonic == "bti" {
        let target = match rest.to_ascii_lowercase().as_str() {
            "" => 32,
            "c" => 34,
            "j" => 36,
            "jc" => 38,
            _ => return Err(error(text)),
        };
        return Ok(Line {
            mnemonic: "hint".to_owned(),
            values: vec![Value::Imm(target)],
            symbol: None,
            addend: 0,
        });
    }
    let mut line = Line { mnemonic, values: Vec::new(), symbol: None, addend: 0 };
    let mut named = (None, 0);
    let pieces = split(rest);
    let mut at = 0;
    while at < pieces.len() {
        let piece = pieces[at];
        if piece.starts_with('[') {
            // A post-indexed address is written as two operands, the address and then the amount
            // it moves by, and it is one operand to the machine.
            let mut addr = address(piece, &mut named)?;
            if addr.mode == Mode::Offset && piece.ends_with(']') && at + 1 < pieces.len() {
                if let (Offset::Imm(0), Some(imm)) = (addr.offset, immediate(pieces[at + 1])) {
                    addr.offset = Offset::Imm(imm);
                    addr.mode = Mode::Post;
                    at += 1;
                }
            }
            line.values.push(Value::Mem(addr));
        } else if piece.starts_with('{') {
            line.values.push(list(piece)?);
        } else if at + 1 == pieces.len() && takes_label(&line.mnemonic) && !piece.starts_with('#') {
            // Where the instruction wants a label a bare word is a symbol, whatever else it could
            // spell. A C global called `le` or `eq` is written `adrp x0, le`, and reading that as
            // the condition would leave the line with no encoding. GNU as reads it the same way.
            line.values.push(reference(piece, &mut named).map(Value::Symbol)?);
        } else {
            line.values.push(operand(piece, &mut named)?);
        }
        at += 1;
    }
    (line.symbol, line.addend) = named;
    Ok(line)
}

/// The symbol a line names so far, and what is added to it.
type Named = (Option<String>, i64);

/// Whether the last operand of the instruction is a label, which is every branch to one, the two
/// address forming instructions and the literal loads.
fn takes_label(mnemonic: &str) -> bool {
    matches!(
        mnemonic,
        "b" | "bl" | "cbz" | "cbnz" | "tbz" | "tbnz" | "adr" | "adrp" | "ldr" | "ldrsw" | "prfm"
    ) || mnemonic.starts_with("b.")
}

/// A list of vector registers, `{v0.16b, v1.16b}` or `{v0.16b - v3.16b}`, which have to follow
/// each other and be of one arrangement. The one after `v31` is `v0`.
fn list(piece: &str) -> Result<Value, Error> {
    let inner = piece.strip_prefix('{').and_then(|inner| inner.strip_suffix('}'));
    let inner = inner.ok_or_else(|| error(piece))?.to_ascii_lowercase();
    let vector = |name: &str| match register(name.trim()) {
        Some(Value::Vector(arrangement, number)) => Ok((arrangement, number)),
        _ => Err(error(piece)),
    };
    let (arrangement, first, count) = match inner.split_once('-') {
        Some((from, to)) => {
            let ((arrangement, first), (other, last)) = (vector(from)?, vector(to)?);
            if other != arrangement {
                return Err(error(piece));
            }
            (arrangement, first, (last + 32 - first) % 32 + 1)
        }
        None => {
            let mut names = inner.split(',');
            let (arrangement, first) = vector(names.next().unwrap_or(""))?;
            let mut count = 1;
            for name in names {
                if vector(name)? != (arrangement, (first + count) % 32) {
                    return Err(error(piece));
                }
                count += 1;
            }
            (arrangement, first, count)
        }
    };
    if count > 4 {
        return Err(error(piece));
    }
    Ok(Value::List(arrangement, first, count))
}

/// The operands, split at the commas that are not inside an address.
fn split(text: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (at, c) in text.char_indices() {
        match c {
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
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

/// A whole number with the `#` in front of it or without.
///
/// GNU as takes either, and gcc leaves it off: `stp x29, x30, [sp, -16]!` and `mov w0, 0` are what
/// its listings say. Nothing but a number reads as one, since a name never starts with a digit.
fn immediate(piece: &str) -> Option<i64> {
    number(piece.strip_prefix('#').unwrap_or(piece))
}

/// One operand that is not an address.
fn operand(piece: &str, symbol: &mut Named) -> Result<Value, Error> {
    let lower = piece.to_ascii_lowercase();
    if let Some(value) = register(&lower) {
        return Ok(value);
    }
    // A number without the `#`, which is how gcc writes every immediate. A float has to start with
    // a digit or a sign to be one, since `inf` and `nan` are names a file may use.
    if lower.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        if let Some(value) = number(&lower) {
            return Ok(Value::Imm(value));
        }
        if let Ok(value) = lower.parse::<f64>() {
            return Ok(Value::Float(value));
        }
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
    if let Some(operation) = prefetch(&lower) {
        return Ok(Value::Prefetch(operation));
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
    if let Some((digits, lanes)) = name.strip_prefix('v').and_then(|rest| rest.split_once('.')) {
        let register = numbered(digits, 32)?;
        // One lane, `v0.s[1]`. GNU as also takes the count in front of the letter, `v0.4s[1]`,
        // and the count says nothing the letter does not.
        if let Some((lane, index)) = lanes.strip_suffix(']').and_then(|lanes| lanes.split_once('['))
        {
            let scalar = match lane.trim_start_matches(|c: char| c.is_ascii_digit()) {
                "b" => Scalar::B,
                "h" => Scalar::H,
                "s" => Scalar::S,
                "d" => Scalar::D,
                _ => return None,
            };
            let index = number(index.trim())?;
            return Some(Value::Element(scalar, register, u8::try_from(index).ok()?));
        }
        return Some(Value::Vector(Arrangement::named(lanes)?, register));
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

/// A symbol with or without an operator in front of it, saving its name and what is added to it.
///
/// A name is what GNU as takes as one, or a numbered local label with the `b` or `f` that says
/// which way to look for it. A number added or taken away after the name is the addend, which is
/// how a compiler reaches a field of a variable without a second instruction.
fn reference(text: &str, symbol: &mut Named) -> Result<Operator, Error> {
    let (operator, name) = match text.strip_prefix(':') {
        Some(rest) => {
            let (operator, name) = rest.split_once(':').ok_or_else(|| error(text))?;
            let operator = match operator.to_ascii_lowercase().as_str() {
                "lo12" => Operator::Lo12,
                "got" => Operator::Got,
                "got_lo12" => Operator::GotLo12,
                "gottprel" => Operator::GotTprel,
                "gottprel_lo12" => Operator::GotTprelLo12,
                "tprel_hi12" => Operator::TprelHi12,
                "tprel_lo12_nc" => Operator::TprelLo12Nc,
                "secrel_hi12" => Operator::SecrelHi12,
                "secrel_lo12" => Operator::SecrelLo12,
                _ => return Err(error(text)),
            };
            (operator, name)
        }
        None if text.contains('@') => return apple(text, symbol),
        None => (Operator::Plain, text),
    };
    named(text, name, symbol)?;
    Ok(operator)
}

/// A symbol with the part of its address Apple's assembler wants said after it, as in
/// `_counter@PAGEOFF`.
///
/// `@PAGE` is what a bare name in `adrp` means to GNU as, so it is the plain operator here. The two
/// thread-local slots fill the same fields the GNU spellings do, and that the slot holds a
/// descriptor's address rather than an offset from the thread pointer is the platform's business
/// and not the encoder's. An addend may be written before the suffix or after it.
fn apple(text: &str, symbol: &mut Named) -> Result<Operator, Error> {
    let (name, rest) = text.split_once('@').ok_or_else(|| error(text))?;
    let end = rest.find(['+', '-']).unwrap_or(rest.len());
    let operator = match rest[..end].to_ascii_uppercase().as_str() {
        "PAGE" => Operator::Plain,
        "PAGEOFF" => Operator::Lo12,
        "GOTPAGE" => Operator::Got,
        "GOTPAGEOFF" => Operator::GotLo12,
        "TLVPPAGE" => Operator::GotTprel,
        "TLVPPAGEOFF" => Operator::GotTprelLo12,
        _ => return Err(error(text)),
    };
    if end < rest.len() && name.get(1..).is_some_and(|tail| tail.contains(['+', '-'])) {
        return Err(error(text));
    }
    named(text, &format!("{name}{}", &rest[end..]), symbol)?;
    Ok(operator)
}

/// The name and the addend of a symbol with any operator already taken off it.
fn named(text: &str, name: &str, symbol: &mut Named) -> Result<(), Error> {
    let (name, addend) = match name.char_indices().skip(1).find(|&(_, c)| c == '+' || c == '-') {
        Some((at, sign)) => {
            let digits = name[at + 1..].trim();
            let magnitude = number(digits).filter(|_| !digits.starts_with('-'));
            let magnitude = magnitude.ok_or_else(|| error(text))?;
            (name[..at].trim(), if sign == '-' { magnitude.wrapping_neg() } else { magnitude })
        }
        None => (name, 0),
    };
    let symbolic =
        name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || "_.$".contains(c))
            && name.chars().all(|c| c.is_ascii_alphanumeric() || "_.$".contains(c));
    let numbered = name.len() > 1
        && name.ends_with(['b', 'f'])
        && name[..name.len() - 1].bytes().all(|b| b.is_ascii_digit());
    if !symbolic && !numbered {
        return Err(error(text));
    }
    *symbol = (Some(name.to_owned()), addend);
    Ok(())
}

/// The barrier options by name, as the number the encoding gives each.
pub(super) static BARRIERS: [(&str, u8); 12] = [
    ("oshld", 1),
    ("oshst", 2),
    ("osh", 3),
    ("nshld", 5),
    ("nshst", 6),
    ("nsh", 7),
    ("ishld", 9),
    ("ishst", 10),
    ("ish", 11),
    ("ld", 13),
    ("st", 14),
    ("sy", 15),
];

/// A barrier option, as the number the encoding gives it.
fn barrier(name: &str) -> Option<u8> {
    BARRIERS.iter().find(|(known, _)| *known == name).map(|&(_, option)| option)
}

/// What a `prfm` is asked to do, from its name, which is the kind of access, the level of cache
/// and whether to keep the line or stream through it, in that order: `pldl1keep` is a load into
/// the first level that is kept, and is zero.
pub(super) fn prefetch(name: &str) -> Option<u8> {
    let rest = name.strip_prefix('p')?;
    let (kind, rest) = rest.split_at_checked(2)?;
    let kind = ["ld", "li", "st"].iter().position(|&known| known == kind)?;
    let (level, policy) = rest.strip_prefix('l')?.split_at_checked(1)?;
    let level = ["1", "2", "3"].iter().position(|&known| known == level)?;
    let policy = ["keep", "strm"].iter().position(|&known| known == policy)?;
    Some((kind << 3 | level << 1 | policy) as u8)
}

/// The name of what a `prfm` is asked to do, or nothing for the numbers that have none.
pub(super) fn prefetch_name(operation: u8) -> Option<String> {
    let (kind, level, policy) = (operation >> 3, operation >> 1 & 3, operation & 1);
    let kind = ["ld", "li", "st"].get(usize::from(kind))?;
    let level = ["1", "2", "3"].get(usize::from(level))?;
    let policy = ["keep", "strm"][usize::from(policy)];
    Some(format!("p{kind}l{level}{policy}"))
}

/// The system registers a compiler reads by name, as the op0, op1, CRn, CRm and op2 fields.
pub(super) static SYSTEM: [(&str, [u16; 5]); 9] = [
    ("nzcv", [3, 3, 4, 2, 0]),
    ("fpcr", [3, 3, 4, 4, 0]),
    ("fpsr", [3, 3, 4, 4, 1]),
    ("tpidr_el0", [3, 3, 13, 0, 2]),
    ("tpidrro_el0", [3, 3, 13, 0, 3]),
    ("cntfrq_el0", [3, 3, 14, 0, 0]),
    ("cntvct_el0", [3, 3, 14, 0, 2]),
    ("dczid_el0", [3, 3, 0, 0, 7]),
    ("ctr_el0", [3, 3, 0, 0, 1]),
];

/// The fifteen bits `mrs` and `msr` carry for a system register with those five fields.
pub(super) const fn system_field([op0, op1, crn, crm, op2]: [u16; 5]) -> u16 {
    (op0 - 2) << 14 | op1 << 11 | crn << 7 | crm << 3 | op2
}

/// A system register, as the fifteen bits `mrs` and `msr` carry for it.
///
/// The ones in [`SYSTEM`], and any of them written the generic way, as `s3_3_c13_c0_2`.
fn system(name: &str) -> Option<u16> {
    if let Some(&(_, fields)) = SYSTEM.iter().find(|(known, _)| *known == name) {
        return Some(system_field(fields));
    }
    let mut parts = name.strip_prefix('s')?.split('_');
    let mut next = |prefix: &str, below: u16| {
        let part = parts.next()?;
        part.strip_prefix(prefix)?.parse::<u16>().ok().filter(|&n| n < below)
    };
    let fields = [next("", 4)?, next("", 8)?, next("c", 16)?, next("c", 16)?, next("", 8)?];
    if parts.next().is_some() || fields[0] < 2 {
        return None;
    }
    Some(system_field(fields))
}

/// An address, `[x0]`, `[x0, #8]`, `[x0, #8]!`, `[x0, x1, lsl #3]` or `[x0, :lo12:name]`.
fn address(piece: &str, symbol: &mut Named) -> Result<Addr, Error> {
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
        [imm] if imm.contains('@') => {
            Offset::Symbol(apple(imm.strip_prefix('#').unwrap_or(imm), symbol)?)
        }
        [imm] if imm.starts_with('#') || immediate(imm).is_some() => {
            Offset::Imm(immediate(imm).ok_or_else(|| error(piece))?)
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
    fn a_number_without_its_hash_is_the_same_number() {
        // How gcc writes them, in an address, after one, and as an operand of its own.
        for (plain, hashed) in [
            ("stp x29, x30, [sp, -16]!", "stp x29, x30, [sp, #-16]!"),
            ("ldp x29, x30, [sp], 16", "ldp x29, x30, [sp], #16"),
            ("ldr w1, [x0, 8]", "ldr w1, [x0, #8]"),
            ("mov w0, 0", "mov w0, #0"),
            ("cmp w0, 0x10", "cmp w0, #0x10"),
            ("fmov d0, 1.5e+0", "fmov d0, #1.5"),
            ("tbnz w0, 3, .L2", "tbnz w0, #3, .L2"),
            ("bne .L2", "b.ne .L2"),
        ] {
            assert_eq!(read(plain).unwrap(), read(hashed).unwrap(), "{plain}");
        }
        // A name that happens to spell a float is still a name.
        assert_eq!(read("adrp x0, inf").unwrap().symbol.as_deref(), Some("inf"));
    }

    #[test]
    fn a_symbol_keeps_its_name() {
        let line = read("ldr x0, [x0, :got_lo12:environ]").unwrap();
        assert_eq!(line.symbol.as_deref(), Some("environ"));
        let line = read("bl printf").unwrap();
        assert_eq!(line.values, [Value::Symbol(Operator::Plain)]);
        assert_eq!(line.symbol.as_deref(), Some("printf"));
        let line = read("add x0, x0, :lo12:table+24").unwrap();
        assert_eq!((line.symbol.as_deref(), line.addend), (Some("table"), 24));
        let line = read("adrp x1, names-8").unwrap();
        assert_eq!((line.symbol.as_deref(), line.addend), (Some("names"), -8));
        let line = read("cbnz w3, 1b").unwrap();
        assert_eq!((line.symbol.as_deref(), line.addend), (Some("1b"), 0));
        assert!(read("b 1x").is_err());
        assert!(read("b table+").is_err());
    }

    #[test]
    fn apple_says_the_part_of_the_address_after_the_name() {
        let line = read("adrp x1, _counter@PAGE").unwrap();
        assert_eq!(line.values[1], Value::Symbol(Operator::Plain));
        assert_eq!(line.symbol.as_deref(), Some("_counter"));
        let line = read("add x1, x1, _table+8@PAGEOFF").unwrap();
        assert_eq!((line.symbol.as_deref(), line.addend), (Some("_table"), 8));
        let line = read("add x1, x1, _table@PAGEOFF+8").unwrap();
        assert_eq!((line.symbol.as_deref(), line.addend), (Some("_table"), 8));
        let line = read("ldr x0, [x0, _environ@GOTPAGEOFF]").unwrap();
        let Value::Mem(addr) = line.values[1] else { panic!("{:?}", line.values) };
        assert_eq!(addr.offset, Offset::Symbol(Operator::GotLo12));
        let line = read("adrp x0, _n@TLVPPAGE").unwrap();
        assert_eq!(line.values[1], Value::Symbol(Operator::GotTprel));
        assert!(read("adrp x0, _n@SIDEWAYS").is_err());
        assert!(read("add x1, x1, _t+8@PAGEOFF+8").is_err());
    }

    /// A global may be called what a condition, a barrier or a register is called, and where the
    /// instruction wants a label it is still the global. c4 has one called `le`.
    #[test]
    fn a_label_is_a_symbol_whatever_it_spells() {
        for text in ["adrp x0, le", "b eq", "bl ish", "b.ne sy", "cbz w1, x2", "adr x3, sp"] {
            let line = read(text).unwrap();
            assert_eq!(line.values.last(), Some(&Value::Symbol(Operator::Plain)), "{text}");
            let name = text.rsplit(' ').next();
            assert_eq!(line.symbol.as_deref(), name, "{text}");
        }
        let line = read("csel x0, x1, x2, le").unwrap();
        assert_eq!(line.values[3], Value::Cond(Cond::Le));
    }

    #[test]
    fn what_is_not_a_register_is_refused() {
        assert!(read("ldr x0, [xzr]").is_err());
        assert!(read("ldr x0, [x1, w2]").is_err());
        assert!(read("add x0, x1, #zz").is_err());
        assert_eq!(system("s3_3_c13_c0_2"), system("tpidr_el0"));
    }
}
