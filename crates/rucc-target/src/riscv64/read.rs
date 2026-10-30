//! Reading one line of RISC-V assembly as the operands the encoder takes.
//!
//! The other half of [`crate::riscv64::write`]: every line that writes, this reads back to the
//! same mnemonic and values. The text is what GNU as reads. A register is its ABI name, its `x`
//! or `f` name, or `fp`. An address is a displacement and a base in parentheses, where the
//! displacement may be `%lo()` or `%tprel_lo()` around a name. A distance inside one opcode is
//! written from `.`, so `.+8` is eight bytes on from the start of the instruction.
//!
//! A name is a register before it is a symbol, the way GNU as reads them, so a symbol spelled
//! `a0` has to be written some other way.

use std::fmt;

use crate::riscv64::encode::{Addr, Offset, Operator, Round, Value};
use crate::riscv64::{FPR_NAMES, GPR_NAMES};

/// One instruction, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Its mnemonic, in lower case.
    pub mnemonic: String,
    /// Its operands, in the order they were written.
    pub values: Vec<Value>,
    /// The symbol it names, when it names one. [`Value::Symbol`] and [`Offset::Symbol`] say where
    /// and which part of its address, and this says which symbol.
    pub symbol: Option<String>,
    /// What is added to the symbol's address, which is the `8` in `%lo(table+8)`.
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
        write!(f, "cannot read `{}` as riscv64 assembly", self.text)
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
/// When an operand is none of the things an instruction here takes, or when the line names two
/// symbols. See [`Error`].
pub fn read(text: &str) -> Result<Line, Error> {
    let text = text.trim();
    let (mnemonic, rest) = match text.find(char::is_whitespace) {
        Some(at) => (&text[..at], text[at..].trim()),
        None => (text, ""),
    };
    if mnemonic.is_empty() {
        return Err(error(text));
    }
    let mnemonic = mnemonic.to_ascii_lowercase();
    let mut line = Line { mnemonic, values: Vec::new(), symbol: None, addend: 0 };
    let operands = split(rest).ok_or_else(|| error(text))?;
    let fence = line.mnemonic == "fence";
    let float = line.mnemonic.starts_with('f') && !fence;
    for (index, operand) in operands.iter().enumerate() {
        let last = index + 1 == operands.len();
        let value = if fence {
            Some(order(operand).map(Value::Order).ok_or_else(|| error(operand))?)
        } else if float && last && index > 0 {
            rounding(operand).map(Value::Round)
        } else {
            None
        };
        let value = match value {
            Some(value) => value,
            None => line.operand(operand).ok_or_else(|| error(operand))??,
        };
        line.values.push(value);
    }
    // `fence` on its own orders everything against everything, which is what GNU as reads it as.
    if fence && line.values.is_empty() {
        line.values = vec![Value::Order(15), Value::Order(15)];
    }
    // `jal` with one operand links through `ra`, the way `jalr` with one does.
    if line.mnemonic == "jal" && line.values.len() == 1 {
        line.values.insert(0, Value::Gpr(1));
    }
    Ok(line)
}

impl Line {
    /// One operand that is not a rounding mode or a fence's order, or `None` for text that is
    /// none of the things an operand can be.
    fn operand(&mut self, text: &str) -> Option<Result<Value, Error>> {
        if let Some(value) = register(text) {
            return Some(Ok(value));
        }
        // An address, which ends in a base register in parentheses.
        if let Some(open) = text.strip_suffix(')').and_then(|inner| inner.rfind('(')) {
            if let Some(Value::Gpr(base)) = register(text[open + 1..text.len() - 1].trim()) {
                let disp = text[..open].trim();
                let offset = if disp.is_empty() {
                    Offset::Imm(0)
                } else if let Some(imm) = number(disp) {
                    Offset::Imm(imm)
                } else {
                    match self.symbol(disp)? {
                        Ok(Value::Symbol(operator)) => Offset::Symbol(operator),
                        Ok(_) => return None,
                        Err(why) => return Some(Err(why)),
                    }
                };
                return Some(Ok(Value::Mem(Addr { base, offset })));
            }
        }
        if let Some(imm) = number(text) {
            return Some(Ok(Value::Imm(imm)));
        }
        // A distance from the start of this instruction.
        if let Some(distance) = text.strip_prefix('.') {
            let distance = distance.trim();
            if distance.is_empty() {
                return Some(Ok(Value::Imm(0)));
            }
            if distance.starts_with(['+', '-']) {
                return number(distance).map(|imm| Ok(Value::Imm(imm)));
            }
        }
        // A CSR by its name, which is a number to the instruction rather than a symbol.
        if self.mnemonic.starts_with("csr") {
            if let Some(number) = csr_named(text) {
                return Some(Ok(Value::Imm(number)));
            }
        }
        self.symbol(text)
    }

