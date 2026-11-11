//! A listing for a Plan 9 target, as the records of a Plan 9 object.
//!
//! Design: tamnd/rucc#4156, and 07.3 and 07.4 of the rucc-plan9 plan.
//!
//! The listing that this compiler writes for a Plan 9 target uses the conventions of kencc, so the
//! work here is a change of spelling and not a change of code. Each instruction becomes one `Prog`,
//! or a small number of them, with the same operands. The linker `6l` then selects the encoding,
//! lays out the text and resolves every symbol, as it does for the output of `6c`.
//!
//! This module has the parts that the two machines share and the translation for amd64. The
//! module `arm64` has the translation for `7l`.
//!
//! These rules of `6l` are different from the rules of the GNU assembler, and the translation
//! obeys them:
//!
//! - `CMP` has its operands in the order of the comparison, which is the reverse of AT&T.
//! - `MOVL $0` and `MOVQ $0` to a register become an `XOR`, which changes the flags. Where the
//!   flags are live after the move, the move becomes `LEAL 0` or `LEAQ 0`.
//! - A `CALL` with a symbol is a direct call. A call through a pointer in memory at a symbol loads
//!   the pointer into `AX` first, which the call overwrites in any case.
//! - A `JMP` with a symbol is a jump through memory. A tail call is `RET $0, name(SB)`, which the
//!   linker changes into the release of the frame and a direct jump.
//! - The linker allocates and releases the frame from the size in `TEXT`. The `subq` at the start
//!   of a function and each `addq` before a return are removed when that is exact.

use rucc_base::hash::{Map, Set};
use rucc_object::plan9::{Addr6, History, Machine, NSNAME, Object, Prog6, Progs, amd64 as a6};

use crate::Trouble;

mod arm64;
mod oplook7;
mod optab;
// `r9 optab` writes the table, with each class and a `pub` that the check does not use.
#[allow(dead_code, unreachable_pub)]
mod optab6;
// `r9 optab --row arm64` writes the table, with each class and a `pub`.
#[allow(dead_code, unreachable_pub)]
#[rustfmt::skip]
mod optab7;

pub use arm64::arm64;

/// The records of a `.6` file for an amd64 listing.
///
/// `file` is the name of the source, which goes in the history, and `directory` is the directory
/// of the compilation, which is written in front of a relative name.
///
/// # Errors
///
/// A line that has no equivalent in a Plan 9 object, such as a section that Plan 9 does not have
/// or an instruction that the linker cannot encode.
pub fn amd64(listing: &str, file: &str, directory: Option<&str>) -> Result<Object, Trouble> {
    let lines = parse(listing, "#")?;
    let scope = Scope::new(&lines);
    let mut out = Out {
        scope: &scope,
        progs: Vec::new(),
        pc: 0,
        line: 0,
        labels: Map::default(),
        fixups: Vec::new(),
        frame: 0,
        cells: Vec::new(),
    };
    let (functions, data) = units(&lines)?;
    for (name, body) in &functions {
        out.function(name, body)?;
    }
    out.resolve()?;
    for datum in &data {
        out.datum(datum)?;
    }
    for (cell, target) in std::mem::take(&mut out.cells) {
        let mut from = Addr6::symbol(a6::D_STATIC, &cell, 0);
        from.scale = 8;
        let mut to = out.symbol(&target);
        to.index = to.ty;
        to.ty = a6::D_ADDR;
        out.progs.push(Prog6::new(a6::ADATA, 0, from, to));
        let sym = Addr6::symbol(a6::D_STATIC, &cell, 0);
        out.progs.push(Prog6::new(a6::AGLOBL, 0, sym, Addr6::constant(8)));
    }
    for (name, size) in &scope.commons {
        let sym = Addr6::symbol(scope.class(name), &scope.object_name(name), 0);
        out.progs.push(Prog6::new(a6::AGLOBL, 0, sym, Addr6::constant(*size)));
    }
    out.progs.push(Prog6::new(a6::AEND, out.line, Addr6::default(), Addr6::default()));
    let mut object = Object::new(Progs::Amd64(out.progs));
    object.directory = directory.map(str::to_owned);
    object.history.push(History::push(file, 1));
    Ok(object)
}

/// A function of the text, by name, with the lines of its body.
type Function<'l, 'a> = (&'a str, Vec<&'l Line<'a>>);

/// The functions and the data of the listing, in order.
fn units<'l, 'a>(lines: &'l [Line<'a>]) -> Result<(Vec<Function<'l, 'a>>, Vec<Datum>), Trouble> {
    let mut functions = Vec::new();
    let mut data = Vec::new();
    let mut section = Section::Text;
    let mut stack = Vec::new();
    let mut func: Option<Function<'l, 'a>> = None;
    let mut symbol: Option<Datum> = None;
    for line in lines {
        if let Item::Directive(name, args) = line.item {
            let switch = match name {
                ".text" => Some(Section::Text),
                ".data" => Some(Section::Data),
                ".bss" => Some(Section::Bss),
                ".section" | ".pushsection" => Some(section_of(args, line.number)?),
                ".previous" | ".popsection" => stack.pop(),
                _ => None,
            };
            if let Some(next) = switch {
                if name == ".pushsection" {
                    stack.push(section);
                } else if name != ".popsection" && name != ".previous" {
                    stack.clear();
                    stack.push(section);
                }
                if let Some(datum) = symbol.take() {
                    data.push(datum);
                }
                section = next;
                continue;
            }
        }
        match section {
            Section::Text => match line.item {
                Item::Label(label) if !label.starts_with(".L") => {
                    functions.extend(func.take());
                    func = Some((label, Vec::new()));
                }
                Item::Directive(".loc", _) => {
                    if let Some((_, body)) = &mut func {
                        body.push(line);
                    }
                }
                Item::Directive(name, args) => {
                    directive_anywhere(name, args, line.number)?;
                }
                Item::Label(_) | Item::Insn { .. } => match &mut func {
                    Some((_, body)) => body.push(line),
                    None => return Err(trouble(line.number, "an instruction outside a function")),
                },
            },
            Section::Data | Section::Bss => match line.item {
                Item::Label(label) => {
                    if let Some(datum) = symbol.take() {
                        data.push(datum);
                    }
                    symbol = Some(Datum::new(label, line.number));
                }
                Item::Directive(name, args) => {
                    let close = datum_directive(&mut symbol, name, args, line.number, section)?;
                    if close {
                        if let Some(datum) = symbol.take() {
                            data.push(datum);
                        }
                    }
                }
                Item::Insn { .. } => {
                    return Err(trouble(line.number, "an instruction outside the text"));
                }
            },
            Section::Other => {}
        }
    }
    functions.extend(func.take());
    data.extend(symbol.take());
    Ok((functions, data))
}

/// One line of the listing that says something.
#[derive(Debug, Clone, Copy)]
struct Line<'a> {
    /// The line number, from one.
    number: usize,
    /// What it says.
    item: Item<'a>,
}

