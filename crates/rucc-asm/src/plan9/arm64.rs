//! An arm64 listing for a Plan 9 target, as the records of a `.7` file.
//!
//! Design: tamnd/rucc#4156, and 07.3 and 07.4 of the rucc-plan9 plan.
//!
//! As for amd64, each instruction of the listing becomes one `Prog`, or a small number of them,
//! and the linker `7l` selects the encoding. These rules of `7l` are different from the rules of
//! the GNU assembler, and the translation obeys them:
//!
//! - The linker makes the frame from the size in `TEXT`. It saves the link register at `0(SP)`
//!   when the function makes a call, and it releases the frame at each `RETURN`. The `stp` and the
//!   `mov x29, sp` of the prologue and the `ldp` of the epilogue are removed. The frame is 16
//!   bytes larger than the area that the body uses, as it is in the listing, so each offset from
//!   `sp` stays the same. The word at `0(SP)` is the shadow word of the kencc convention, which the
//!   body does not use.
//! - The frame pointer is set only in a function whose body reads `x29`. No other code reads it.
//! - `7l` writes `R17` when it expands an instruction, for example a load from a far offset or the
//!   address of a symbol. The listing uses `x17` as a scratch register, so `x17` becomes `R25`,
//!   which neither this compiler nor `7c` gives to a variable in a program. An address with an
//!   offset that `7l` cannot encode goes in `R24`, for the same reason. `R26` and `R27` hold the
//!   `extern register` variables, for example `up` and `m` of the kernel, and the listing writes
//!   them as it writes any other register. `R28` holds the static base and is never written.
//! - An address is `adrp` and then an `add` or a load with `:lo12:`. The `adrp` is removed and the
//!   instruction that uses the page becomes `MOV $sym(SB), R` or a load from `sym(SB)`, which the
//!   linker makes from the static base.
//! - A load of a signed byte or halfword into a `w` register sign extends to 64 bits in `7l`, so a
//!   `MOVWU` after it clears the upper half, as the `w` form does.
//! - A `q` register is spilled whole in the listing. Plan 9 code has no vector values, so the spill
//!   moves the low 64 bits, which hold the double.

use rucc_base::hash::Map;
use rucc_object::plan9::{Addr7, History, Machine, NSNAME, Object, Prog7, Progs, arm64 as a7};

use super::{Datum, Expr, Item, Line, Scope, expr, number, parse, trouble, units};
use crate::Trouble;

/// The records of a `.7` file for an arm64 listing.
///
/// `file` is the name of the source, which goes in the history, and `directory` is the directory
/// of the compilation, which is written in front of a relative name.
///
/// # Errors
///
/// A line that has no equivalent in a Plan 9 object, such as a section that Plan 9 does not have
/// or an instruction that the linker cannot encode.
pub fn arm64(listing: &str, file: &str, directory: Option<&str>) -> Result<Object, Trouble> {
    let lines = parse(listing, "//")?;
    let scope = Scope::new(&lines);
    let mut out = Out {
        scope: &scope,
        progs: Vec::new(),
        pc: 0,
        line: 0,
        labels: Map::default(),
        fixups: Vec::new(),
    };
    let (functions, data) = units(&lines)?;
    for (name, body) in &functions {
        out.function(name, body)?;
    }
    out.resolve()?;
    for datum in &data {
        out.datum(datum)?;
    }
    for (name, size) in &scope.commons {
        let mut glob = Prog7::new(a7::AGLOBL, 0, out.symbol(a7::D_OREG, name, 0), con(*size));
        glob.reg = 0;
        out.progs.push(glob);
    }
    out.progs.push(Prog7::new(a7::AEND, out.line, Addr7::default(), Addr7::default()));
    let mut object = Object::new(Progs::Arm64(out.progs));
    object.directory = directory.map(str::to_owned);
    object.history.push(History::push(file, 1));
    Ok(object)
}

/// The scratch register of the listing that `7l` also writes.
const X17: u8 = 17;

/// The register that holds the value of `x17` of the listing.
const SCRATCH: u8 = 25;

/// The register that holds an address with an offset that `7l` cannot encode.
const UNALIGNED: u8 = 24;

/// The stack pointer, and the zero register, in the operands of `7l`.
const RSP: u8 = 31;

/// The frame pointer.
const FP: u8 = 29;

/// The link register.
const LR: u8 = 30;

/// One operand of the listing.
#[derive(Debug, Clone, PartialEq)]
enum Op {
    /// A 64 bit register. 31 is `xzr`.
    X(u8),
    /// A 32 bit register. 31 is `wzr`.
    W(u8),
    /// `sp`.
    Sp,
    /// A float or vector register and the letter of its width: `d`, `s` or `q`.
    F(u8, char),
    Imm(i64),
    Float(f64),
    /// `:lo12:sym`, the low 12 bits of an address.
    Lo12(Expr),
    /// A symbol, a label or a condition, which the instruction tells apart.
    Name(String),
    Mem(Mem),
    /// `lsl`, `lsr`, `asr` or `ror` and the amount, as the operand after a register.
    Shift(u8, i64),
    /// An extension, `uxtb` to `sxtx`, and the amount.
    Ext(u8, i64),
}