    /// A name, with the operator around it that picks part of its address and what is added to
    /// it, which is kept on the line.
    fn symbol(&mut self, text: &str) -> Option<Result<Value, Error>> {
        let (operator, inner) = match text.strip_prefix('%') {
            Some(rest) => {
                let open = rest.find('(')?;
                let inner = rest[open + 1..].strip_suffix(')')?;
                let operator = match rest[..open].trim() {
                    "hi" => Operator::Hi,
                    "lo" => Operator::Lo,
                    "tprel_hi" => Operator::TprelHi,
                    "tprel_lo" => Operator::TprelLo,
                    "tprel_add" => Operator::TprelAdd,
                    _ => return None,
                };
                (operator, inner.trim())
            }
            None => (Operator::Plain, text),
        };
        // Around a number the parts are worked out here, as gas does, which is how the kernel's
        // `runtime_const_ptr` writes the number it patches later: `lui a0, %hi(0x89ABCDEF)`.
        if let Some(n) = number(inner) {
            let low = ((n & 0xfff) ^ 0x800) - 0x800;
            match operator {
                Operator::Hi => return Some(Ok(Value::Imm(((n - low) >> 12) & 0xf_ffff))),
                Operator::Lo => return Some(Ok(Value::Imm(low))),
                _ => {}
            }
        }
        let (name, addend) = addend(inner)?;
        if self.symbol.as_deref().is_some_and(|held| held != name) {
            return Some(Err(error(text)));
        }
        self.symbol = Some(name.to_owned());
        self.addend = addend;
        Some(Ok(Value::Symbol(operator)))
    }
}

/// The number of a CSR GNU as knows by name, for the ones a kernel or a program reads: the float
/// status, the counters, and the supervisor and machine registers.
fn csr_named(name: &str) -> Option<i64> {
    Some(match name {
        "fflags" => 0x001,
        "frm" => 0x002,
        "fcsr" => 0x003,
        "sstatus" => 0x100,
        "sie" => 0x104,
        "stvec" => 0x105,
        "scounteren" => 0x106,
        "senvcfg" => 0x10a,
        "sscratch" => 0x140,
        "sepc" => 0x141,
        "scause" => 0x142,
        "stval" => 0x143,
        "sip" => 0x144,
        "stimecmp" => 0x14d,
        "satp" => 0x180,
        "mstatus" => 0x300,
        "misa" => 0x301,
        "medeleg" => 0x302,
        "mideleg" => 0x303,
        "mie" => 0x304,
        "mtvec" => 0x305,
        "mscratch" => 0x340,
        "mepc" => 0x341,
        "mcause" => 0x342,
        "mtval" => 0x343,
        "mip" => 0x344,
        "cycle" => 0xc00,
        "time" => 0xc01,
        "instret" => 0xc02,
        "mhartid" => 0xf14,
        _ => return None,
    })
}

/// The operands, split at the commas that are not inside parentheses, or `None` when the
/// parentheses do not match.
fn split(text: &str) -> Option<Vec<&str>> {
    let mut out = Vec::new();
    if text.is_empty() {
        return Some(out);
    }
    let (mut depth, mut start) = (0u32, 0);
    for (at, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                out.push(text[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    out.push(text[start..].trim());
    out.iter().all(|operand| !operand.is_empty()).then_some(out)
}

/// A register by any of its names, `a0`, `x10` or `fp`, as the operand the encoder takes.
#[must_use]
pub fn register(text: &str) -> Option<Value> {
    if text == "fp" {
        return Some(Value::Gpr(8));
    }
    if let Some(at) = GPR_NAMES.iter().position(|&name| name == text) {
        return u8::try_from(at).ok().map(Value::Gpr);
    }
    if let Some(at) = FPR_NAMES.iter().position(|&name| name == text) {
        return u8::try_from(at).ok().map(Value::Fpr);
    }
    let (file, digits) = text.split_at_checked(1)?;
    if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) {
        return None;
    }
    let number: u8 = digits.parse().ok().filter(|&number| number < 32)?;
    match file {
        "x" => Some(Value::Gpr(number)),
        "f" => Some(Value::Fpr(number)),
        _ => None,
    }
}

/// A number in decimal, or in hexadecimal or binary with the prefix C gives them, with a sign
/// in front if it has one.
fn number(text: &str) -> Option<i64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, text.strip_prefix('+').unwrap_or(text).trim_start()),
    };
    let lower = digits.to_ascii_lowercase();
    let magnitude = if let Some(hex) = lower.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()?
    } else if let Some(binary) = lower.strip_prefix("0b") {
        u64::from_str_radix(binary, 2).ok()?
    } else if lower.bytes().all(|byte| byte.is_ascii_digit()) && !lower.is_empty() {
        lower.parse().ok()?
    } else {
        return None;
    };
    // Through the unsigned value, so that a constant written as its sixty four bits comes back
    // as the same bits.
    let value = magnitude as i64;
    Some(if negative { value.wrapping_neg() } else { value })
}