#[derive(Debug, Clone, Copy)]
enum Item<'a> {
    /// `name:`.
    Label(&'a str),
    /// `.name args`.
    Directive(&'a str, &'a str),
    /// An instruction, with a prefix such as `lock` or `rep` and the operands as written.
    Insn { prefix: Option<&'a str>, mnemonic: &'a str, operands: &'a str },
}

fn trouble(line: usize, why: impl Into<String>) -> Trouble {
    Trouble { line, why: why.into() }
}

/// The lines of the listing, without the comments and the empty lines. A comment starts with
/// `comment`, which is `#` on amd64 and `//` on arm64.
fn parse<'a>(listing: &'a str, comment: &str) -> Result<Vec<Line<'a>>, Trouble> {
    let mut lines = Vec::new();
    for (index, raw) in listing.lines().enumerate() {
        let number = index + 1;
        let text = strip_comment(raw, comment).trim();
        if text.is_empty() {
            continue;
        }
        if let Some(label) = text.strip_suffix(':') {
            if !label.contains(char::is_whitespace) && !label.contains('"') {
                lines.push(Line { number, item: Item::Label(label) });
                continue;
            }
        }
        let (head, rest) = match text.find(char::is_whitespace) {
            Some(at) => (&text[..at], text[at..].trim()),
            None => (text, ""),
        };
        if head.starts_with('.') {
            lines.push(Line { number, item: Item::Directive(head, rest) });
            continue;
        }
        let item = if matches!(head, "lock" | "rep" | "repe" | "repz" | "repne" | "repnz")
            && !rest.is_empty()
        {
            let (mnemonic, operands) = match rest.find(char::is_whitespace) {
                Some(at) => (&rest[..at], rest[at..].trim()),
                None => (rest, ""),
            };
            Item::Insn { prefix: Some(head), mnemonic, operands }
        } else {
            Item::Insn { prefix: None, mnemonic: head, operands: rest }
        };
        lines.push(Line { number, item });
    }
    Ok(lines)
}

/// The line without a comment that is outside a string.
fn strip_comment<'a>(line: &'a str, comment: &str) -> &'a str {
    let mut quoted = false;
    let mut escaped = false;
    for (at, c) in line.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            _ if !quoted && line[at..].starts_with(comment) => return &line[..at],
            _ => {}
        }
    }
    line
}

/// Split operands at the commas that are not in parentheses or in a string.
fn split_operands(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    for (at, c) in text.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Text,
    Data,
    Bss,
    /// A section with nothing that the program needs, such as `.note.GNU-stack`.
    Other,
}

fn section_of(args: &str, line: usize) -> Result<Section, Trouble> {
    let name = args.split(',').next().unwrap_or("").trim().trim_matches('"');
    if name.starts_with(".text") {
        return Ok(Section::Text);
    }
    if name.starts_with(".tdata") || name.starts_with(".tbss") {
        return Err(trouble(
            line,
            format!("Plan 9 has no thread local storage, for the section {name}"),
        ));
    }
    if name.starts_with(".bss") || name.starts_with(".sbss") {
        return Ok(Section::Bss);
    }
    if name.starts_with(".rodata") || name.starts_with(".data") || name.starts_with(".sdata") {
        return Ok(Section::Data);
    }
    if name.starts_with(".note") || name.starts_with(".comment") || name.starts_with(".debug") {
        return Ok(Section::Other);
    }
    Err(trouble(line, format!("a Plan 9 object has no place for the section {name}")))
}

/// A directive that has the same effect in every section, or none.
fn directive_anywhere(name: &str, args: &str, line: usize) -> Result<(), Trouble> {
    match name {
        ".globl" | ".global" | ".weak" | ".local" | ".type" | ".size" | ".hidden"
        | ".protected" | ".internal" | ".ident" | ".file" | ".loc" | ".p2align" | ".balign"
        | ".align" | ".comm" | ".lcomm" => Ok(()),
        _ if name.starts_with(".cfi_") => Ok(()),
        _ => Err(trouble(
            line,
            format!("the directive {name} {args} has no equivalent in a Plan 9 object"),
        )),
    }
}

/// What the first pass found out about the symbols of the unit.
struct Scope {
    /// The symbols that are `.globl` or `.weak`.
    global: Set<String>,
    /// The symbols with a definition in the unit.
    defined: Set<String>,
    /// The common symbols, with their sizes, in order.
    commons: Vec<(String, i64)>,
}

impl Scope {
    fn new(lines: &[Line<'_>]) -> Self {
        let mut scope =
            Scope { global: Set::default(), defined: Set::default(), commons: Vec::new() };
        for line in lines {
            match line.item {
                Item::Label(label) => {
                    scope.defined.insert(label.to_owned());
                }
                Item::Directive(".globl" | ".global" | ".weak", args) => {
                    for name in args.split(',') {
                        scope.global.insert(name.trim().to_owned());
                    }
                }
                Item::Directive(name @ (".comm" | ".lcomm"), args) => {
                    let parts: Vec<&str> = args.split(',').map(str::trim).collect();
                    if let (Some(sym), Some(size)) =
                        (parts.first(), parts.get(1).and_then(|s| number(s)))
                    {
                        if name == ".comm" {
                            scope.global.insert((*sym).to_owned());
                        } else {
                            scope.defined.insert((*sym).to_owned());
                        }
                        scope.commons.push(((*sym).to_owned(), size));
                    }
                }
                _ => {}
            }
        }
        // A `.local` common is static, which the second loop sees after the first.
        for line in lines {
            if let Item::Directive(".local", args) = line.item {
                for name in args.split(',') {
                    let name = name.trim();
                    scope.global.remove(name);
                    scope.defined.insert(name.to_owned());
                }
            }
        }
        scope
    }

    /// Whether other objects cannot see the symbol.
    fn is_static(&self, name: &str) -> bool {
        !self.global.contains(name) && self.defined.contains(name)
    }

    /// `D_EXTERN` for a symbol that other objects can see, and `D_STATIC` for one they cannot.
    fn class(&self, name: &str) -> u8 {
        if self.is_static(name) { a6::D_STATIC } else { a6::D_EXTERN }
    }

    /// The name in the object. A local label of the listing starts with `.L`, which becomes a
    /// single `.`, a name that no C identifier can have.
    fn object_name(&self, name: &str) -> String {
        match name.strip_prefix(".L") {
            Some(rest) => format!(".{rest}"),
            None => name.to_owned(),
        }
    }
}

/// A number as the listing writes it, in decimal, in hexadecimal or in octal.
fn number(text: &str) -> Option<i64> {
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let value = if let Some(hex) = digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()? as i64
    } else if digits.len() > 1
        && digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit())
    {
        u64::from_str_radix(&digits[1..], 8).ok()? as i64
    } else {
        digits.parse::<u64>().ok()? as i64
    };
    Some(if negative { value.wrapping_neg() } else { value })
}

/// A symbol and an offset, either of which can be missing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Expr {
    sym: Option<String>,
    offset: i64,
}

fn expr(text: &str, line: usize) -> Result<Expr, Trouble> {
    let mut out = Expr::default();
    let text = text.trim();
    if text.is_empty() {
        return Ok(out);
    }
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut terms = Vec::new();
    for at in 1..bytes.len() {
        if (bytes[at] == b'+' || bytes[at] == b'-') && !matches!(bytes[at - 1], b'+' | b'-') {
            terms.push(&text[start..at]);
            start = at;
        }
    }
    terms.push(&text[start..]);
    for term in terms {
        let (sign, body) = match term.strip_prefix('+') {
            Some(rest) => (1, rest.trim()),
            None => match term.strip_prefix('-') {
                Some(rest) if !rest.starts_with(|c: char| c.is_ascii_digit()) => (-1, rest.trim()),
                _ => (1, term.trim()),
            },
        };
        if let Some(value) = number(body) {
            out.offset = out.offset.wrapping_add(value * sign);
        } else if sign == 1
            && out.sym.is_none()
            && body.chars().all(|c| {
                !c.is_ascii() || c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$' | '@')
            })
            && body != "."
        {
            out.sym = Some(body.to_owned());
        } else {
            return Err(trouble(
                line,
                format!("the expression {text} has no equivalent in a Plan 9 object"),
            ));
        }
    }
    Ok(out)
}

/// The width of a register operand, as the suffix of an AT&T mnemonic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Width {
    B,
    W,
    L,
    Q,
    X,
}