/// A memory operand in brackets.
#[derive(Debug, Clone, PartialEq)]
struct Mem {
    /// The base register, with 31 for `sp`.
    base: u8,
    at: At,
    /// `[base, #n]!`.
    pre: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum At {
    Offset(i64),
    Lo12(Expr),
    /// An index register, its extension and whether it is scaled by the size of the access.
    Index {
        reg: u8,
        ext: u8,
        scaled: bool,
    },
}

/// The number of a general register in `7l`.
fn gpr(n: u8, line: usize) -> Result<u8, Trouble> {
    match n {
        X17 => Ok(SCRATCH),
        SCRATCH | UNALIGNED | 28 => Err(trouble(
            line,
            format!("the register x{n}, which the Plan 9 translation keeps for itself"),
        )),
        _ => Ok(n),
    }
}

fn extension(name: &str) -> Option<u8> {
    Some(match name {
        "uxtb" => 0,
        "uxth" => 1,
        "uxtw" => 2,
        "uxtx" => 3,
        "sxtb" => 4,
        "sxth" => 5,
        "sxtw" => 6,
        "sxtx" => 7,
        _ => return None,
    })
}

fn shift_kind(name: &str) -> Option<u8> {
    Some(match name {
        "lsl" => 0,
        "lsr" => 1,
        "asr" => 2,
        "ror" => 3,
        _ => return None,
    })
}

/// The condition code of `eq` to `al`.
fn condition(name: &str) -> Option<u8> {
    Some(match name {
        "eq" => 0,
        "ne" => 1,
        "hs" | "cs" => 2,
        "lo" | "cc" => 3,
        "mi" => 4,
        "pl" => 5,
        "vs" => 6,
        "vc" => 7,
        "hi" => 8,
        "ls" => 9,
        "ge" => 10,
        "lt" => 11,
        "gt" => 12,
        "le" => 13,
        "al" => 14,
        _ => return None,
    })
}

/// The register of a name such as `x3`, `w0`, `xzr` or `d1`.
fn register(name: &str) -> Option<Op> {
    match name {
        "sp" => return Some(Op::Sp),
        "xzr" => return Some(Op::X(31)),
        "wzr" => return Some(Op::W(31)),
        _ => {}
    }
    if name.is_empty() || !name.is_char_boundary(1) {
        return None;
    }
    let (head, rest) = name.split_at(1);
    let rest = match rest.split_once('.') {
        Some((n, _)) if head == "v" => n,
        _ => rest,
    };
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u8 = rest.parse().ok()?;
    match head {
        "x" if n <= 30 => Some(Op::X(n)),
        "w" if n <= 30 => Some(Op::W(n)),
        "d" | "s" | "q" if n <= 31 => Some(Op::F(n, head.chars().next()?)),
        "v" if n <= 31 => Some(Op::F(n, 'q')),
        _ => None,
    }
}

/// Whether operand `at` of `mnemonic` is always a symbol or a label. A C program can name a
/// function or an object as a register, as games/gba/ppu.c does with `sp` and cmd/map's hex.c
/// does with `w2`, and in these places the name is the symbol.
fn names_a_symbol(mnemonic: &str, at: usize) -> bool {
    match mnemonic {
        "adrp" | "cbz" | "cbnz" => at == 1,
        "b" | "bl" => at == 0,
        "tbz" | "tbnz" => at == 2,
        _ => mnemonic.starts_with("b.") && at == 0,
    }
}

/// Split operands at the commas that are not in brackets.
fn split(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (at, c) in text.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(text[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    let last = text[start..].trim();
    if !last.is_empty() || !parts.is_empty() {
        parts.push(last);
    }
    parts
}

fn immediate(text: &str) -> Option<i64> {
    number(text.strip_prefix('#')?)
}

fn operand(text: &str, line: usize) -> Result<Op, Trouble> {
    let bad = || trouble(line, format!("the operand {text} has no equivalent in a Plan 9 object"));
    if let Some(inner) = text.strip_prefix('[') {
        let (inner, pre) = match inner.strip_suffix("]!") {
            Some(inner) => (inner, true),
            None => (inner.strip_suffix(']').ok_or_else(bad)?, false),
        };
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        let base = match register(parts[0]) {
            Some(Op::X(n)) => gpr(n, line)?,
            Some(Op::Sp) => RSP,
            _ => return Err(bad()),
        };
        let at = match &parts[1..] {
            [] => At::Offset(0),
            [off] if off.starts_with('#') => At::Offset(immediate(off).ok_or_else(bad)?),
            [lo] if lo.starts_with(":lo12:") => At::Lo12(expr(&lo[6..], line)?),
            [index, rest @ ..] => {
                let (reg, wide) = match register(index) {
                    Some(Op::X(n)) => (gpr(n, line)?, true),
                    Some(Op::W(n)) => (gpr(n, line)?, false),
                    _ => return Err(bad()),
                };
                let (ext, scaled) = match rest {
                    [] if wide => (3, false),
                    [how] => {
                        let mut words = how.split_whitespace();
                        let name = words.next().unwrap_or("");
                        let amount = match words.next() {
                            Some(n) => immediate(n).ok_or_else(bad)?,
                            None => 0,
                        };
                        let ext = match name {
                            "lsl" if wide => 3,
                            _ => extension(name).ok_or_else(bad)?,
                        };
                        (ext, amount != 0)
                    }
                    _ => return Err(bad()),
                };
                At::Index { reg, ext, scaled }
            }
        };
        return Ok(Op::Mem(Mem { base, at, pre }));
    }
    if let Some(rest) = text.strip_prefix(":lo12:") {
        return Ok(Op::Lo12(expr(rest, line)?));
    }
    if let Some(value) = text.strip_prefix('#') {
        if let Some(n) = number(value) {
            return Ok(Op::Imm(n));
        }
        return value.parse().map(Op::Float).map_err(|_| bad());
    }
    if let Some(reg) = register(text) {
        return Ok(reg);
    }
    let mut words = text.split_whitespace();
    if let (Some(name), Some(amount), None) = (words.next(), words.next(), words.next()) {
        let amount = immediate(amount).ok_or_else(bad)?;
        if let Some(kind) = shift_kind(name) {
            return Ok(Op::Shift(kind, amount));
        }
        if let Some(ext) = extension(name) {
            return Ok(Op::Ext(ext, amount));
        }
        return Err(bad());
    }
    if let Some(ext) = extension(text) {
        return Ok(Op::Ext(ext, 0));
    }
    if text
        .chars()
        .all(|c| !c.is_ascii() || c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$'))
    {
        return Ok(Op::Name(text.to_owned()));
    }
    Err(bad())
}

fn con(value: i64) -> Addr7 {
    Addr7::constant(value)
}

fn reg(n: u8) -> Addr7 {
    Addr7::register(a7::D_REG, n)
}

/// The stack pointer as an operand. 7l reads register 31 as the zero register unless the type
/// is `D_SP`.
fn sp() -> Addr7 {
    Addr7::register(a7::D_SP, RSP)
}

fn freg(n: u8) -> Addr7 {
    Addr7::register(a7::D_FREG, n)
}

fn cond(code: u8) -> Addr7 {
    Addr7::register(a7::D_COND, code)
}

fn oreg(base: u8, offset: i64) -> Addr7 {
    Addr7 { ty: a7::D_OREG, reg: base, offset, ..Addr7::default() }
}

/// The frame of the function that is written.
#[derive(Debug, Clone, Copy, Default)]
struct Frame {
    /// The size of the area below the frame record, or `None` for a function with no frame.
    size: Option<i64>,
    /// No call, so `7l` does not save the link register.
    leaf: bool,
    /// The body reads `x29`, so it is set and restored.
    fp: bool,
}

/// The records as they are written.
struct Out<'s> {
    scope: &'s Scope,
    progs: Vec<Prog7>,
    /// The number of the next instruction, as the linker counts them.
    pc: i64,
    /// The line of the source of the next record.
    line: u32,
    /// Where each local label is, as a `pc`.
    labels: Map<String, i64>,
    /// The records with a branch to a label, and the label.
    fixups: Vec<(usize, String, usize)>,
}

impl Out<'_> {
    fn push(&mut self, p: Prog7) {
        let mut p = p;
        p.line = self.line;
        self.progs.push(p);
        self.pc += 1;
    }

    fn emit(&mut self, op: u16, from: Addr7, to: Addr7) {
        self.push(Prog7::new(op, self.line, from, to));
    }

    fn emit3(&mut self, op: u16, from: Addr7, middle: u8, to: Addr7) {
        let mut p = Prog7::new(op, self.line, from, to);
        p.reg = middle;
        self.push(p);
    }

    /// The opcode of a return, which `7l` makes into the release of the frame.
    fn ret() -> u16 {
        Machine::Arm64.opcode("ARETURN").unwrap_or_default()
    }

    fn op(name: &str, line: usize) -> Result<u16, Trouble> {
        Machine::Arm64
            .opcode(&format!("A{name}"))
            .ok_or_else(|| trouble(line, format!("7l has no instruction {name}")))
    }

    /// A symbol as an operand of the type `ty`, `D_OREG` for the memory and `D_CONST` for the
    /// address.
    fn symbol(&self, ty: u8, name: &str, offset: i64) -> Addr7 {
        let class = if self.scope.is_static(name) { a7::D_STATIC } else { a7::D_EXTERN };
        Addr7 {
            ty,
            name: class,
            sym: Some(self.scope.object_name(name)),
            offset,
            ..Addr7::default()
        }
    }

    fn expr_symbol(&self, ty: u8, e: &Expr) -> Addr7 {
        self.symbol(ty, e.sym.as_deref().unwrap_or_default(), e.offset)
    }

    fn function(&mut self, name: &str, body: &[&Line<'_>]) -> Result<(), Trouble> {
        let insns: Vec<(usize, &str, Vec<&str>, usize)> = body
            .iter()
            .enumerate()
            .filter_map(|(i, l)| match l.item {
                Item::Insn { prefix: None, mnemonic, operands } => {
                    Some((i, mnemonic, split(operands), l.number))
                }
                _ => None,
            })
            .collect();
        if let Some(l) = body.iter().find(|l| matches!(l.item, Item::Insn { prefix: Some(_), .. }))
        {
            return Err(trouble(l.number, "a prefix on an arm64 instruction"));
        }
        // The prologue is the frame record, the frame pointer and the area below it.
        let mut frame = Frame::default();
        let mut skip = 0;
        // A leaf can push other pairs first, and those are not a frame record.
        if let Some((_, "stp", ops, number)) = insns.first().filter(|i| i.2.first() == Some(&"x29"))
        {
            if ops[..] != ["x29", "x30", "[sp, #-16]!"] {
                return Err(trouble(*number, "a frame record that is not at the top of the frame"));
            }
            match insns.get(1) {
                Some((_, "mov", ops, _)) if ops[..] == ["x29", "sp"] => {}
                _ => return Err(trouble(*number, "a frame record without the frame pointer")),
            }
            skip = 2;
            let mut size = 0;
            while let Some((_, "sub", ops, number)) = insns.get(skip) {
                if ops.len() < 3 || ops[0] != "sp" || ops[1] != "sp" {
                    break;
                }
                let value = immediate(ops[2]).ok_or_else(|| trouble(*number, "a bad frame"))?;
                size += match ops.get(3) {
                    Some(&"lsl #12") => value << 12,
                    None => value,
                    Some(_) => return Err(trouble(*number, "a bad frame")),
                };
                skip += 1;
            }
            if size % 16 != 0 {
                return Err(trouble(insns[0].3, "a frame that is not a multiple of 16 bytes"));
            }
            frame.size = Some(size);
            let first = insns[0].0;
            let last = insns[skip - 1].0;
            if body[first..last].iter().any(|l| matches!(l.item, Item::Label(_))) {
                return Err(trouble(insns[0].3, "a label in the prologue"));
            }
        }
        frame.leaf = !insns.iter().any(|(_, m, _, _)| matches!(*m, "bl" | "blr"));
        let prologue: Vec<usize> = insns[..skip].iter().map(|i| i.0).collect();
        frame.fp = insns[skip..].iter().any(|(_, m, ops, _)| {
            !(matches!(*m, "ldp" | "mov") && is_epilogue(m, ops))
                && ops.iter().any(|o| o.contains("x29") || o.contains("w29"))
        });
        if frame.size.is_none() && !frame.leaf {
            return Err(trouble(
                body.first().map_or(0, |l| l.number),
                "a call in a function with no frame",
            ));
        }
        let mut text = Prog7::new(
            a7::ATEXT,
            self.line,
            self.symbol(a7::D_OREG, name, 0),
            con(frame.size.map_or(0, |n| n + 8)),
        );
        text.reg = 0;
        self.push(text);
        let size = frame.size.unwrap_or(0);
        if frame.fp {
            self.emit(Self::op("MOV", 0)?, reg(FP), oreg(RSP, size));
            self.above_sp(size, FP)?;
        }
        let mut released = false;
        for (at, l) in body.iter().enumerate() {
            match l.item {
                Item::Label(label) => {
                    if released {
                        return Err(trouble(l.number, "a label after the release of the frame"));
                    }
                    self.labels.insert(label.to_owned(), self.pc);
                }
                Item::Directive(".loc", args) => {
                    if let Some(n) = args.split_whitespace().nth(1).and_then(|s| s.parse().ok()) {
                        self.line = n;
                    }
                }
                Item::Directive(..) => {}
                Item::Insn { mnemonic, operands, .. } => {
                    if prologue.contains(&at) {
                        continue;
                    }
                    let ops = split(operands);
                    if frame.size.is_some() && is_epilogue(mnemonic, &ops) {
                        if mnemonic == "ldp" {
                            released = true;
                        }
                        continue;
                    }
                    let number = l.number;
                    let first = self.progs.len();
                    match (mnemonic, released) {
                        ("ret", _) => {
                            if frame.size.is_some() && !released {
                                return Err(trouble(
                                    number,
                                    "a return before the frame is released",
                                ));
                            }
                            if frame.fp {
                                self.emit(Self::op("MOV", 0)?, oreg(RSP, size), reg(FP));
                            }
                            self.emit(Self::ret(), Addr7::default(), Addr7::default());
                            released = false;
                        }
                        ("b", true) => {
                            let [target] = ops[..] else {
                                return Err(trouble(number, "a bad tail call"));
                            };
                            if frame.fp {
                                self.emit(Self::op("MOV", 0)?, oreg(RSP, size), reg(FP));
                            }
                            if !frame.leaf {
                                self.emit(Self::op("MOV", 0)?, oreg(RSP, 0), reg(LR));
                            }
                            let rsp = reg(RSP);
                            self.emit3(Self::op("ADD", 0)?, con(size + 16), RSP, rsp);
                            let to = self.symbol(a7::D_OREG, target, 0);
                            self.emit(Self::op("B", 0)?, Addr7::default(), to);
                            released = false;
                        }
                        (_, true) => {
                            return Err(trouble(
                                number,
                                "an instruction after the frame is released",
                            ));
                        }
                        _ => {
                            let ops: Vec<Op> = ops
                                .iter()
                                .enumerate()
                                .map(|(at, o)| match names_a_symbol(mnemonic, at) {
                                    true => Ok(Op::Name((*o).to_owned())),
                                    false => operand(o, number),
                                })
                                .collect::<Result<_, _>>()?;
                            self.insn(mnemonic, &ops, number, frame)?;
                        }
                    }
                    self.check(first, number)?;
                }
            }
        }
        // Each function ends with a return or a jump, as with `6l`, so that `7l` does not follow
        // the code into the next function.
        let b = Self::op("B", 0)?;
        if self.progs.last().is_none_or(|p| p.op != Self::ret() && p.op != b) {
            if frame.fp {
                self.emit(Self::op("MOV", 0)?, oreg(RSP, size), reg(FP));
            }
            self.emit(Self::ret(), Addr7::default(), Addr7::default());
        }
        Ok(())
    }

    /// Check each record from `first` against the table of `7l`, as the records of the listing
    /// line `line`.
    fn check(&self, first: usize, line: usize) -> Result<(), Trouble> {
        for p in &self.progs[first..] {
            if let Some((a1, a2, a3, why)) = super::oplook7::CHECKER.check(p) {
                let name = super::oplook7::Checker::name;
                return Err(trouble(
                    line,
                    format!(
                        "7l cannot encode {} with the operand classes {}, {} and {}: {why}",
                        a7::NAMES[usize::from(p.op)].strip_prefix('A').unwrap_or_default(),
                        name(a1),
                        name(a2),
                        name(a3),
                    ),
                ));
            }
        }
        Ok(())
    }

    fn branch(&mut self, op: u16, from: Addr7, middle: u8, label: &str, line: usize) {
        self.fixups.push((self.progs.len(), label.to_owned(), line));
        let mut p = Prog7::new(op, self.line, from, Addr7::of(a7::D_BRANCH));
        p.reg = middle;
        self.push(p);
    }

    /// One instruction that is not part of the prologue or the epilogue.
    #[allow(clippy::too_many_lines)]
    fn insn(&mut self, m: &str, ops: &[Op], line: usize, frame: Frame) -> Result<(), Trouble> {
        let bad = || trouble(line, format!("the instruction {m} {ops:?} has no equivalent in 7l"));
        // The `W` form of an opcode for a 32 bit destination.
        let sized = |base: &str, op: &Op| -> Result<u16, Trouble> {
            match op {
                Op::W(_) => Self::op(&format!("{base}W"), line),
                Op::X(_) | Op::Sp => Self::op(base, line),
                _ => Err(bad()),
            }
        };
        let r = |op: &Op| -> Result<u8, Trouble> {
            match op {
                Op::X(n) | Op::W(n) => gpr(*n, line),
                _ => Err(bad()),
            }
        };
        let f = |op: &Op| -> Result<(u8, char), Trouble> {
            match op {
                Op::F(n, w) => Ok((*n, *w)),
                _ => Err(bad()),
            }
        };
        match m {
            "nop" => {}
            "adrp" => {
                // The page goes with the `:lo12:` of the instruction that uses it.
                let [Op::X(_), Op::Name(_)] = ops else { return Err(bad()) };
            }
            "add" | "sub" | "adds" | "subs" | "and" | "ands" | "orr" | "eor" | "bic" | "orn"
            | "eon" | "bics" => {
                let base = match m {
                    "add" => "ADD",
                    "sub" => "SUB",
                    "adds" => "ADDS",
                    "subs" => "SUBS",
                    "and" => "AND",
                    "ands" => "ANDS",
                    "orr" => "ORR",
                    "eor" => "EOR",
                    "bic" => "BIC",
                    "bics" => "BICS",
                    "orn" => "ORN",
                    _ => "EON",
                };
                if let [Op::X(d), Op::X(_), Op::Lo12(e)] = ops {
                    if m != "add" {
                        return Err(bad());
                    }
                    let from = self.expr_symbol(a7::D_CONST, e);
                    self.emit(Self::op("MOV", line)?, from, reg(gpr(*d, line)?));
                    return Ok(());
                }
                let [d, n, rest @ ..] = ops else { return Err(bad()) };
                // A leaf with no frame record has no frame in 7l, so `RETURN` is a bare `RET` and
                // its own changes of sp are copied as they are.
                if matches!(d, Op::Sp) && frame.size.is_some() {
                    return Err(trouble(line, "a change of sp in the body of a function"));
                }
                if matches!(d, Op::Sp) && !matches!((m, n), ("add" | "sub", Op::Sp)) {
                    return Err(bad());
                }
                let rn = match n {
                    Op::Sp if matches!(rest.first(), Some(Op::Imm(_))) => RSP,
                    Op::Sp => return Err(bad()),
                    _ => r(n)?,
                };
                if let (Op::X(d), "add" | "sub", Op::Sp, [Op::Imm(0)]) = (d, m, n, rest) {
                    return self.above_sp(0, gpr(*d, line)?);
                }
                let from = self.second(rest, line)?;
                let rd = if matches!(d, Op::Sp) { RSP } else { r(d)? };
                self.emit3(sized(base, d)?, from, rn, reg(rd));
            }
            "mul" | "sdiv" | "udiv" | "smulh" | "umulh" | "lslv" | "lsrv" | "asrv" | "rorv"
            | "mneg" | "smull" | "umull" => {
                let base = match m {
                    "mul" => "MUL",
                    "sdiv" => "SDIV",
                    "udiv" => "UDIV",
                    "smulh" => "SMULH",
                    "umulh" => "UMULH",
                    "lslv" => "LSL",
                    "lsrv" => "LSR",
                    "asrv" => "ASR",
                    "rorv" => "ROR",
                    "mneg" => "MNEG",
                    "smull" => "SMULL",
                    _ => "UMULL",
                };
                let [d, n, mm] = ops else { return Err(bad()) };
                let op = if matches!(m, "smulh" | "umulh" | "smull" | "umull") {
                    Self::op(base, line)?
                } else {
                    sized(base, d)?
                };
                self.emit3(op, reg(r(mm)?), r(n)?, reg(r(d)?));
            }
            "lsl" | "lsr" | "asr" | "ror" => {
                let base = m.to_ascii_uppercase();
                let [d, n, by] = ops else { return Err(bad()) };
                let from = match by {
                    Op::Imm(v) => con(*v),
                    _ => reg(r(by)?),
                };
                self.emit3(sized(&base, d)?, from, r(n)?, reg(r(d)?));
            }
            "madd" | "msub" => {
                let [d, n, mm, a] = ops else { return Err(bad()) };
                let mut p = Prog7::new(
                    sized(&m.to_ascii_uppercase(), d)?,
                    self.line,
                    reg(r(mm)?),
                    reg(r(d)?),
                );
                p.reg = r(a)?;
                p.from3 = reg(r(n)?);
                self.push(p);
            }
            "neg" | "negs" | "mvn" => {
                let base = match m {
                    "neg" => "NEG",
                    "negs" => "NEGS",
                    _ => "MVN",
                };
                let [d, rest @ ..] = ops else { return Err(bad()) };
                let from = self.second(rest, line)?;
                self.emit(sized(base, d)?, from, reg(r(d)?));
            }
            "cmp" | "cmn" | "tst" => {
                let base = m.to_ascii_uppercase();
                let [n, rest @ ..] = ops else { return Err(bad()) };
                let rn = match n {
                    Op::Sp => RSP,
                    _ => r(n)?,
                };
                let from = self.second(rest, line)?;
                // 7l has no form of `TST`, so it is an `ANDS` that writes the zero register.
                let (base, to) = match base.as_str() {
                    "TST" => ("ANDS".to_owned(), reg(RSP)),
                    _ => (base, Addr7::default()),
                };
                self.emit3(sized(&base, n)?, from, rn, to);
            }
            "mov" => match ops {
                [Op::F(d, _), Op::F(s, _)] => {
                    self.emit(Self::op("FMOVD", line)?, freg(*s), freg(*d));
                }
                [Op::X(d), Op::Sp] => self.above_sp(0, gpr(*d, line)?)?,
                [Op::Sp, _] => {
                    return Err(trouble(line, "a change of sp in the body of a function"));
                }
                [d @ (Op::X(_) | Op::W(_)), Op::Imm(v)] => {
                    self.emit(sized("MOV", d)?, con(*v), reg(r(d)?));
                }
                [d @ Op::X(_), Op::X(31)] | [d @ Op::W(_), Op::W(31)] => {
                    self.emit(sized("MOV", d)?, con(0), reg(r(d)?));
                }
                [d @ Op::X(_), s @ Op::X(_)] => {
                    self.emit(Self::op("MOV", line)?, reg(r(s)?), reg(r(d)?));
                }
                [d @ Op::W(_), s @ Op::W(_)] => {
                    self.emit(Self::op("MOVWU", line)?, reg(r(s)?), reg(r(d)?));
                }
                _ => return Err(bad()),
            },
            "movz" | "movn" | "movk" => {
                let base = m.to_ascii_uppercase();
                let (d, v, shift) = match ops {
                    [d, Op::Imm(v)] => (d, *v, None),
                    [d, Op::Imm(v), Op::Shift(0, s)] => (d, *v, Some(*s)),
                    _ => return Err(bad()),
                };
                let mut p = Prog7::new(sized(&base, d)?, self.line, con(v), reg(r(d)?));
                if let Some(s) = shift {
                    p.from3 = con(s);
                }
                self.push(p);
            }
            "sxtb" | "sxth" | "sxtw" | "uxtb" | "uxth" | "uxtw" => {
                let base = m.to_ascii_uppercase();
                let [d, s] = ops else { return Err(bad()) };
                let op =
                    if base.ends_with('W') { Self::op(&base, line)? } else { sized(&base, d)? };
                self.emit(op, reg(r(s)?), reg(r(d)?));
            }
            "ubfx" | "sbfx" | "ubfiz" | "sbfiz" | "bfi" | "bfxil" => {
                let [d, n, Op::Imm(a), Op::Imm(b)] = ops else { return Err(bad()) };
                let mut p =
                    Prog7::new(sized(&m.to_ascii_uppercase(), d)?, self.line, con(*a), reg(r(d)?));
                p.reg = r(n)?;
                p.from3 = con(*b);
                self.push(p);
            }
            "csel" | "csinc" | "csinv" | "csneg" => {
                let [d, n, mm, Op::Name(c)] = ops else { return Err(bad()) };
                let code = condition(c).ok_or_else(bad)?;
                let mut p = Prog7::new(
                    sized(&m.to_ascii_uppercase(), d)?,
                    self.line,
                    cond(code),
                    reg(r(d)?),
                );
                p.reg = r(mm)?;
                p.from3 = reg(r(n)?);
                self.push(p);
            }
            "cset" | "csetm" => {
                let [d, Op::Name(c)] = ops else { return Err(bad()) };
                let code = condition(c).ok_or_else(bad)?;
                self.emit(sized(&m.to_ascii_uppercase(), d)?, cond(code), reg(r(d)?));
            }
            "clz" | "rbit" | "rev" | "rev16" | "cls" => {
                let [d, s] = ops else { return Err(bad()) };
                let base = match m {
                    "rbit" => "RBIT",
                    "rev" => "REV",
                    "rev16" => "REV16",
                    "cls" => "CLS",
                    _ => "CLZ",
                };
                self.emit(sized(base, d)?, reg(r(s)?), reg(r(d)?));
            }
            "b" => match ops {
                [Op::Name(label)] if label.starts_with(".L") => {
                    self.branch(Self::op("B", line)?, Addr7::default(), a7::NREG, label, line);
                }
                [Op::Name(target)] if frame.size.is_none() => {
                    let to = self.symbol(a7::D_OREG, target, 0);
                    self.emit(Self::op("B", line)?, Addr7::default(), to);
                }
                _ => return Err(trouble(line, "a jump out of a function that keeps its frame")),
            },
            "br" => {
                let [n] = ops else { return Err(bad()) };
                // 7l sends each branch on through a chain of `B`s, and a `B` through a register
                // has no target that it knows. Thus a branch to it gets no target. When a label
                // is at it, a `NOP` goes first, which 7l takes out after it finds the targets.
                if self.labels.values().any(|&pc| pc == self.pc) {
                    self.emit(Self::op("NOP", line)?, Addr7::default(), Addr7::default());
                }
                self.emit(Self::op("B", line)?, Addr7::default(), oreg(r(n)?, 0));
            }
            "bl" => {
                let [Op::Name(target)] = ops else { return Err(bad()) };
                let to = self.symbol(a7::D_OREG, target, 0);
                self.emit(Self::op("BL", line)?, Addr7::default(), to);
            }
            "blr" => {
                let [n] = ops else { return Err(bad()) };
                self.emit(Self::op("BL", line)?, Addr7::default(), oreg(r(n)?, 0));
            }
            "cbz" | "cbnz" => {
                let [t, Op::Name(label)] = ops else { return Err(bad()) };
                let op = sized(&m.to_ascii_uppercase(), t)?;
                self.branch(op, reg(r(t)?), a7::NREG, label, line);
            }
            "tbz" | "tbnz" => {
                let [t, Op::Imm(bit), Op::Name(label)] = ops else { return Err(bad()) };
                let op = Self::op(&m.to_ascii_uppercase(), line)?;
                self.branch(op, con(*bit), r(t)?, label, line);
            }
            _ if m.starts_with("b.") => {
                let base = match &m[2..] {
                    "eq" => "BEQ",
                    "ne" => "BNE",
                    "hs" | "cs" => "BHS",
                    "lo" | "cc" => "BLO",
                    "mi" => "BMI",
                    "pl" => "BPL",
                    "vs" => "BVS",
                    "vc" => "BVC",
                    "hi" => "BHI",
                    "ls" => "BLS",
                    "ge" => "BGE",
                    "lt" => "BLT",
                    "gt" => "BGT",
                    "le" => "BLE",
                    _ => return Err(bad()),
                };
                let [Op::Name(label)] = ops else { return Err(bad()) };
                self.branch(Self::op(base, line)?, Addr7::default(), a7::NREG, label, line);
            }
            "ldr" | "ldur" | "ldrb" | "ldurb" | "ldrh" | "ldurh" | "ldrsb" | "ldursb" | "ldrsh"
            | "ldursh" | "ldrsw" | "ldursw" => {
                let [t, mem, post @ ..] = ops else { return Err(bad()) };
                let kind = m.replace("ldur", "ldr");
                let (name, fix) = match (kind.as_str(), t) {
                    ("ldr", Op::X(_)) => ("MOV", false),
                    ("ldr", Op::W(_)) => ("MOVWU", false),
                    ("ldr", Op::F(_, 'd' | 'q')) => ("FMOVD", false),
                    ("ldr", Op::F(_, 's')) => ("FMOVS", false),
                    ("ldrb", Op::W(_)) => ("MOVBU", false),
                    ("ldrh", Op::W(_)) => ("MOVHU", false),
                    ("ldrsb", Op::X(_)) => ("MOVB", false),
                    ("ldrsb", Op::W(_)) => ("MOVB", true),
                    ("ldrsh", Op::X(_)) => ("MOVH", false),
                    ("ldrsh", Op::W(_)) => ("MOVH", true),
                    ("ldrsw", Op::X(_)) => ("MOVW", false),
                    _ => return Err(bad()),
                };
                let from = self.memory(mem, post, width(name), line)?;
                let to = match t {
                    Op::F(n, _) => freg(*n),
                    _ => reg(r(t)?),
                };
                self.emit(Self::op(name, line)?, from, to.clone());
                if fix {
                    self.emit(Self::op("MOVWU", line)?, to.clone(), to);
                }
            }
            "str" | "stur" | "strb" | "sturb" | "strh" | "sturh" => {
                let [t, mem, post @ ..] = ops else { return Err(bad()) };
                let kind = m.replace("stur", "str");
                let name = match (kind.as_str(), t) {
                    ("str", Op::X(_)) => "MOV",
                    ("str", Op::W(_)) => "MOVW",
                    ("str", Op::F(_, 'd' | 'q')) => "FMOVD",
                    ("str", Op::F(_, 's')) => "FMOVS",
                    ("strb", Op::W(_)) => "MOVB",
                    ("strh", Op::W(_)) => "MOVH",
                    _ => return Err(bad()),
                };
                let to = self.memory(mem, post, width(name), line)?;
                let from = match t {
                    Op::F(n, _) => freg(*n),
                    _ => reg(r(t)?),
                };
                self.emit(Self::op(name, line)?, from, to);
            }
            "ldp" | "stp" | "ldpsw" => {
                let [a, b, mem, post @ ..] = ops else { return Err(bad()) };
                let name = match (m, a) {
                    ("ldpsw", Op::X(_)) => "MOVPSW",
                    (_, Op::X(_)) => "MOVP",
                    (_, Op::W(_)) => "MOVPW",
                    (_, Op::F(_, 'd')) => "MOVPD",
                    (_, Op::F(_, 's')) => "MOVPS",
                    _ => return Err(bad()),
                };
                let one = |op: &Op| -> Result<(Addr7, u8), Trouble> {
                    match op {
                        Op::F(n, _) => Ok((freg(*n), *n)),
                        _ => Ok((reg(r(op)?), r(op)?)),
                    }
                };
                let (first, first_reg) = one(a)?;
                let (second, second_reg) = one(b)?;
                let at = self.memory(mem, post, width(name), line)?;
                let op = Self::op(name, line)?;
                if m == "stp" {
                    self.emit3(op, first, second_reg, at);
                } else {
                    self.emit3(op, at, first_reg, second);
                }
            }
            "fmov" => {
                let (op, from, to) = match ops {
                    [Op::F(d, w), Op::F(s, _)] => (fname("FMOV", *w), freg(*s), freg(*d)),
                    [Op::F(d, w), s @ (Op::X(_) | Op::W(_))] => {
                        (fname("FMOV", *w), reg(r(s)?), freg(*d))
                    }
                    [d @ (Op::X(_) | Op::W(_)), Op::F(s, w)] => {
                        (fname("FMOV", *w), freg(*s), reg(r(d)?))
                    }
                    [Op::F(d, w), Op::Float(v)] => {
                        let mut c = Addr7::of(a7::D_FCONST);
                        c.float = *v;
                        (fname("FMOV", *w), c, freg(*d))
                    }
                    _ => return Err(bad()),
                };
                self.emit(Self::op(&op, line)?, from, to);
            }
            "fadd" | "fsub" | "fmul" | "fdiv" | "fnmul" | "fmax" | "fmin" => {
                let [d, n, mm] = ops else { return Err(bad()) };
                let (dn, w) = f(d)?;
                let op = fname(&m.to_ascii_uppercase(), w);
                self.emit3(Self::op(&op, line)?, freg(f(mm)?.0), f(n)?.0, freg(dn));
            }
            "fneg" | "fabs" | "fsqrt" => {
                let [d, n] = ops else { return Err(bad()) };
                let (dn, w) = f(d)?;
                let op = fname(&m.to_ascii_uppercase(), w);
                self.emit(Self::op(&op, line)?, freg(f(n)?.0), freg(dn));
            }
            "fcmp" | "fcmpe" => {
                let [n, second] = ops else { return Err(bad()) };
                let (nn, w) = f(n)?;
                let base = if m == "fcmp" { "FCMP" } else { "FCMPE" };
                let from = match second {
                    Op::Float(v) => {
                        let mut c = Addr7::of(a7::D_FCONST);
                        c.float = *v;
                        c
                    }
                    Op::Imm(0) => Addr7::of(a7::D_FCONST),
                    _ => freg(f(second)?.0),
                };
                self.emit3(Self::op(&fname(base, w), line)?, from, nn, Addr7::default());
            }
            "fcsel" => {
                let [d, n, mm, Op::Name(c)] = ops else { return Err(bad()) };
                let code = condition(c).ok_or_else(bad)?;
                let (dn, w) = f(d)?;
                let mut p = Prog7::new(
                    Self::op(&fname("FCSEL", w), line)?,
                    self.line,
                    cond(code),
                    freg(dn),
                );
                p.reg = f(mm)?.0;
                p.from3 = freg(f(n)?.0);
                self.push(p);
            }
            "fcvt" => {
                let [d, s] = ops else { return Err(bad()) };
                let name = match (f(d)?.1, f(s)?.1) {
                    ('d', 's') => "FCVTSD",
                    ('s', 'd') => "FCVTDS",
                    _ => return Err(bad()),
                };
                self.emit(Self::op(name, line)?, freg(f(s)?.0), freg(f(d)?.0));
            }
            "fcvtzs" | "fcvtzu" => {
                let [d, s] = ops else { return Err(bad()) };
                let (sn, w) = f(s)?;
                let base = if m == "fcvtzs" { "FCVTZS" } else { "FCVTZU" };
                let name = format!(
                    "{base}{}{}",
                    fname("", w),
                    if matches!(d, Op::W(_)) { "W" } else { "" }
                );
                self.emit(Self::op(&name, line)?, freg(sn), reg(r(d)?));
            }
            "scvtf" | "ucvtf" => {
                let [d, s] = ops else { return Err(bad()) };
                let (dn, w) = f(d)?;
                let base = if m == "scvtf" { "SCVTF" } else { "UCVTF" };
                let name = format!(
                    "{base}{}{}",
                    if matches!(s, Op::W(_)) { "W" } else { "" },
                    fname("", w)
                );
                self.emit(Self::op(&name, line)?, reg(r(s)?), freg(dn));
            }
            _ => return Err(bad()),
        }
        Ok(())
    }

    /// The second operand of an operation: a register, a shifted or an extended register, or a
    /// constant.
    /// `rd = sp + offset`. 7l takes the constant 0 for the zero register, so `ADD $0, RSP, Rd`
    /// matches the register form of `ADD` and gives zero. The zero offset is a `MOV` from a
    /// `D_SP` operand, which 7l writes as `add rd, sp, #0`.
    fn above_sp(&mut self, offset: i64, rd: u8) -> Result<(), Trouble> {
        if offset == 0 {
            self.emit(Self::op("MOV", 0)?, sp(), reg(rd));
        } else {
            self.emit3(Self::op("ADD", 0)?, con(offset), RSP, reg(rd));
        }
        Ok(())
    }

    fn second(&self, ops: &[Op], line: usize) -> Result<Addr7, Trouble> {
        let bad = || trouble(line, format!("the operand {ops:?} has no equivalent in 7l"));
        Ok(match ops {
            [Op::Imm(v)] => con(*v),
            [Op::Imm(v), Op::Shift(0, 12)] => con(*v << 12),
            [Op::X(n) | Op::W(n)] => reg(gpr(*n, line)?),
            [Op::X(n) | Op::W(n), Op::Shift(kind, amount)] => {
                let n = gpr(*n, line)?;
                Addr7 {
                    ty: a7::D_SHIFT,
                    offset: (i64::from(n) << 16) | (amount << 10) | (i64::from(*kind) << 22),
                    ..Addr7::default()
                }
            }
            [Op::X(n) | Op::W(n), Op::Ext(ext, amount)] => {
                let n = gpr(*n, line)?;
                Addr7 {
                    ty: a7::D_EXTREG,
                    reg: n,
                    offset: (i64::from(n) << 16) | (i64::from(*ext) << 13) | (amount << 10),
                    ..Addr7::default()
                }
            }
            _ => return Err(bad()),
        })
    }

    /// A memory operand, with the offset after it for a post index, for an access of `size`
    /// bytes.
    ///
    /// `7l` scales a positive offset by the size of the access and has no form for an offset that
    /// is not a multiple of it, which the listing has for a copy that overlaps its last word. The
    /// address goes in `R27` first for such an offset.
    fn memory(&mut self, mem: &Op, post: &[Op], size: i64, line: usize) -> Result<Addr7, Trouble> {
        let bad = || trouble(line, format!("the memory operand {mem:?} has no equivalent in 7l"));
        let Op::Mem(Mem { base, at, pre }) = mem else { return Err(bad()) };
        if let (At::Offset(off), [], false) = (at, post, pre) {
            if *off > 0 && off % size != 0 {
                self.emit3(Self::op("ADD", line)?, con(*off), *base, reg(UNALIGNED));
                return Ok(oreg(UNALIGNED, 0));
            }
        }
        Ok(match (at, post) {
            (At::Offset(0), [Op::Imm(off)]) if !pre => {
                Addr7 { ty: a7::D_XPOST, reg: *base, offset: *off, ..Addr7::default() }
            }
            (At::Offset(off), []) => {
                let ty = if *pre { a7::D_XPRE } else { a7::D_OREG };
                Addr7 { ty, reg: *base, offset: *off, ..Addr7::default() }
            }
            (At::Lo12(e), []) if !pre => self.expr_symbol(a7::D_OREG, e),
            (At::Index { reg: index, ext, scaled }, []) if !pre => Addr7 {
                ty: a7::D_ROFF,
                reg: *base,
                offset: i64::from(*index) | (i64::from(*ext) << 8) | (i64::from(*scaled) << 16),
                ..Addr7::default()
            },
            _ => return Err(bad()),
        })
    }

    /// Give every branch to a label the `pc` of the label.
    fn resolve(&mut self) -> Result<(), Trouble> {
        for (at, label, line) in std::mem::take(&mut self.fixups) {
            let pc = *self.labels.get(&label).ok_or_else(|| {
                trouble(line, format!("a branch to {label}, which is not in the text"))
            })?;
            self.progs[at].to.offset = pc;
        }
        Ok(())
    }

    /// The `DATA` and the `GLOBL` of one symbol.
    fn datum(&mut self, datum: &Datum) -> Result<(), Trouble> {
        let mut covered = vec![false; datum.bytes.len()];
        for (at, width, e) in &datum.addresses {
            if self.labels.contains_key(e.sym.as_deref().unwrap_or_default()) {
                return Err(trouble(
                    datum.line,
                    format!("the address of a label of the text in {}", datum.name),
                ));
            }
            covered[*at..*at + usize::from(*width)].fill(true);
            let from = self.symbol(a7::D_OREG, &datum.name, *at as i64);
            let to = self.expr_symbol(a7::D_CONST, e);
            let mut p = Prog7::new(a7::ADATA, 0, from, to);
            p.reg = *width;
            self.progs.push(p);
        }
        let mut at = 0;
        while at < datum.bytes.len() {
            if covered[at] {
                at += 1;
                continue;
            }
            let end = (at..datum.bytes.len().min(at + NSNAME))
                .take_while(|&i| !covered[i])
                .last()
                .map_or(at, |i| i + 1);
            let chunk = &datum.bytes[at..end];
            if chunk.iter().any(|&b| b != 0) {
                let from = self.symbol(a7::D_OREG, &datum.name, at as i64);
                let mut to = Addr7::of(a7::D_SCONST);
                to.string[..chunk.len()].copy_from_slice(chunk);
                let mut p = Prog7::new(a7::ADATA, 0, from, to);
                p.reg = chunk.len() as u8;
                self.progs.push(p);
            }
            at = end;
        }
        let size = datum.size.unwrap_or(0).max(datum.bytes.len() as i64);
        let mut glob =
            Prog7::new(a7::AGLOBL, 0, self.symbol(a7::D_OREG, &datum.name, 0), con(size));
        glob.reg = 0;
        self.progs.push(glob);
        Ok(())
    }
}

/// The size of the access of a load or a store of `7l`, or of one register of a pair.
fn width(op: &str) -> i64 {
    match op {
        "MOVB" | "MOVBU" => 1,
        "MOVH" | "MOVHU" => 2,
        "MOVW" | "MOVWU" | "FMOVS" | "MOVPW" | "MOVPS" | "MOVPSW" => 4,
        _ => 8,
    }
}

/// The name of a float opcode for the width of its register.
fn fname(base: &str, width: char) -> String {
    format!("{base}{}", if width == 's' { 'S' } else { 'D' })
}

/// Whether the instruction releases the frame record: `mov sp, x29` or the `ldp` of the frame
/// pointer and the link register.
fn is_epilogue(m: &str, ops: &[&str]) -> bool {
    match m {
        "mov" => ops[..] == ["sp", "x29"],
        "ldp" => ops[..] == ["x29", "x30", "[sp]", "#16"],
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progs(listing: &str) -> Vec<Prog7> {
        match arm64(listing, "x.c", None).unwrap().progs {
            Progs::Arm64(progs) => progs,
            Progs::Amd64(_) => unreachable!(),
        }
    }

    fn ops(listing: &str) -> Vec<&'static str> {
        progs(listing).iter().map(|p| Machine::Arm64.opcode_name(p.op).unwrap_or("?")).collect()
    }

    #[test]
    fn the_frame_record_becomes_the_frame_of_text() {
        let listing = "\t.text\n\t.globl f\nf:\n\tstp x29, x30, [sp, #-16]!\n\tmov x29, sp\n\
                       \tsub sp, sp, #32\n\tstr x0, [sp, #8]\n\tbl g\n\tmov sp, x29\n\
                       \tldp x29, x30, [sp], #16\n\tret\n";
        let p = progs(listing);
        assert_eq!(ops(listing), ["ATEXT", "AMOV", "ABL", "ARETURN", "AEND"]);
        assert_eq!(p[0].to.offset, 40);
        assert_eq!(p[1].to.reg, RSP);
        assert_eq!(p[1].to.offset, 8);
        assert_eq!(p[2].to.sym.as_deref(), Some("g"));
    }

    #[test]
    fn a_leaf_with_no_frame_has_a_frame_of_zero() {
        let listing = "\t.text\n\t.globl leaf\nleaf:\n\tldr w1, [sp, #16]\n\tmul w0, w0, w1\n\
                       \tadd w0, w0, #3\n\tret\n";
        let p = progs(listing);
        assert_eq!(ops(listing), ["ATEXT", "AMOVWU", "AMULW", "AADDW", "ARETURN", "AEND"]);
        assert_eq!(p[0].to.offset, 0);
        assert_eq!((p[2].from.reg, p[2].reg, p[2].to.reg), (1, 0, 0));
        assert_eq!((p[3].from.offset, p[3].reg), (3, 0));
    }

    #[test]
    fn a_page_and_its_low_bits_become_one_symbol() {
        let listing = "\t.text\n\t.globl f\nf:\n\tadrp x0, s\n\tadd x0, x0, :lo12:s\n\
                       \tadrp x1, t\n\tldr w2, [x1, :lo12:t+8]\n\tret\n\t.bss\ns:\n\t.zero 8\n";
        let p = progs(listing);
        assert_eq!(ops(listing)[..4], ["ATEXT", "AMOV", "AMOVWU", "ARETURN"]);
        assert_eq!((p[1].from.ty, p[1].from.name), (a7::D_CONST, a7::D_STATIC));
        assert_eq!(p[1].from.sym.as_deref(), Some("s"));
        assert_eq!((p[2].from.ty, p[2].from.name, p[2].from.offset), (a7::D_OREG, a7::D_EXTERN, 8));
    }

    #[test]
    fn a_symbol_with_the_name_of_a_register_is_a_symbol() {
        let listing = "\t.text\n\t.globl f\nf:\n\tstp x29, x30, [sp, #-16]!\n\tmov x29, sp\n\
                       \tadrp x0, sp\n\tldr w0, [x0, :lo12:sp]\n\tbl w2\n\tmov sp, x29\n\
                       \tldp x29, x30, [sp], #16\n\tret\n";
        let p = progs(listing);
        assert_eq!(ops(listing), ["ATEXT", "AMOVWU", "ABL", "ARETURN", "AEND"]);
        assert_eq!(p[1].from.sym.as_deref(), Some("sp"));
        assert_eq!(p[2].to.sym.as_deref(), Some("w2"));
    }

    #[test]
    fn the_stack_pointer_with_no_offset_is_a_move_from_d_sp() {
        // 7l takes `ADD $0, RSP, R2` for the register form, where register 31 is the zero
        // register, so find1 in 7l's obj.c wrote through a null pointer.
        let listing = "\t.text\n\t.globl f\nf:\n\tadd x2, sp, #0\n\tmov x3, sp\n\
                       \tadd x4, sp, #8\n\tret\n";
        let p = progs(listing);
        assert_eq!(ops(listing), ["ATEXT", "AMOV", "AMOV", "AADD", "ARETURN", "AEND"]);
        for (q, to) in [(&p[1], 2), (&p[2], 3)] {
            assert_eq!((q.from.ty, q.from.reg, q.to.ty, q.to.reg), (a7::D_SP, RSP, a7::D_REG, to));
        }
        assert_eq!((p[3].from.offset, p[3].reg, p[3].to.reg), (8, RSP, 4));
    }

    #[test]
    fn x17_is_moved_out_of_the_way_of_the_linker() {
        let listing = "\t.text\n\t.globl f\nf:\n\tldr x17, [sp, #8]\n\tcmp w16, w17\n\tret\n";
        let p = progs(listing);
        assert_eq!(p[1].to.reg, SCRATCH);
        assert_eq!((p[2].from.reg, p[2].reg), (SCRATCH, 16));
    }

    /// The kernel keeps `up` in `R26` and `m` in `R27`, and its code reads and writes them.
    #[test]
    fn the_extern_register_variables_pass_through() {
        let listing = "\t.text\n\t.globl f\nf:\n\tmov x26, x0\n\tldr x1, [x27, #8]\n\tret\n";
        let p = progs(listing);
        assert_eq!((p[1].from.reg, p[1].to.reg), (0, 26));
        assert_eq!(p[2].from.reg, 27);
        assert_ne!(SCRATCH, 26);
        assert_ne!(UNALIGNED, 27);
    }

    #[test]
    fn a_select_and_a_set_keep_the_order_of_7l() {
        let listing = "\t.text\n\t.globl f\nf:\n\tcsel w0, w1, w2, ne\n\tcset w3, lt\n\tret\n";
        let p = progs(listing);
        assert_eq!((p[1].from.ty, p[1].from.reg), (a7::D_COND, 1));
        assert_eq!((p[1].reg, p[1].from3.reg, p[1].to.reg), (2, 1, 0));
        assert_eq!((p[2].from.reg, p[2].to.reg), (11, 3));
    }

    #[test]
    fn an_index_and_a_shift_are_encoded_as_7a_does() {
        let listing = "\t.text\n\t.globl f\nf:\n\tldr x0, [x16, w1, sxtw #3]\n\
                       \tadd x2, x3, x4, lsl #2\n\tldrb w5, [x6, x7]\n\tret\n";
        let p = progs(listing);
        assert_eq!(
            (p[1].from.ty, p[1].from.reg, p[1].from.offset),
            (a7::D_ROFF, 16, 1 | 6 << 8 | 1 << 16)
        );
        assert_eq!((p[2].from.ty, p[2].from.offset, p[2].reg), (a7::D_SHIFT, 4 << 16 | 2 << 10, 3));
        assert_eq!(p[3].from.offset, 7 | 3 << 8);
    }

    #[test]
    fn a_test_is_an_and_that_writes_the_zero_register() {
        let listing = "\t.text\n\t.globl f\nf:\n\ttst w1, #31\n\tret\n";
        let p = progs(listing);
        assert_eq!(ops(listing)[1], "AANDSW");
        assert_eq!((p[1].from.offset, p[1].reg, p[1].to.reg), (31, 1, RSP));
    }

    #[test]
    fn an_offset_that_is_not_a_multiple_of_the_size_goes_in_a_register() {
        let listing = "\t.text\n\t.globl f\nf:\n\tldr w3, [x0, #7]\n\tldrb w4, [x0, #7]\n\tret\n";
        let p = progs(listing);
        assert_eq!(ops(listing), ["ATEXT", "AADD", "AMOVWU", "AMOVBU", "ARETURN", "AEND"]);
        assert_eq!((p[1].from.offset, p[1].reg, p[1].to.reg), (7, 0, UNALIGNED));
        assert_eq!((p[2].from.reg, p[2].from.offset), (UNALIGNED, 0));
        assert_eq!((p[3].from.reg, p[3].from.offset), (0, 7));
    }

    #[test]
    fn a_leaf_with_no_frame_record_keeps_its_own_changes_of_sp() {
        let listing = "\t.text\n\t.globl f\nf:\n\tstp x19, x20, [sp, #-16]!\n\tsub sp, sp, #64\n\
                       \tstr w0, [sp, #4]\n\tadd sp, sp, #64\n\tldp x19, x20, [sp], #16\n\tret\n";
        let p = progs(listing);
        assert_eq!(
            ops(listing),
            ["ATEXT", "AMOVP", "ASUB", "AMOVW", "AADD", "AMOVP", "ARETURN", "AEND"]
        );
        assert_eq!(p[0].to.offset, 0);
        assert_eq!((p[2].from.offset, p[2].reg, p[2].to.reg), (64, RSP, RSP));
        assert_eq!((p[4].from.offset, p[4].reg, p[4].to.reg), (64, RSP, RSP));
        // A function with a frame record still refuses one.
        let framed = "f:\n\tstp x29, x30, [sp, #-16]!\n\tmov x29, sp\n\tbl g\n\tsub sp, sp, #16\n\
                      \tldp x29, x30, [sp], #16\n\tret\n";
        assert!(arm64(framed, "x.c", None).is_err());
    }

    #[test]
    fn a_label_with_letters_that_are_not_ascii_is_a_label() {
        let listing = "f:\n\tcbz w0, .L\u{3c4}conv_4\n\tmov w0, #1\n.L\u{3c4}conv_4:\n\tret\n";
        assert_eq!(ops(listing), ["ATEXT", "ACBZW", "AMOVW", "ARETURN", "AEND"]);
    }

    #[test]
    fn a_signed_byte_in_a_w_register_clears_the_upper_half() {
        let listing = "\t.text\n\t.globl f\nf:\n\tldrsb w0, [x1]\n\tret\n";
        assert_eq!(ops(listing), ["ATEXT", "AMOVB", "AMOVWU", "ARETURN", "AEND"]);
    }

    #[test]
    fn a_tail_call_releases_the_frame_and_jumps() {
        let listing = "\t.text\n\t.globl f\nf:\n\tstp x29, x30, [sp, #-16]!\n\tmov x29, sp\n\
                       \tsub sp, sp, #16\n\tbl g\n\tmov sp, x29\n\tldp x29, x30, [sp], #16\n\tb h\n";
        let p = progs(listing);
        assert_eq!(ops(listing), ["ATEXT", "ABL", "AMOV", "AADD", "AB", "AEND"]);
        assert_eq!((p[2].from.reg, p[2].from.offset, p[2].to.reg), (RSP, 0, LR));
        assert_eq!((p[3].from.offset, p[3].reg, p[3].to.reg), (32, RSP, RSP));
        assert_eq!(p[4].to.sym.as_deref(), Some("h"));
    }

    #[test]
    fn a_branch_to_a_jump_through_a_register_goes_to_a_nop() {
        let listing = "\t.text\n\t.globl f\nf:\n\tcbz w0, .Lf_2\n\tret\n.Lf_2:\n\tbr x1\n";
        let p = progs(listing);
        assert_eq!(ops(listing), ["ATEXT", "ACBZW", "ARETURN", "ANOP", "AB", "AEND"]);
        assert_eq!((p[1].to.ty, p[1].to.offset), (a7::D_BRANCH, 3));
        let listing = "\t.text\n\t.globl f\nf:\n\tbr x1\n";
        assert_eq!(ops(listing), ["ATEXT", "AB", "AEND"]);
    }

    #[test]
    fn a_branch_goes_to_the_pc_of_its_label() {
        let listing = "\t.text\n\t.globl f\nf:\n\tcbz w0, .Lf_2\n\tmov w0, #1\n.Lf_2:\n\tret\n";
        let p = progs(listing);
        assert_eq!(ops(listing), ["ATEXT", "ACBZW", "AMOVW", "ARETURN", "AEND"]);
        assert_eq!((p[1].to.ty, p[1].to.offset), (a7::D_BRANCH, 3));
    }

    #[test]
    fn a_body_that_reads_the_frame_pointer_sets_it() {
        let listing = "\t.text\n\t.globl f\nf:\n\tstp x29, x30, [sp, #-16]!\n\tmov x29, sp\n\
                       \tsub sp, sp, #16\n\tmov x0, x29\n\tmov sp, x29\n\tldp x29, x30, [sp], #16\n\
                       \tret\n";
        assert_eq!(ops(listing), ["ATEXT", "AMOV", "AADD", "AMOV", "AMOV", "ARETURN", "AEND"]);
    }

    #[test]
    fn data_with_an_address_is_written_with_its_width() {
        let listing = "\t.data\n\t.globl p\np:\n\t.quad s+4\n\t.ascii \"hi\\000\"\n\t.size p, 11\n";
        let p = progs(listing);
        assert_eq!(ops(listing), ["ADATA", "ADATA", "AGLOBL", "AEND"]);
        assert_eq!((p[0].reg, p[0].to.ty, p[0].to.offset), (8, a7::D_CONST, 4));
        assert_eq!((p[1].reg, p[1].from.offset), (3, 8));
        assert_eq!(p[2].to.offset, 11);
    }
}