/// A symbol's name and the number added to it, from `name`, `name+8` or `name-8`.
fn addend(text: &str) -> Option<(&str, i64)> {
    let text = text.trim();
    let split = text.char_indices().skip(1).find(|&(_, ch)| ch == '+' || ch == '-');
    let (name, addend) = match split {
        Some((at, _)) => (text[..at].trim(), number(text[at..].trim())?),
        None => (text, 0),
    };
    let named = name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._$".contains(&byte));
    ((!name.is_empty() && named && !name.as_bytes()[0].is_ascii_digit()) || numbered(name))
        .then_some((name, addend))
}

/// Whether the name is a numbered label's `1b` or `2f`.
fn numbered(name: &str) -> bool {
    name.strip_suffix(['b', 'f']).is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// A rounding mode by the name the text gives it.
fn rounding(text: &str) -> Option<Round> {
    [Round::Rne, Round::Rtz, Round::Rdn, Round::Rup, Round::Rmm, Round::Dyn]
        .into_iter()
        .find(|round| round.name() == text)
}

/// One side of a `fence`, as the four bits of `iorw`.
fn order(text: &str) -> Option<u8> {
    let mut bits = 0;
    for letter in text.chars() {
        let bit = match letter {
            'i' => 8,
            'o' => 4,
            'r' => 2,
            'w' => 1,
            _ => return None,
        };
        if bits & bit != 0 {
            return None;
        }
        bits |= bit;
    }
    (bits != 0).then_some(bits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::riscv64::write;

    use Value::{Fpr, Gpr, Imm, Mem, Order, Symbol};

    fn read_ok(text: &str) -> Line {
        read(text).unwrap_or_else(|why| panic!("{why}"))
    }

    #[test]
    fn registers_are_read_by_every_name() {
        let line = read_ok("add a0, sp, zero");
        assert_eq!(line.values, [Gpr(10), Gpr(2), Gpr(0)]);
        assert_eq!(read_ok("add x10, fp, x31").values, [Gpr(10), Gpr(8), Gpr(31)]);
        assert_eq!(read_ok("fadd.d fa0, ft0, f31").values, [Fpr(10), Fpr(0), Fpr(31)]);
        assert_eq!(read_ok("RET").mnemonic, "ret");
    }

    #[test]
    fn an_address_is_a_displacement_and_a_base() {
        let at = |base, disp| Mem(Addr { base, offset: Offset::Imm(disp) });
        assert_eq!(read_ok("ld a0, -24(s0)").values, [Gpr(10), at(8, -24)]);
        assert_eq!(read_ok("ld a0, (sp)").values, [Gpr(10), at(2, 0)]);
        assert_eq!(read_ok("sd ra, 0x10(sp)").values, [Gpr(1), at(2, 16)]);
        let line = read_ok("sw a1, %lo(n+4)(a0)");
        let low = Mem(Addr { base: 10, offset: Offset::Symbol(Operator::Lo) });
        assert_eq!(line.values, [Gpr(11), low]);
        assert_eq!((line.symbol.as_deref(), line.addend), (Some("n"), 4));
    }

    #[test]
    fn a_symbol_keeps_its_name_its_addend_and_its_operator() {
        let line = read_ok("lla a0, .LC0-8");
        assert_eq!(line.values, [Gpr(10), Symbol(Operator::Plain)]);
        assert_eq!((line.symbol.as_deref(), line.addend), (Some(".LC0"), -8));
        let line = read_ok("add a0, a0, tp, %tprel_add(t)");
        assert_eq!(line.values[3], Symbol(Operator::TprelAdd));
        assert_eq!(read_ok("jal f").values, [Gpr(1), Symbol(Operator::Plain)]);
        assert_eq!(read_ok("j 1b").symbol.as_deref(), Some("1b"));
        assert!(read("lui a0, %hi(x)\n").is_ok());
        assert_eq!(read_ok("lui a0, %hi(0x89ABCDEF)").values, [Gpr(10), Imm(0x89abd)]);
        assert_eq!(read_ok("addiw a0, a0, %lo(0x89ABCDEF)").values[2], Imm(-0x211));
        assert_eq!(read_ok("lui a0, %hi(0x7ff)").values[1], Imm(0));
        assert!(read("add a0, a1, %tprel_add(t), %lo(u)").is_err());
        assert!(read("addi a0, a0, %pcrel_lo(x)").is_err());
    }

    #[test]
    fn a_distance_is_counted_from_the_dot() {
        assert_eq!(read_ok("bnez a3, .+8").values, [Gpr(13), Imm(8)]);
        assert_eq!(read_ok("j .-12").values, [Imm(-12)]);
        assert_eq!(read_ok("j .").values, [Imm(0)]);
    }

    #[test]
    fn a_csr_is_read_by_its_name_or_its_number() {
        assert_eq!(read_ok("csrr a0, sstatus").values, [Gpr(10), Imm(0x100)]);
        assert_eq!(read_ok("csrw 0x105, a0").values, [Imm(0x105), Gpr(10)]);
        assert_eq!(read_ok("csrrs a0, time, zero").values, [Gpr(10), Imm(0xc01), Gpr(0)]);
        // Only a CSR instruction reads one, so anywhere else the name is a symbol.
        assert_eq!(read_ok("call time").symbol.as_deref(), Some("time"));
        let held = Mem(Addr { base: 10, offset: Offset::Imm(0) });
        assert_eq!(read_ok("lr.w.aq a5, (a0)").values, [Gpr(15), held]);
    }

    #[test]
    fn rounding_and_ordering_are_read_by_name() {
        let line = read_ok("fcvt.w.d a0, fa1, rtz");
        assert_eq!(line.values, [Gpr(10), Fpr(11), Value::Round(Round::Rtz)]);
        assert_eq!(read_ok("fence rw, rw").values, [Order(3), Order(3)]);
        assert_eq!(read_ok("fence").values, [Order(15), Order(15)]);
        assert!(read("fence rr, w").is_err());
    }

    /// Every line the printer writes reads back as what it was written from, and the encoder
    /// takes it.
    #[test]
    fn what_write_writes_reads_back_the_same() {
        let at = |base, disp| Mem(Addr { base, offset: Offset::Imm(disp) });
        let low = |base| Mem(Addr { base, offset: Offset::Symbol(Operator::Lo) });
        let lines: &[(&str, &[Value])] = &[
            ("add", &[Gpr(10), Gpr(11), Gpr(12)]),
            ("addiw", &[Gpr(5), Gpr(6), Imm(-2048)]),
            ("slli", &[Gpr(10), Gpr(10), Imm(63)]),
            ("ld", &[Gpr(1), at(2, 2040)]),
            ("fsd", &[Fpr(8), at(8, -8)]),
            ("lw", &[Gpr(10), low(10)]),
            ("sb", &[Gpr(0), low(11)]),
            ("beq", &[Gpr(10), Gpr(11), Imm(-4096)]),
            ("bnez", &[Gpr(10), Symbol(Operator::Plain)]),
            ("lui", &[Gpr(10), Symbol(Operator::Hi)]),
            ("lui", &[Gpr(10), Imm(0xfffff)]),
            ("jal", &[Gpr(1), Imm(16)]),
            ("jalr", &[Gpr(0), at(1, 0)]),
            ("call", &[Symbol(Operator::Plain)]),
            ("la.tls.ie", &[Gpr(10), Symbol(Operator::Plain)]),
            ("li", &[Gpr(10), Imm(i64::MIN)]),
            ("fmadd.d", &[Fpr(0), Fpr(1), Fpr(2), Fpr(3)]),
            ("fcvt.l.d", &[Gpr(10), Fpr(10), Value::Round(Round::Rtz)]),
            ("fmv.x.d", &[Gpr(10), Fpr(10)]),
            ("fence", &[Order(2), Order(3)]),
            ("ecall", &[]),
        ];
        for &(mnemonic, values) in lines {
            let text = write(mnemonic, values, Some("name"));
            let line = read_ok(&text);
            assert_eq!(line.mnemonic, mnemonic, "{text}");
            assert_eq!(line.values, values, "{text}");
            crate::riscv64::encode(&line.mnemonic, &line.values)
                .unwrap_or_else(|why| panic!("{text}: {why}"));
        }
    }
}