/// A register of the listing as its `D_` value and its width.
fn register(name: &str) -> Option<(u8, Width)> {
    const Q: [&str; 16] = [
        "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12",
        "r13", "r14", "r15",
    ];
    const L: [&str; 8] = ["eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi"];
    const W: [&str; 8] = ["ax", "cx", "dx", "bx", "sp", "bp", "si", "di"];
    const B: [&str; 8] = ["al", "cl", "dl", "bl", "spl", "bpl", "sil", "dil"];
    const H: [&str; 4] = ["ah", "ch", "dh", "bh"];
    let index = |list: &[&str]| list.iter().position(|r| *r == name).map(|n| n as u8);
    if let Some(n) = index(&Q) {
        return Some((a6::D_AX + n, Width::Q));
    }
    if let Some(n) = index(&L) {
        return Some((a6::D_AX + n, Width::L));
    }
    if let Some(n) = index(&W) {
        return Some((a6::D_AX + n, Width::W));
    }
    if let Some(n) = index(&B) {
        return Some((a6::D_AL + n, Width::B));
    }
    if let Some(n) = index(&H) {
        return Some((a6::D_AH + n, Width::B));
    }
    if let Some(rest) = name.strip_prefix("xmm") {
        let n: u8 = rest.parse().ok()?;
        return (n < 16).then_some((a6::D_X0 + n, Width::X));
    }
    let rest = name.strip_prefix('r')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let n: u8 = digits.parse().ok()?;
    if !(8..16).contains(&n) {
        return None;
    }
    let width = match &rest[digits.len()..] {
        "" => Width::Q,
        "d" => Width::L,
        "w" => Width::W,
        "b" => return Some((a6::D_AL + n, Width::B)),
        _ => return None,
    };
    Some((a6::D_AX + n, width))
}

/// One operand of an instruction.
#[derive(Debug, Clone, PartialEq)]
enum Operand {
    Reg(u8, Width),
    Imm(Expr),
    Mem {
        disp: Expr,
        base: Option<u8>,
        rip: bool,
        index: Option<u8>,
        scale: u8,
    },
    /// A bare name, which is the target of a jump or a call.
    Name(Expr),
    /// `*operand`, the target of an indirect jump or call.
    Star(Box<Operand>),
}

fn operand(text: &str, line: usize) -> Result<Operand, Trouble> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('*') {
        return Ok(Operand::Star(Box::new(operand(rest, line)?)));
    }
    if let Some(rest) = text.strip_prefix('%') {
        if rest.contains(':') {
            return Err(trouble(line, format!("Plan 9 has no segment override, as in {text}")));
        }
        let (reg, width) =
            register(rest).ok_or_else(|| trouble(line, format!("6l has no register {text}")))?;
        return Ok(Operand::Reg(reg, width));
    }
    if let Some(rest) = text.strip_prefix('$') {
        return Ok(Operand::Imm(expr(rest, line)?));
    }
    if let Some(open) = text.find('(') {
        let close = text.rfind(')').ok_or_else(|| trouble(line, format!("no ')' in {text}")))?;
        let disp = expr(&text[..open], line)?;
        let inside: Vec<&str> = text[open + 1..close].split(',').map(str::trim).collect();
        let mut base = None;
        let mut rip = false;
        if let Some(first) = inside.first().filter(|s| !s.is_empty()) {
            let name = first
                .strip_prefix('%')
                .ok_or_else(|| trouble(line, format!("a base that is not a register in {text}")))?;
            if name == "rip" {
                rip = true;
            } else {
                base = Some(
                    register(name)
                        .ok_or_else(|| trouble(line, format!("6l has no register {first}")))?
                        .0,
                );
            }
        }
        let index = match inside.get(1).filter(|s| !s.is_empty()) {
            Some(r) => Some(
                register(r.trim_start_matches('%'))
                    .ok_or_else(|| trouble(line, format!("6l has no register {r}")))?
                    .0,
            ),
            None => None,
        };
        let scale = match inside.get(2) {
            Some(s) => number(s)
                .and_then(|n| u8::try_from(n).ok())
                .ok_or_else(|| trouble(line, format!("a bad scale in {text}")))?,
            None => u8::from(index.is_some()),
        };
        return Ok(Operand::Mem { disp, base, rip, index, scale });
    }
    if text.contains(':') {
        return Err(trouble(line, format!("Plan 9 has no segment override, as in {text}")));
    }
    let e = expr(text, line)?;
    if e.sym.is_some() {
        return Ok(Operand::Name(e));
    }
    Ok(Operand::Mem { disp: e, base: None, rip: false, index: None, scale: 0 })
}

/// The conditions, as AT&T writes them and as Plan 9 writes them.
fn condition(cc: &str) -> Option<&'static str> {
    Some(match cc {
        "o" => "OS",
        "no" => "OC",
        "b" | "c" | "nae" => "CS",
        "ae" | "nb" | "nc" => "CC",
        "e" | "z" => "EQ",
        "ne" | "nz" => "NE",
        "be" | "na" => "LS",
        "a" | "nbe" => "HI",
        "s" => "MI",
        "ns" => "PL",
        "p" | "pe" => "PS",
        "np" | "po" => "PC",
        "l" | "nge" => "LT",
        "ge" | "nl" => "GE",
        "le" | "ng" => "LE",
        "g" | "nle" => "GT",
        _ => return None,
    })
}

const fn suffix(width: Width) -> &'static str {
    match width {
        Width::B => "B",
        Width::W => "W",
        Width::L => "L",
        Width::Q | Width::X => "Q",
    }
}

fn size_letter(c: char) -> Option<Width> {
    match c {
        'b' => Some(Width::B),
        'w' => Some(Width::W),
        'l' => Some(Width::L),
        'q' => Some(Width::Q),
        _ => None,
    }
}

/// Whether an instruction reads the flags, writes them without a read, or does neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flags {
    Read,
    Write,
    Neither,
}

fn flags(mnemonic: &str) -> Flags {
    let m = mnemonic;
    if m == "jmp" || m == "call" || m == "ret" || m == "retq" {
        return Flags::Write;
    }
    if (m.starts_with('j') && condition(&m[1..]).is_some())
        || (m.starts_with("set") && condition(&m[3..]).is_some())
        || m.starts_with("cmov")
        || m.starts_with("adc")
        || m.starts_with("sbb")
        || m.starts_with("pushf")
    {
        return Flags::Read;
    }
    const WRITERS: [&str; 24] = [
        "cmp", "test", "add", "sub", "and", "or", "xor", "neg", "inc", "dec", "shl", "shr", "sar",
        "sal", "rol", "ror", "imul", "mul", "div", "idiv", "ucomis", "comis", "bsf", "bsr",
    ];
    if WRITERS.iter().any(|w| m.starts_with(w))
        && !m.starts_with("orp")
        && !m.starts_with("andp")
        && !m.starts_with("xorp")
    {
        return Flags::Write;
    }
    Flags::Neither
}

/// One symbol of the data, as it is read.
struct Datum {
    name: String,
    line: usize,
    bytes: Vec<u8>,
    /// The addresses in it: where, how wide, and of what.
    addresses: Vec<(usize, u8, Expr)>,
    size: Option<i64>,
}

impl Datum {
    fn new(name: &str, line: usize) -> Self {
        Datum { name: name.to_owned(), line, bytes: Vec::new(), addresses: Vec::new(), size: None }
    }
}

/// A directive in a data section. The result says whether the symbol is complete.
fn datum_directive(
    symbol: &mut Option<Datum>,
    name: &str,
    args: &str,
    line: usize,
    section: Section,
) -> Result<bool, Trouble> {
    let width = match name {
        ".byte" => Some(1u8),
        ".short" | ".value" | ".2byte" | ".hword" => Some(2),
        ".long" | ".int" | ".4byte" => Some(4),
        ".quad" | ".8byte" => Some(8),
        _ => None,
    };
    if let Some(width) = width {
        let datum = symbol.as_mut().ok_or_else(|| trouble(line, "data outside a symbol"))?;
        for part in split_operands(args) {
            let e = expr(part, line)?;
            let at = datum.bytes.len();
            datum.bytes.extend(e.offset.to_le_bytes().iter().take(usize::from(width)));
            if e.sym.is_some() {
                if width < 4 {
                    return Err(trouble(line, format!("an address in {width} bytes")));
                }
                datum.bytes[at..].fill(0);
                datum.addresses.push((at, width, e));
            }
        }
        if section == Section::Bss && datum.bytes.iter().any(|&b| b != 0) {
            return Err(trouble(line, "a value that is not zero in .bss"));
        }
        return Ok(false);
    }
    match name {
        ".ascii" | ".asciz" | ".string" => {
            let datum = symbol.as_mut().ok_or_else(|| trouble(line, "data outside a symbol"))?;
            for part in split_operands(args) {
                datum.bytes.extend(unquote(part, line)?);
                if name != ".ascii" {
                    datum.bytes.push(0);
                }
            }
            Ok(false)
        }
        ".zero" | ".skip" | ".space" => {
            let datum = symbol.as_mut().ok_or_else(|| trouble(line, "data outside a symbol"))?;
            let parts = split_operands(args);
            let count = parts
                .first()
                .and_then(|s| number(s))
                .ok_or_else(|| trouble(line, format!("a bad count in {name} {args}")))?;
            let fill = parts.get(1).and_then(|s| number(s)).unwrap_or(0) as u8;
            datum.bytes.extend(std::iter::repeat_n(fill, count as usize));
            Ok(false)
        }
        ".p2align" | ".balign" | ".align" => {
            if let Some(datum) = symbol.as_mut() {
                let n = split_operands(args).first().and_then(|s| number(s)).unwrap_or(0);
                let step =
                    if name == ".p2align" { 1usize << n.clamp(0, 12) } else { n.max(1) as usize };
                while datum.bytes.len() % step != 0 {
                    datum.bytes.push(0);
                }
            }
            Ok(false)
        }
        ".size" => {
            let parts = split_operands(args);
            if let (Some(datum), Some(sym)) = (symbol.as_mut(), parts.first()) {
                if *sym == datum.name {
                    let size = match parts.get(1) {
                        Some(s) if s.starts_with(".-") => datum.bytes.len() as i64,
                        Some(s) => number(s).unwrap_or(datum.bytes.len() as i64),
                        None => datum.bytes.len() as i64,
                    };
                    datum.size = Some(size);
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => directive_anywhere(name, args, line).map(|()| false),
    }
}

/// The bytes of a string of the listing.
fn unquote(text: &str, line: usize) -> Result<Vec<u8>, Trouble> {
    let inner = text
        .trim()
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .ok_or_else(|| trouble(line, format!("a string that is not quoted, {text}")))?;
    let mut out = Vec::with_capacity(inner.len());
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        if b != b'\\' {
            out.push(b);
            continue;
        }
        let Some(&e) = bytes.get(i) else { break };
        i += 1;
        match e {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'a' => out.push(7),
            b'0'..=b'7' => {
                let mut value = u32::from(e - b'0');
                for _ in 0..2 {
                    match bytes.get(i) {
                        Some(&d @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out.push(value as u8);
            }
            b'x' => {
                let mut value = 0u32;
                while let Some(d) = bytes.get(i).and_then(|&d| (d as char).to_digit(16)) {
                    value = value * 16 + d;
                    i += 1;
                }
                out.push(value as u8);
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// The records as they are written.
struct Out<'s> {
    scope: &'s Scope,
    progs: Vec<Prog6>,
    /// The number of the next instruction, as the linker counts them.
    pc: i64,
    /// The line of the source of the next record.
    line: u32,
    /// Where each local label is, as a `pc`.
    labels: Map<String, i64>,
    /// The records with a branch to a label, and the label.
    fixups: Vec<(usize, String, usize)>,
    /// The size of the frame of the function that is written.
    frame: i64,
    /// The cells that hold the address of the target of a tail call from a function with no
    /// frame, by the name of the target.
    cells: Vec<(String, Expr)>,
}

impl Out<'_> {
    fn push(&mut self, op: u16, from: Addr6, to: Addr6) {
        self.progs.push(Prog6::new(op, self.line, from, to));
        self.pc += 1;
    }

    fn op(&self, name: &str, line: usize) -> Result<u16, Trouble> {
        Machine::Amd64
            .opcode(&format!("A{name}"))
            .ok_or_else(|| trouble(line, format!("6l has no instruction {name}")))
    }

    fn symbol(&self, e: &Expr) -> Addr6 {
        let name = e.sym.as_deref().unwrap_or_default();
        Addr6::symbol(self.scope.class(name), &self.scope.object_name(name), e.offset)
    }

    fn addr(&self, operand: &Operand, line: usize) -> Result<Addr6, Trouble> {
        Ok(match operand {
            Operand::Reg(reg, _) => Addr6::of(*reg),
            Operand::Imm(e) if e.sym.is_some() => {
                let mut a = self.symbol(e);
                a.index = a.ty;
                a.ty = a6::D_ADDR;
                a
            }
            Operand::Imm(e) => Addr6::constant(e.offset),
            Operand::Mem { disp, base, rip, index, scale } => {
                let mut a = if disp.sym.is_some() {
                    let mut a = self.symbol(disp);
                    if let Some(base) = base {
                        if index.is_some() {
                            return Err(trouble(line, "a symbol with a base and an index"));
                        }
                        a.index = *base;
                        a.scale = 1;
                    }
                    a
                } else if *rip {
                    return Err(trouble(
                        line,
                        "an address relative to the program counter with no symbol",
                    ));
                } else {
                    let mut a = Addr6::of(a6::D_INDIR + base.unwrap_or(a6::D_NONE));
                    a.offset = disp.offset;
                    a
                };
                if let Some(index) = index {
                    a.index = *index;
                    a.scale = *scale;
                }
                a
            }
            Operand::Name(e) => self.symbol(e),
            Operand::Star(inner) => self.addr(inner, line)?,
        })
    }

    /// One function: its `TEXT` and its instructions.
    fn function(&mut self, name: &str, body: &[&Line<'_>]) -> Result<(), Trouble> {
        let insns: Vec<(usize, &Line<'_>)> = body
            .iter()
            .enumerate()
            .filter(|(_, l)| matches!(l.item, Item::Insn { .. }))
            .map(|(i, l)| (i, *l))
            .collect();
        let frame = frame(body, &insns);
        let mut text = Addr6::symbol(self.scope.class(name), &self.scope.object_name(name), 0);
        text.scale = 0;
        let line = body.first().map_or(0, |l| l.number);
        self.frame = frame.unwrap_or(0);
        self.push(a6::ATEXT, text, Addr6::constant(self.frame));
        let mut skip: Set<usize> = Set::default();
        if let Some(size) = frame {
            skip.insert(insns[0].0);
            for (k, (at, l)) in insns.iter().enumerate() {
                if is_release(l, size) {
                    if let Some((after, next)) = insns.get(k + 1) {
                        if ends(next)
                            && !body[*at + 1..*after]
                                .iter()
                                .any(|l| matches!(l.item, Item::Label(_)))
                        {
                            skip.insert(*at);
                        }
                    }
                }
            }
        }
        let _ = line;
        for (at, l) in body.iter().enumerate() {
            match l.item {
                Item::Label(label) => {
                    self.labels.insert(label.to_owned(), self.pc);
                }
                Item::Directive(".loc", args) => {
                    if let Some(n) = args.split_whitespace().nth(1).and_then(|s| s.parse().ok()) {
                        self.line = n;
                    }
                }
                Item::Directive(..) => {}
                Item::Insn { prefix, mnemonic, operands } => {
                    if skip.contains(&at) {
                        continue;
                    }
                    let first = self.progs.len();
                    self.insn(prefix, mnemonic, operands, l.number, &body[at + 1..])?;
                    for p in &self.progs[first..] {
                        if let Some((from, to, why)) = optab::CHECKER.check(p) {
                            let name = a6::NAMES[usize::from(p.op)];
                            return Err(trouble(
                                l.number,
                                format!(
                                    "6l cannot encode {} with the operand classes {} and {}: {why}",
                                    name.strip_prefix('A').unwrap_or(name),
                                    optab::Checker::name(from),
                                    optab::Checker::name(to),
                                ),
                            ));
                        }
                    }
                }
            }
        }
        // A body that ends in a call that does not return, such as `exits`, ends with no jump.
        // `6l` follows the code from one instruction to the next to lay it out, and it would go
        // on into the next function and put the branch targets of this one after the last
        // function, where it gives them the frame of that function. `6c` ends each function
        // with a `RET`, and so does this.
        let jmp = self.op("JMP", 0)?;
        if self.progs.last().is_none_or(|p| p.op != a6::ARET && p.op != jmp) {
            self.push(a6::ARET, Addr6::default(), Addr6::default());
        }
        Ok(())
    }

    /// A jump through a register or through memory. 6l sends each branch on through a chain of
    /// `JMP`s to the end of the chain, and an indirect `JMP` has no target that it knows. Thus a
    /// branch to an indirect `JMP` gets no target, and 6l makes it a branch to itself. 6c never
    /// writes such a branch. When a label is at the jump, a `NOP` goes first and the label is at
    /// the `NOP`. 6l keeps the `NOP` as a branch target and writes no byte for it.
    fn indirect_jump(&mut self, to: Addr6, line: usize) -> Result<(), Trouble> {
        if self.labels.values().any(|&pc| pc == self.pc) {
            let nop = self.op("NOP", line)?;
            self.push(nop, Addr6::default(), Addr6::default());
        }
        let op = self.op("JMP", line)?;
        self.push(op, Addr6::default(), to);
        Ok(())
    }

    fn branch(&mut self, op: u16, label: &str, line: usize) {
        let mut to = Addr6::of(a6::D_BRANCH);
        to.offset = 0;
        self.fixups.push((self.progs.len(), label.to_owned(), line));
        self.push(op, Addr6::default(), to);
    }

    fn insn(
        &mut self,
        prefix: Option<&str>,
        m: &str,
        text: &str,
        line: usize,
        rest: &[&Line<'_>],
    ) -> Result<(), Trouble> {
        let parts = split_operands(text);
        let ops: Vec<Operand> = parts.iter().map(|p| operand(p, line)).collect::<Result<_, _>>()?;
        if let Some(prefix) = prefix {
            let name = match prefix {
                "lock" => "LOCK",
                "rep" | "repe" | "repz" => "REP",
                _ => "REPN",
            };
            let op = self.op(name, line)?;
            self.push(op, Addr6::default(), Addr6::default());
        }
        let none = Addr6::default;
        match m {
            "nop" | "nopw" | "nopl" | "endbr64" => return Ok(()),
            "ret" | "retq" if ops.is_empty() => {
                self.push(a6::ARET, none(), none());
                return Ok(());
            }
            "jmp" | "jmpq" => {
                return match &ops[..] {
                    [Operand::Name(e)] if self.is_label(e) => {
                        let op = self.op("JMP", line)?;
                        self.branch(op, e.sym.as_deref().unwrap_or_default(), line);
                        Ok(())
                    }
                    [Operand::Name(e)] if self.frame > 0 => {
                        // A tail call, which the linker changes into the release of the frame and
                        // a direct jump.
                        let to = self.symbol(e);
                        self.push(a6::ARET, Addr6::constant(0), to);
                        Ok(())
                    }
                    [Operand::Name(e)] => {
                        // With no frame the linker writes a release of nothing, which it cannot
                        // encode, so the jump goes through a cell that holds the address.
                        let target = self.scope.object_name(e.sym.as_deref().unwrap_or_default());
                        let cell = match e.offset {
                            0 => format!(".tail.{target}"),
                            offset => format!(".tail.{target}+{offset}"),
                        };
                        if !self.cells.iter().any(|(name, _)| *name == cell) {
                            self.cells.push((cell.clone(), e.clone()));
                        }
                        self.indirect_jump(Addr6::symbol(a6::D_STATIC, &cell, 0), line)
                    }
                    [Operand::Star(inner)] => {
                        let to = self.addr(inner, line)?;
                        self.indirect_jump(to, line)
                    }
                    _ => Err(trouble(line, format!("a jump to {text}"))),
                };
            }
            "call" | "callq" => {
                let op = self.op("CALL", line)?;
                return match &ops[..] {
                    [Operand::Name(e)] => {
                        let to = self.symbol(e);
                        self.push(op, none(), to);
                        Ok(())
                    }
                    [Operand::Star(inner)] => {
                        let target = self.addr(inner, line)?;
                        if target.sym.is_some() {
                            // A call with a symbol is a direct call in 6l, so the pointer goes
                            // through AX, which holds the result of the call after it.
                            let mov = self.op("MOVQ", line)?;
                            self.push(mov, target, Addr6::of(a6::D_AX));
                            self.push(op, none(), Addr6::of(a6::D_AX));
                        } else {
                            self.push(op, none(), target);
                        }
                        Ok(())
                    }
                    _ => Err(trouble(line, format!("a call to {text}"))),
                };
            }
            "ud2" => {
                let op = self.op("BYTE", line)?;
                self.push(op, Addr6::constant(0x0f), none());
                self.push(op, Addr6::constant(0x0b), none());
                return Ok(());
            }
            "int3" => {
                let op = self.op("INT", line)?;
                self.push(op, Addr6::constant(3), none());
                return Ok(());
            }
            "pause" => {
                let op = self.op("BYTE", line)?;
                self.push(op, Addr6::constant(0xf3), none());
                self.push(op, Addr6::constant(0x90), none());
                return Ok(());
            }
            "bswapl" | "bswapq" | "bswap" => {
                let [Operand::Reg(reg, width)] = &ops[..] else {
                    return Err(trouble(line, format!("bswap of {text}")));
                };
                let n = reg - a6::D_AX;
                let op = self.op("BYTE", line)?;
                let mut rex = 0u8;
                if *width == Width::Q {
                    rex |= 0x48;
                }
                if n >= 8 {
                    rex |= 0x41;
                }
                if rex != 0 {
                    self.push(op, Addr6::constant(i64::from(rex)), none());
                }
                self.push(op, Addr6::constant(0x0f), none());
                self.push(op, Addr6::constant(i64::from(0xc8 + (n & 7))), none());
                return Ok(());
            }
            _ => {}
        }
        if let Some(cc) = m.strip_prefix('j').and_then(condition) {
            let [Operand::Name(e)] = &ops[..] else {
                return Err(trouble(line, format!("a branch to {text}")));
            };
            let op = self.op(&format!("J{cc}"), line)?;
            self.branch(op, e.sym.as_deref().unwrap_or_default(), line);
            return Ok(());
        }
        if let Some(cc) = m.strip_prefix("set").and_then(condition) {
            let op = self.op(&format!("SET{cc}"), line)?;
            let to =
                self.addr(ops.first().ok_or_else(|| trouble(line, "set with no operand"))?, line)?;
            self.push(op, none(), to);
            return Ok(());
        }
        let width_of = |ops: &[Operand]| {
            ops.iter().rev().find_map(|o| match o {
                Operand::Reg(_, w) if *w != Width::X => Some(*w),
                _ => None,
            })
        };
        // A double shift is SHL or SHR in 6l. The source register is the index of the count.
        if let Some(rest) = m.strip_prefix("shld").or_else(|| m.strip_prefix("shrd")) {
            let width = match rest.chars().next() {
                Some(c) => size_letter(c).filter(|_| rest.len() == 1),
                None => width_of(&ops),
            };
            let (count, src, dst) = match &ops[..] {
                [count, src, dst] => (self.addr(count, line)?, src, dst),
                [src, dst] => (Addr6::of(a6::D_CL), src, dst),
                _ => return Err(trouble(line, format!("{m} with the wrong number of operands"))),
            };
            let Some(width) = width.filter(|w| matches!(w, Width::W | Width::L | Width::Q)) else {
                return Err(trouble(line, format!("6l has no instruction {m}")));
            };
            let Operand::Reg(src, _) = src else {
                return Err(trouble(line, format!("the source of {m} is not a register")));
            };
            let op = self.op(&format!("SH{}{}", m[2..3].to_uppercase(), suffix(width)), line)?;
            let mut from = count;
            from.index = *src;
            from.scale = 0;
            let to = self.addr(dst, line)?;
            self.push(op, from, to);
            return Ok(());
        }
        let name: String = if let Some(rest) = m.strip_prefix("cmov") {
            let (cc, width) = match rest.char_indices().last() {
                Some((at, c)) if size_letter(c).is_some() && condition(&rest[..at]).is_some() => {
                    (condition(&rest[..at]), size_letter(c))
                }
                _ => (condition(rest), width_of(&ops)),
            };
            let (Some(cc), Some(width)) = (cc, width) else {
                return Err(trouble(line, format!("6l has no instruction {m}")));
            };
            format!("CMOV{}{cc}", suffix(width))
        } else if m.len() == 6
            && m.starts_with("mov")
            && matches!(&m[3..4], "s" | "z")
            && size_letter(m.as_bytes()[4] as char).is_some()
            && size_letter(m.as_bytes()[5] as char).is_some()
        {
            let kind = if &m[3..4] == "s" { "SX" } else { "ZX" };
            format!("MOV{}{}{kind}", m[4..5].to_uppercase(), m[5..6].to_uppercase())
        } else {
            match m {
                "movabsq" | "movabs" => "MOVQ".to_owned(),
                "movd" => "MOVL".to_owned(),
                "movdqu" => "MOVOU".to_owned(),
                "movdqa" => "MOVO".to_owned(),
                "movsd" | "movsl" if ops.is_empty() => "MOVSL".to_owned(),
                "stosd" | "stosl" => "STOSL".to_owned(),
                "cltq" | "cdqe" => {
                    let op = self.op("MOVLQSX", line)?;
                    self.push(op, Addr6::of(a6::D_AX), Addr6::of(a6::D_AX));
                    return Ok(());
                }
                "cwtl" | "cwde" => {
                    let op = self.op("MOVWLSX", line)?;
                    self.push(op, Addr6::of(a6::D_AX), Addr6::of(a6::D_AX));
                    return Ok(());
                }
                "cqto" | "cqo" => "CQO".to_owned(),
                "cltd" | "cdq" => "CDQ".to_owned(),
                "cwtd" | "cwd" => "CWD".to_owned(),
                _ if m.starts_with("cvt") => convert(m, &ops)
                    .ok_or_else(|| trouble(line, format!("6l has no instruction {m}")))?,
                _ if m.starts_with("sal") => format!("SHL{}", m[3..].to_uppercase()),
                _ => {
                    let upper = m.to_uppercase();
                    if Machine::Amd64.opcode(&format!("A{upper}")).is_some() {
                        upper
                    } else if let Some(width) = width_of(&ops) {
                        format!("{upper}{}", suffix(width))
                    } else {
                        upper
                    }
                }
            }
        };
        // A move of zero to a register is an XOR in 6l. Where the flags are live after it, the
        // move is a load of the address zero instead, which leaves the flags as they are.
        if matches!(name.as_str(), "MOVL" | "MOVQ" | "MOVW")
            && matches!(&ops[..], [Operand::Imm(Expr { sym: None, offset: 0 }), Operand::Reg(..)])
            && live(rest)
        {
            let op = self.op(&format!("LEA{}", &name[3..]), line)?;
            let to = self.addr(&ops[1], line)?;
            self.push(op, Addr6::of(a6::D_INDIR + a6::D_NONE), to);
            return Ok(());
        }
        // `imul $c, src, dst` has no form in 6l, so the source is moved to the destination first.
        if name.starts_with("IMUL") && ops.len() == 3 {
            let op = self.op(&name, line)?;
            let from = self.addr(&ops[0], line)?;
            let src = self.addr(&ops[1], line)?;
            let dst = self.addr(&ops[2], line)?;
            if src != dst {
                let mov = self.op(&format!("MOV{}", &name[4..]), line)?;
                self.push(mov, src, dst.clone());
            }
            self.push(op, from, dst);
            return Ok(());
        }
        let op = self.op(&name, line)?;
        let (from, to) = match &ops[..] {
            [] => (none(), none()),
            [one] => {
                let shift = ["SHL", "SHR", "SAR", "ROL", "ROR"].iter().any(|s| name.starts_with(s));
                let a = self.addr(one, line)?;
                if shift {
                    (Addr6::constant(1), a)
                } else if ["DIV", "IDIV", "MUL", "IMUL", "PUSH"].iter().any(|s| name.starts_with(s))
                {
                    (a, none())
                } else {
                    (none(), a)
                }
            }
            [a, b] => {
                let (a, b) = (self.addr(a, line)?, self.addr(b, line)?);
                if name.starts_with("CMP")
                    && !name.starts_with("CMPXCHG")
                    && !name.starts_with("CMPS")
                {
                    (b, a)
                } else {
                    (a, b)
                }
            }
            _ => return Err(trouble(line, format!("6l has no form of {m} with three operands"))),
        };
        self.push(op, from, to);
        Ok(())
    }

    fn is_label(&self, e: &Expr) -> bool {
        e.offset == 0 && e.sym.as_deref().is_some_and(|s| s.starts_with(".L"))
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
        let class = self.scope.class(&datum.name);
        let name = self.scope.object_name(&datum.name);
        let mut covered = vec![false; datum.bytes.len()];
        for (at, width, e) in &datum.addresses {
            if self.labels.contains_key(e.sym.as_deref().unwrap_or_default()) {
                return Err(trouble(
                    datum.line,
                    format!("the address of a label of the text in {}", datum.name),
                ));
            }
            covered[*at..*at + usize::from(*width)].fill(true);
            let mut from = Addr6::symbol(class, &name, *at as i64);
            from.scale = *width;
            let mut to = self.symbol(e);
            to.index = to.ty;
            to.ty = a6::D_ADDR;
            self.progs.push(Prog6::new(a6::ADATA, 0, from, to));
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
                let mut from = Addr6::symbol(class, &name, at as i64);
                from.scale = chunk.len() as u8;
                let mut to = Addr6::of(a6::D_SCONST);
                to.string[..chunk.len()].copy_from_slice(chunk);
                self.progs.push(Prog6::new(a6::ADATA, 0, from, to));
            }
            at = end;
        }
        let size = datum.size.unwrap_or(0).max(datum.bytes.len() as i64);
        self.progs.push(Prog6::new(
            a6::AGLOBL,
            0,
            Addr6::symbol(class, &name, 0),
            Addr6::constant(size),
        ));
        Ok(())
    }
}

/// The size of the frame, when the function allocates it with its first instruction and releases
/// it right before each return.
fn frame(body: &[&Line<'_>], insns: &[(usize, &Line<'_>)]) -> Option<i64> {
    let (_, first) = insns.first()?;
    let Item::Insn { prefix: None, mnemonic: "subq", operands } = first.item else {
        return None;
    };
    let parts = split_operands(operands);
    let [size, "%rsp"] = parts[..] else { return None };
    let size = number(size.strip_prefix('$')?)?;
    if size <= 0 {
        return None;
    }
    for (k, (_, l)) in insns.iter().enumerate().skip(1) {
        if ends(l) {
            let (at, before) = insns.get(k - 1)?;
            if k == 1 || !is_release(before, size) {
                return None;
            }
            let (this, _) = insns[k];
            if body[at + 1..this].iter().any(|l| matches!(l.item, Item::Label(_))) {
                return None;
            }
        }
    }
    Some(size)
}

/// Whether the line is `addq $size, %rsp`.
fn is_release(line: &Line<'_>, size: i64) -> bool {
    let Item::Insn { prefix: None, mnemonic: "addq", operands } = line.item else {
        return false;
    };
    let parts = split_operands(operands);
    matches!(parts[..], [s, "%rsp"] if s.strip_prefix('$').and_then(number) == Some(size))
}

/// Whether the line leaves the function: a return or a tail call.
fn ends(line: &Line<'_>) -> bool {
    match line.item {
        Item::Insn { mnemonic: "ret" | "retq", .. } => true,
        Item::Insn { mnemonic: "jmp" | "jmpq", operands, .. } => {
            let target = operands.trim();
            !target.starts_with(".L") && !target.starts_with('*')
        }
        _ => false,
    }
}

/// Whether the flags can be read before an instruction writes them.
fn live(rest: &[&Line<'_>]) -> bool {
    for line in rest {
        match line.item {
            Item::Label(_) => return true,
            Item::Directive(..) => {}
            Item::Insn { mnemonic, operands, .. } => {
                if mnemonic == "jmp" && operands.trim().starts_with(".L") {
                    return true;
                }
                match flags(mnemonic) {
                    Flags::Read => return true,
                    Flags::Write => return false,
                    Flags::Neither => {}
                }
            }
        }
    }
    false
}

/// The name of a conversion in 6l.
fn convert(m: &str, ops: &[Operand]) -> Option<String> {
    let reg_width = |o: &Operand| match o {
        Operand::Reg(_, w) => Some(*w),
        _ => None,
    };
    let int = |explicit: Option<char>, o: Option<&Operand>| -> Option<&'static str> {
        let w =
            explicit.and_then(size_letter).or_else(|| o.and_then(reg_width)).unwrap_or(Width::L);
        Some(if w == Width::Q { "Q" } else { "L" })
    };
    for (stem, to) in [("cvtsi2sd", "SD"), ("cvtsi2ss", "SS")] {
        if let Some(rest) = m.strip_prefix(stem) {
            let w = int(rest.chars().next(), ops.first())?;
            return Some(format!("CVTS{w}2{to}"));
        }
    }
    for (stem, name) in [
        ("cvttsd2si", "CVTTSD2S"),
        ("cvttss2si", "CVTTSS2S"),
        ("cvtsd2si", "CVTSD2S"),
        ("cvtss2si", "CVTSS2S"),
    ] {
        if let Some(rest) = m.strip_prefix(stem) {
            let w = int(rest.chars().next(), ops.get(1))?;
            return Some(format!("{name}{w}"));
        }
    }
    match m {
        "cvtss2sd" | "cvtsd2ss" | "cvtps2pd" | "cvtpd2ps" => Some(m.to_uppercase()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rucc_object::plan9::{Record, read_plan9, write_plan9};

    fn progs(listing: &str) -> Vec<Prog6> {
        let object = amd64(listing, "a.c", Some("/usr/glenda")).expect("the listing translates");
        let bytes = write_plan9(&object).expect("the object is written");
        let records = read_plan9(Machine::Amd64, &bytes).expect("the object is read back");
        records
            .into_iter()
            .filter_map(|r| match r {
                Record::Amd64(p) if p.op != a6::AHISTORY => Some(p),
                _ => None,
            })
            .collect()
    }

    fn names(progs: &[Prog6]) -> Vec<&'static str> {
        progs.iter().map(|p| Machine::Amd64.opcode_name(p.op).unwrap_or("?")).collect()
    }

    #[test]
    fn a_frame_goes_in_the_text_record() {
        let p = progs(
            "\t.text\n\t.globl\tf\nf:\n.Lf_0:\n\tsubq\t$24, %rsp\n\tmovq\t%rax, 8(%rsp)\n\tcall\tg\n\taddq\t$24, %rsp\n\tret\n",
        );
        assert_eq!(names(&p), ["ATEXT", "AMOVQ", "ACALL", "ARET", "AEND"]);
        assert_eq!(p[0].to.offset, 24);
        assert_eq!(p[0].from.ty, a6::D_EXTERN);
        assert_eq!(p[1].to.ty, a6::D_INDIR + a6::D_SP);
        assert_eq!(p[1].to.offset, 8);
        assert_eq!(p[2].to.sym.as_deref(), Some("g"));
    }

    #[test]
    fn a_body_that_ends_in_a_call_gets_a_return() {
        let p = progs(
            "f:\n\tsubq\t$8, %rsp\n\ttestl\t%eax, %eax\n\tje\t.Lf_1\n\taddq\t$8, %rsp\n\tret\n.Lf_1:\n\tcall\texits\ng:\n\tret\n",
        );
        assert_eq!(
            names(&p),
            ["ATEXT", "ATESTL", "AJEQ", "ARET", "ACALL", "ARET", "ATEXT", "ARET", "AEND"]
        );
    }

    #[test]
    fn a_comparison_has_its_operands_in_the_other_order() {
        let p = progs("f:\n\tcmpl\t%ecx, %edx\n\tjl\t.Lf_1\n.Lf_1:\n\tcmpl\t$7, %eax\n\tret\n");
        assert_eq!(names(&p), ["ATEXT", "ACMPL", "AJLT", "ACMPL", "ARET", "AEND"]);
        assert_eq!((p[1].from.ty, p[1].to.ty), (a6::D_DX, a6::D_CX));
        assert_eq!(p[2].to.ty, a6::D_BRANCH);
        assert_eq!(p[2].to.offset, 3);
        assert_eq!((p[3].from.ty, p[3].to.ty, p[3].to.offset), (a6::D_AX, a6::D_CONST, 7));
    }

    #[test]
    fn a_zero_with_live_flags_is_a_load_of_an_address() {
        let p = progs(
            "f:\n\tcmpl\t%ecx, %edx\n\tmovl\t$0, %eax\n\tsete\t%al\n\tmovl\t$0, %ecx\n\tret\n",
        );
        assert_eq!(names(&p), ["ATEXT", "ACMPL", "ALEAL", "ASETEQ", "AMOVL", "ARET", "AEND"]);
        assert_eq!(p[2].from.ty, a6::D_INDIR + a6::D_NONE);
    }

    #[test]
    fn a_tail_call_and_a_call_through_memory() {
        let p = progs("f:\n\tsubq\t$8, %rsp\n\tcall\t*fp(%rip)\n\taddq\t$8, %rsp\n\tjmp\tg\n");
        assert_eq!(names(&p), ["ATEXT", "AMOVQ", "ACALL", "ARET", "AEND"]);
        assert_eq!(p[1].from.sym.as_deref(), Some("fp"));
        assert_eq!(p[2].to.ty, a6::D_AX);
        assert_eq!((p[3].from.ty, p[3].to.sym.as_deref()), (a6::D_CONST, Some("g")));
        // With no frame, through a cell.
        let p = progs("f:\n\tjmp\tg\nh:\n\tjmp\tg\n");
        assert_eq!(names(&p), ["ATEXT", "AJMP", "ATEXT", "AJMP", "ADATA", "AGLOBL", "AEND"]);
        assert_eq!((p[1].to.ty, p[1].to.sym.as_deref()), (a6::D_STATIC, Some(".tail.g")));
        assert_eq!(
            (p[4].to.ty, p[4].to.index, p[4].to.sym.as_deref()),
            (a6::D_ADDR, a6::D_EXTERN, Some("g"))
        );
    }

    /// 6l gives no target to a branch to an indirect jump, so a `NOP` is the target. This is
    /// `run` in /sys/src/cmd/mk/run.c, where `mk -n` looped on a `JLT` to itself.
    #[test]
    fn a_branch_to_an_indirect_jump_goes_to_a_nop() {
        let p = progs(
            "f:
	cmpl	$1, %eax
	jl	.L1
	ret
.L1:
	jmp	g
",
        );
        assert_eq!(
            names(&p),
            ["ATEXT", "ACMPL", "AJLT", "ARET", "ANOP", "AJMP", "ADATA", "AGLOBL", "AEND"]
        );
        assert_eq!(p[2].to.offset, 4);
        let p = progs(
            "f:
	testl	%eax, %eax
	je	.L1
	ret
.L1:
	jmp	*%rcx
",
        );
        assert_eq!(names(&p), ["ATEXT", "ATESTL", "AJEQ", "ARET", "ANOP", "AJMP", "AEND"]);
        assert_eq!(p[2].to.offset, 4);
        // With no label at the jump, there is no NOP.
        let p = progs(
            "f:
	jmp	*%rcx
",
        );
        assert_eq!(names(&p), ["ATEXT", "AJMP", "AEND"]);
    }

    #[test]
    fn a_frame_that_is_not_released_before_each_return_stays_in_the_code() {
        let p = progs(
            "f:\n\tsubq\t$8, %rsp\n\ttestl\t%eax, %eax\n\tje\t.Lf_1\n\taddq\t$8, %rsp\n.Lf_1:\n\tret\n",
        );
        assert_eq!(names(&p), ["ATEXT", "ASUBQ", "ATESTL", "AJEQ", "AADDQ", "ARET", "AEND"]);
        assert_eq!(p[0].to.offset, 0);
    }

    #[test]
    fn a_release_is_taken_out_when_a_label_comes_later_in_the_body() {
        // The release before the first return goes, though a block follows it. Left in, the
        // frame would be released twice, once here and once by the linker.
        let p = progs(
            "f:\n\tsubq\t$8, %rsp\n\ttestl\t%eax, %eax\n\tje\t.Lf_1\n\taddq\t$8, %rsp\n\t.cfi_def_cfa_offset 8\n\tret\n\
             .Lf_1:\n\tmovl\t$1, %eax\n\taddq\t$8, %rsp\n\tret\n",
        );
        assert_eq!(names(&p), ["ATEXT", "ATESTL", "AJEQ", "ARET", "AMOVL", "ARET", "AEND"]);
        assert_eq!(p[0].to.offset, 8);
    }

    #[test]
    fn the_spellings_that_differ() {
        let p = progs(
            "f:\n\tmovslq\t%edx, %rdi\n\tmovzbl\t%al, %eax\n\tcmovll\t%eax, %ecx\n\tcvtsi2sdq\t%rax, %xmm0\n\
             \tcvttsd2sil\t%xmm0, %eax\n\tshlq\t%cl, %rsi\n\tcqto\n\tidivq\t%rcx\n\timulq\t$3, %rax, %rcx\n\
             \tleaq\t(%rdi,%rdi,2), %rdi\n\tmovq\t8(%rax,%rdi,8), %r8\n\tret\n",
        );
        assert_eq!(
            names(&p),
            [
                "ATEXT",
                "AMOVLQSX",
                "AMOVBLZX",
                "ACMOVLLT",
                "ACVTSQ2SD",
                "ACVTTSD2SL",
                "ASHLQ",
                "ACQO",
                "AIDIVQ",
                "AMOVQ",
                "AIMULQ",
                "ALEAQ",
                "AMOVQ",
                "ARET",
                "AEND"
            ]
        );
        assert_eq!(p[6].from.ty, a6::D_CL);
        assert_eq!(p[8].from.ty, a6::D_CX);
        assert_eq!(
            (p[11].from.ty, p[11].from.index, p[11].from.scale),
            (a6::D_INDIR + a6::D_DI, a6::D_DI, 2)
        );
        assert_eq!(
            (p[12].from.ty, p[12].from.index, p[12].from.scale, p[12].from.offset),
            (a6::D_INDIR + a6::D_AX, a6::D_DI, 8, 8)
        );
        assert_eq!(p[12].to.ty, a6::D_R8);
    }

    #[test]
    fn data_is_written_in_pieces_of_eight_bytes_and_addresses() {
        let p = progs(
            "\t.data\n\t.globl\tgreet\ngreet:\n\t.quad\t.Lstr.0\n\t.size\tgreet, .-greet\n\t.section\t.rodata\n\
             .Lstr.0:\n\t.ascii\t\"hello, world\\012\\000\"\n\t.size\t.Lstr.0, .-.Lstr.0\n\t.bss\ncount:\n\t.zero\t4\n",
        );
        assert_eq!(names(&p), ["ADATA", "AGLOBL", "ADATA", "ADATA", "AGLOBL", "AGLOBL", "AEND"]);
        assert_eq!((p[0].from.sym.as_deref(), p[0].from.scale), (Some("greet"), 8));
        assert_eq!(
            (p[0].to.ty, p[0].to.index, p[0].to.sym.as_deref()),
            (a6::D_ADDR, a6::D_STATIC, Some(".str.0"))
        );
        assert_eq!(&p[2].to.string, b"hello, w");
        assert_eq!((p[3].from.offset, p[3].from.scale), (8, 6));
        assert_eq!(&p[3].to.string[..6], b"orld\n\0");
        assert_eq!((p[4].from.ty, p[4].to.offset), (a6::D_STATIC, 14));
        assert_eq!(
            (p[5].from.sym.as_deref(), p[5].from.ty, p[5].to.offset),
            (Some("count"), a6::D_STATIC, 4)
        );
    }

    #[test]
    fn a_double_shift_has_the_source_as_the_index_of_the_count() {
        let p = progs("f:\n\tshrdl\t$1, %esi, %ecx\n\tshldq\t%cl, %rdx, %rax\n\tret\n");
        assert_eq!(names(&p), ["ATEXT", "ASHRL", "ASHLQ", "ARET", "AEND"]);
        assert_eq!((p[1].from.ty, p[1].from.offset, p[1].from.index), (a6::D_CONST, 1, a6::D_SI));
        assert_eq!((p[1].from.scale, p[1].to.ty), (0, a6::D_CX));
        assert_eq!((p[2].from.ty, p[2].from.index, p[2].to.ty), (a6::D_CL, a6::D_DX, a6::D_AX));
    }

    #[test]
    fn a_symbol_can_end_in_x_or_have_letters_that_are_not_ascii() {
        let p = progs(
            "f:\n\tleaq\tonex+512(%rip), %rax\n\tjmp\t.L\u{3c4}conv_4\n.L\u{3c4}conv_4:\n\tret\n",
        );
        assert_eq!(names(&p), ["ATEXT", "ALEAQ", "AJMP", "ARET", "AEND"]);
        assert_eq!((p[1].from.sym.as_deref(), p[1].from.offset), (Some("onex"), 512));
        assert_eq!(p[2].to.ty, a6::D_BRANCH);
    }

    #[test]
    fn a_quote_after_a_backslash_does_not_end_the_string() {
        assert_eq!(split_operands(r#""a\"b,c", 1"#), [r#""a\"b,c""#, "1"]);
        let p = progs("\t.data\ns:\n\t.ascii\t\"\\\"x,y\\\"\"\n");
        assert_eq!(&p[0].to.string[..5], b"\"x,y\"");
    }

    #[test]
    fn what_plan9_has_no_place_for_is_refused() {
        for listing in [
            "\t.section\t.tbss\nx:\n\t.zero\t4\n",
            "f:\n\tmovq\t%fs:0, %rax\n\tret\n",
            "\t.section\t.init_array\n\t.quad\tf\n",
        ] {
            assert!(amd64(listing, "a.c", None).is_err(), "{listing}");
        }
    }

    #[test]
    fn an_instruction_that_6l_cannot_encode_is_refused_at_its_line() {
        let listing = "f:\n\tmovq\t$1, (%rax)\n\tmovq\t$1099511627776, (%rax)\n\tret\n";
        let trouble =
            amd64(listing, "a.c", None).expect_err("6l has no store of a 64-bit constant");
        assert_eq!(trouble.line, 3);
        assert_eq!(
            trouble.why,
            "6l cannot encode MOVQ with the operand classes Yi64 and Ym: no pair of its ytab or of \
             ymovtab covers the classes of its operands"
        );
    }
}
