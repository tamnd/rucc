//! The `.seh_` directives an AArch64 Windows function is described with, worked out from the
//! instructions its listing writes.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4, and `crate::xdata` for the codes.
//!
//! x86-64 builds its codes out of the rows the frame code writes, because every x86 code is about
//! one thing a row says: a push, an allocation, a frame pointer. ARM64 is not like that. Its table
//! has exactly one code for every instruction of a prologue and of each epilogue, and the codes
//! are the instructions themselves: `.seh_save_regp_x x19, 16` is `stp x19, x20, [sp, #-16]!` and
//! nothing else, and the unwinder does its work by running the codes of the instructions that have
//! run, backwards. So what is read here is the instruction, as the listing spells it, and the code
//! is the one that instruction is. An instruction the table has no code for gets `.seh_nop`, which
//! is right for anything that moves neither the stack pointer nor a register the unwinder puts
//! back.
//!
//! Which instructions are the prologue is the frame code's answer: the prologue ends at the
//! instruction the row that remembers the body's rules is written behind. Each epilogue is read
//! backwards from the `ret` or the tail call that ends it, for as long as the instructions are
//! ones an epilogue writes, and it is the run of them the table is told about.
//!
//! What cannot be said is refused with [`Error::Frame`], the way x86-64 refuses a prologue its
//! codes cannot describe. The one thing checked beyond the codes themselves is the stack pointer:
//! the codes restore every register from where the stack pointer is, so a function whose body
//! moves it, which is one with `alloca` or a variable length array, has to end its prologue with
//! the frame pointer code that puts it back first. The frame code writes such a prologue on this
//! target, and the check here is what keeps a table that would unwind from the wrong place out of
//! an object if it ever stopped.

use crate::Error;
use crate::xdata::Code;

/// One instruction of the function, as the listing writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    /// The instruction without the tab in front, such as `stp x29, x30, [sp, #-16]!`.
    pub text: String,
    /// Which block it is in, counted in the order the listing writes them.
    pub block: usize,
    /// Which of the function's instructions it was written for, counted the same way.
    pub place: usize,
}

/// What goes around the instructions of one function.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// The directives written in front of each instruction, by its index in the lines given.
    pub before: Vec<Vec<String>>,
    /// The directives written after each instruction.
    pub after: Vec<Vec<String>>,
    /// Whether the prologue is nothing, which is `.seh_endprologue` straight after `.seh_proc`.
    pub empty: bool,
}

/// Whether a line of the listing is an instruction, which is one that is indented and is not a
/// directive. A label is not indented.
pub(crate) fn machine(line: &str) -> bool {
    line.starts_with('\t') && !line.starts_with("\t.") && !line.trim().is_empty()
}

/// The directives for a function whose prologue ends with the instruction written for `end`, or
/// has no prologue at all when that is `None`.
pub(crate) fn plan(func: &str, lines: &[Line], end: Option<usize>) -> Result<Plan, Error> {
    let mut plan = Plan {
        before: vec![Vec::new(); lines.len()],
        after: vec![Vec::new(); lines.len()],
        empty: true,
    };
    let parsed: Vec<Inst> = lines.iter().map(|line| Inst::parse(&line.text)).collect();

    // The prologue, one code an instruction, with what `x15` was given kept for the subtraction
    // that takes a frame reached through `__chkstk`.
    let count = end.map_or(0, |end| lines.iter().take_while(|line| line.place <= end).count());
    let mut codes = Vec::with_capacity(count);
    let mut x15 = 0u32;
    for (at, inst) in parsed.iter().enumerate().take(count) {
        let code = match prologue(inst, &mut x15) {
            Some(code) => code,
            // A register the caller wants back, put away in a way no code says, such as a pair of
            // registers that are not next to each other. A `nop` would leave it unrestored.
            None if inst.saves() => {
                let why = format!("a prologue that saves registers with `{}`", lines[at].text);
                return Err(frame(func, why));
            }
            None if inst.writes_sp() => {
                // An instruction that moves the stack pointer by an amount that is not a constant,
                // which is the alignment a frame forces. The codes before it cannot be run from
                // below it, so the one just before it has to be the frame pointer code, which is
                // what makes the rest of the unwind not care where the stack pointer went.
                if !matches!(last_real(&codes), Some(Code::SetFp | Code::AddFp(_))) {
                    let why = format!(
                        "a prologue that moves the stack pointer with `{}`",
                        lines[at].text
                    );
                    return Err(frame(func, why));
                }
                Code::Nop
            }
            None => Code::Nop,
        };
        codes.push(code);
    }
    for (at, code) in codes.iter().enumerate() {
        check(func, *code)?;
        plan.after[at].push(code.to_string());
    }
    if count > 0 {
        plan.after[count - 1].push("\t.seh_endprologue".to_owned());
        plan.empty = false;
    }

    // The epilogues, each ending at the instruction that leaves the function.
    let mut ends = Vec::new();
    for (at, inst) in parsed.iter().enumerate().skip(count) {
        if count == 0 || !inst.leaves() {
            continue;
        }
        let Some(start) = epilogue_start(&parsed, lines, count, at) else {
            continue;
        };
        let scope: Vec<Code> =
            parsed[start..at].iter().map(|inst| epilogue(inst).unwrap_or(Code::Nop)).collect();
        for code in &scope {
            check(func, *code)?;
        }
        plan.before[start].push("\t.seh_startepilogue".to_owned());
        for (offset, code) in scope.iter().enumerate() {
            plan.after[start + offset].push(code.to_string());
        }
        plan.after[at - 1].push("\t.seh_endepilogue".to_owned());
        ends.push(start..at);
    }

    // The body, which is everything that is neither. If anything in it moves the stack pointer,
    // unwinding from there has to start by putting it back from the frame pointer.
    let body = |at: usize| at >= count && !ends.iter().any(|range| range.contains(&at));
    let moved = parsed.iter().enumerate().find(|&(at, inst)| body(at) && inst.writes_sp());
    if let Some((at, _)) = moved {
        let settled =
            match codes.iter().rposition(|code| matches!(code, Code::SetFp | Code::AddFp(_))) {
                Some(fp) => codes[fp + 1..].iter().all(|code| *code == Code::Nop),
                None => false,
            };
        if !settled {
            let why = format!(
                "a body that moves the stack pointer with `{}` after a prologue that does not end \
                 by setting the frame pointer",
                lines[at].text
            );
            return Err(frame(func, why));
        }
    }
    Ok(plan)
}

/// The last code that is not a `nop`.
fn last_real(codes: &[Code]) -> Option<Code> {
    codes.iter().rev().copied().find(|code| *code != Code::Nop)
}

/// Refuses a code whose numbers do not fit, which is the same refusal the assembler would make
/// reading it back, said here with the function's name on it.
fn check(func: &str, code: Code) -> Result<(), Error> {
    code.bytes()
        .map(|_| ())
        .map_err(|why| frame(func, format!("`{}` ({why})", code.to_string().trim())))
}

/// Where the epilogue ending in front of the instruction at `at` starts, or `None` when nothing in
/// front of it gives the frame back.
///
/// Walks back through the same block for as long as the instructions are ones an epilogue writes
/// or ones that leave the frame alone, and starts the epilogue at the first of the ones that give
/// it back. The instructions that leave it alone are there because the scheduler may put the last
/// of the body in among the restores, and they are described as `nop`, which is what they are to
/// the unwinder.
fn epilogue_start(parsed: &[Inst], lines: &[Line], floor: usize, at: usize) -> Option<usize> {
    let block = lines[at].block;
    let mut start = None;
    let mut look = at;
    while look > floor {
        let before = look - 1;
        if lines[before].block != block {
            break;
        }
        let inst = &parsed[before];
        if epilogue(inst).is_some() {
            start = Some(before);
        } else if !inst.harmless() {
            break;
        }
        look = before;
    }
    start
}

/// The code an instruction of a prologue is, or `None` for one that is not a code of its own.
fn prologue(inst: &Inst, x15: &mut u32) -> Option<Code> {
    let ops: Vec<&str> = inst.ops.iter().map(String::as_str).collect();
    match (inst.mnemonic.as_str(), ops.as_slice()) {
        ("stp", [a, b, mem]) => {
            let (at, pre) = sp_mem(mem)?;
            save_pair(a, b, at, pre)
        }
        ("str", [a, mem]) => {
            let (at, pre) = sp_mem(mem)?;
            save_one(a, at, pre)
        }
        ("mov", ["x29", "sp"]) => Some(Code::SetFp),
        ("add", ["x29", "sp", imm]) => match number(imm)? {
            0 => Some(Code::SetFp),
            n => Some(Code::AddFp(u32::try_from(n).ok()?)),
        },
        ("sub", ["sp", "sp", imm]) => Some(Code::Alloc(u32::try_from(number(imm)?).ok()?)),
        ("sub", ["sp", "sp", imm, "lsl #12"]) => {
            Some(Code::Alloc(u32::try_from(number(imm)?).ok()? << 12))
        }
        ("sub", ["sp", "sp", "x15", "lsl #4"]) => Some(Code::Alloc(*x15 << 4)),
        ("mov", ["x15", imm]) => {
            *x15 = u32::try_from(number(imm)?).ok()?;
            Some(Code::Nop)
        }
        ("movk", ["x15", imm, "lsl #16"]) => {
            *x15 = (*x15 & 0xffff) | (u32::try_from(number(imm)?).ok()? << 16);
            Some(Code::Nop)
        }
        _ => None,
    }
}

/// The code an instruction of an epilogue is, or `None` for one that gives nothing back.
///
/// The same codes as the prologue's, since what an epilogue does is what the unwinder does: a
/// load that takes a pair back off the stack is the code of the store that put it there.
fn epilogue(inst: &Inst) -> Option<Code> {
    let ops: Vec<&str> = inst.ops.iter().map(String::as_str).collect();
    match (inst.mnemonic.as_str(), ops.as_slice()) {
        ("ldp", [a, b, "[sp]", imm]) => save_pair(a, b, u32::try_from(number(imm)?).ok()?, true),
        ("ldp", [a, b, mem]) => {
            let (at, pre) = sp_mem(mem)?;
            (!pre).then_some(())?;
            save_pair(a, b, at, false)
        }
        ("ldr", [a, "[sp]", imm]) => save_one(a, u32::try_from(number(imm)?).ok()?, true),
        ("ldr", [a, mem]) => {
            let (at, pre) = sp_mem(mem)?;
            (!pre).then_some(())?;
            save_one(a, at, false)
        }
        ("mov", ["sp", "x29"]) => Some(Code::SetFp),
        ("add", ["sp", "x29", imm]) => match number(imm)? {
            0 => Some(Code::SetFp),
            n if n < 0 => Some(Code::AddFp(u32::try_from(-n).ok()?)),
            _ => None,
        },
        ("sub", ["sp", "x29", imm]) => match number(imm)? {
            0 => Some(Code::SetFp),
            n => Some(Code::AddFp(u32::try_from(n).ok()?)),
        },
        ("add", ["sp", "sp", imm]) => Some(Code::Alloc(u32::try_from(number(imm)?).ok()?)),
        ("add", ["sp", "sp", imm, "lsl #12"]) => {
            Some(Code::Alloc(u32::try_from(number(imm)?).ok()? << 12))
        }
        _ => None,
    }
}

/// The code of a pair stored at, or loaded from, that many bytes above the stack pointer, where
/// `moved` says the stack pointer moves by that many as well.
fn save_pair(a: &str, b: &str, at: u32, moved: bool) -> Option<Code> {
    match (reg(a)?, reg(b)?) {
        (('x', 29), ('x', 30)) => Some(if moved { Code::FplrX(at) } else { Code::Fplr(at) }),
        (('x', first), ('x', 30)) if !moved && first >= 19 => Some(Code::Lrpair(first, at)),
        (('x', first), ('x', second)) if first >= 19 && second == first + 1 => {
            Some(if moved { Code::RegpX(first, at) } else { Code::Regp(first, at) })
        }
        (('d', first), ('d', second)) if first >= 8 && second == first + 1 => {
            Some(if moved { Code::FregpX(first, at) } else { Code::Fregp(first, at) })
        }
        _ => None,
    }
}

/// The code of one register stored at, or loaded from, that many bytes above the stack pointer.
fn save_one(a: &str, at: u32, moved: bool) -> Option<Code> {
    match reg(a)? {
        ('x', number) if number >= 19 => {
            Some(if moved { Code::RegX(number, at) } else { Code::Reg(number, at) })
        }
        ('d', number) if number >= 8 => {
            Some(if moved { Code::FregX(number, at) } else { Code::Freg(number, at) })
        }
        _ => None,
    }
}

/// An address off the stack pointer, `[sp]`, `[sp, #n]` or `[sp, #-n]!`, as how far and whether
/// the stack pointer moves there first. A move is always down, and an offset that is not is never
/// one, which is the only way a prologue writes either.
fn sp_mem(mem: &str) -> Option<(u32, bool)> {
    let (inside, pre) = match mem.strip_suffix('!') {
        Some(inside) => (inside, true),
        None => (mem, false),
    };
    let inside = inside.strip_prefix('[')?.strip_suffix(']')?;
    let mut parts = inside.split(',').map(str::trim);
    if parts.next()? != "sp" {
        return None;
    }
    let at = match parts.next() {
        Some(imm) => number(imm)?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    match (pre, at) {
        (true, at) if at < 0 => Some((u32::try_from(-at).ok()?, true)),
        (false, at) if at >= 0 => Some((u32::try_from(at).ok()?, false)),
        _ => None,
    }
}

/// A register as its bank and number: `x19` is `('x', 19)` and `d8` is `('d', 8)`.
fn reg(text: &str) -> Option<(char, u8)> {
    let text = match text {
        "fp" => "x29",
        "lr" => "x30",
        other => other,
    };
    let bank = text.chars().next()?;
    let number = text[1..].parse().ok()?;
    matches!(bank, 'x' | 'd').then_some((bank, number))
}

/// A number written `#16`, `#-16` or `#0x10`.
fn number(text: &str) -> Option<i64> {
    let text = text.strip_prefix('#').unwrap_or(text);
    let (negative, text) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let value = match text.strip_prefix("0x") {
        Some(hex) => i64::from_str_radix(hex, 16).ok()?,
        None => text.parse().ok()?,
    };
    Some(if negative { -value } else { value })
}

/// An instruction taken apart into its mnemonic and its operands.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Inst {
    mnemonic: String,
    /// Split at the commas outside brackets, so `[sp, #16]` is one operand and `[sp], #16` is two.
    ops: Vec<String>,
}

impl Inst {
    fn parse(text: &str) -> Inst {
        let text = text.trim();
        let (mnemonic, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let mut ops = Vec::new();
        let mut depth = 0;
        let mut current = String::new();
        for c in rest.chars() {
            match c {
                '[' => depth += 1,
                ']' => depth -= 1,
                ',' if depth == 0 => {
                    ops.push(current.trim().to_owned());
                    current.clear();
                    continue;
                }
                _ => {}
            }
            current.push(c);
        }
        if !current.trim().is_empty() {
            ops.push(current.trim().to_owned());
        }
        Inst { mnemonic: mnemonic.to_ascii_lowercase(), ops }
    }

    /// Whether this leaves the function, which is a return or a branch to a name that is not a
    /// label of this function, the tail call.
    fn leaves(&self) -> bool {
        match self.mnemonic.as_str() {
            "ret" => true,
            "b" | "br" => self.ops.first().is_some_and(|to| !to.starts_with(".L")),
            _ => false,
        }
    }

    /// Whether this writes the stack pointer, as its destination or through a write back.
    fn writes_sp(&self) -> bool {
        let first = self.ops.first().map(String::as_str);
        let stores = self.mnemonic.starts_with("st");
        if !stores && matches!(first, Some("sp" | "wsp")) {
            return true;
        }
        // A write back, `[sp, #-16]!` before or `[sp], #16` after.
        let at = self.ops.iter().position(|op| op.starts_with("[sp"));
        at.is_some_and(|at| self.ops[at].ends_with('!') || at + 1 < self.ops.len())
    }

    /// Whether this is a store onto the stack of a register a call leaves alone, which is
    /// `x19` to `x30` and `d8` to `d15`.
    fn saves(&self) -> bool {
        if !self.mnemonic.starts_with("st") || !self.ops.iter().any(|op| op.starts_with("[sp")) {
            return false;
        }
        self.ops.iter().take_while(|op| !op.starts_with('[')).any(|op| match reg(op) {
            Some(('x', number)) => number >= 19,
            Some(('d', number)) => (8..16).contains(&number),
            _ => false,
        })
    }

    /// Whether this can sit among an epilogue's restores as a `nop`: it neither leaves nor calls,
    /// and it writes none of the stack pointer, the frame pointer and the link register.
    fn harmless(&self) -> bool {
        let control = matches!(
            self.mnemonic.as_str(),
            "b" | "bl" | "blr" | "br" | "ret" | "cbz" | "cbnz" | "tbz" | "tbnz"
        ) || self.mnemonic.starts_with("b.");
        let first = self.ops.first().map(String::as_str);
        let stores = self.mnemonic.starts_with("st");
        let frame = !stores && matches!(first, Some("sp" | "wsp" | "x29" | "x30" | "fp" | "lr"));
        !control && !frame && !self.writes_sp()
    }
}

/// A function this cannot describe, named with what about it.
fn frame(func: &str, why: String) -> Error {
    Error::Frame { func: func.to_owned(), why }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<Line> {
        text.iter()
            .enumerate()
            .map(|(place, text)| Line { text: (*text).to_owned(), block: 0, place })
            .collect()
    }

    /// Every line with what goes around it, the way the listing will have it.
    fn written(text: &[&str], end: Option<usize>) -> Vec<String> {
        let lines = lines(text);
        let plan = plan("f", &lines, end).unwrap();
        let mut out = Vec::new();
        for (at, line) in lines.iter().enumerate() {
            out.extend(plan.before[at].iter().map(|line| line.trim().replace('\t', " ")));
            out.push(line.text.clone());
            out.extend(plan.after[at].iter().map(|line| line.trim().replace('\t', " ")));
        }
        out
    }

    /// The frame rucc writes for a function that saves registers and has locals, described one
    /// code an instruction, with the epilogue read back from the `ret`.
    #[test]
    fn a_frame_is_one_code_an_instruction_both_ways() {
        let got = written(
            &[
                "stp x29, x30, [sp, #-16]!",
                "mov x29, sp",
                "stp x19, x20, [sp, #-16]!",
                "str x21, [sp, #-16]!",
                "sub sp, sp, #32",
                "str d8, [sp, #8]",
                "bl g",
                "ldr d8, [sp, #8]",
                "add sp, x29, #-32",
                "ldr x21, [sp], #16",
                "ldp x19, x20, [sp], #16",
                "ldp x29, x30, [sp], #16",
                "ret",
            ],
            Some(5),
        );
        let want = [
            "stp x29, x30, [sp, #-16]!",
            ".seh_save_fplr_x 16",
            "mov x29, sp",
            ".seh_set_fp",
            "stp x19, x20, [sp, #-16]!",
            ".seh_save_regp_x x19, 16",
            "str x21, [sp, #-16]!",
            ".seh_save_reg_x x21, 16",
            "sub sp, sp, #32",
            ".seh_stackalloc 32",
            "str d8, [sp, #8]",
            ".seh_save_freg d8, 8",
            ".seh_endprologue",
            "bl g",
            ".seh_startepilogue",
            "ldr d8, [sp, #8]",
            ".seh_save_freg d8, 8",
            "add sp, x29, #-32",
            ".seh_add_fp 32",
            "ldr x21, [sp], #16",
            ".seh_save_reg_x x21, 16",
            "ldp x19, x20, [sp], #16",
            ".seh_save_regp_x x19, 16",
            "ldp x29, x30, [sp], #16",
            ".seh_save_fplr_x 16",
            ".seh_endepilogue",
            "ret",
        ];
        assert_eq!(got, want);
    }

    /// The call a large frame is reached with is two `nop`s and the subtraction after it is the
    /// whole of the frame, the size `x15` was given counted in sixteens, as clang writes it.
    #[test]
    fn a_frame_reached_through_chkstk_is_the_size_in_x15() {
        let got = written(
            &[
                "stp x29, x30, [sp, #-16]!",
                "mov x29, sp",
                "mov x15, #13398",
                "movk x15, #18, lsl #16",
                "bl __chkstk",
                "sub sp, sp, x15, lsl #4",
                "ret",
            ],
            Some(5),
        );
        assert_eq!(
            got[4..12],
            [
                "mov x15, #13398",
                ".seh_nop",
                "movk x15, #18, lsl #16",
                ".seh_nop",
                "bl __chkstk",
                ".seh_nop",
                "sub sp, sp, x15, lsl #4",
                ".seh_stackalloc 19088736",
            ]
        );
    }

    /// Something of the body the scheduler put among the restores is a `nop` inside the epilogue,
    /// and the epilogue starts at the first restore rather than at it.
    #[test]
    fn the_body_among_the_restores_is_a_nop_and_not_the_start() {
        let got = written(
            &[
                "stp x29, x30, [sp, #-16]!",
                "mov x29, sp",
                "add w0, w0, #1",
                "mov sp, x29",
                "uxtb w0, w0",
                "ldp x29, x30, [sp], #16",
                "ret",
            ],
            Some(1),
        );
        assert_eq!(
            got[5..],
            [
                "add w0, w0, #1",
                ".seh_startepilogue",
                "mov sp, x29",
                ".seh_set_fp",
                "uxtb w0, w0",
                ".seh_nop",
                "ldp x29, x30, [sp], #16",
                ".seh_save_fplr_x 16",
                ".seh_endepilogue",
                "ret",
            ]
        );
    }

    /// A function with no frame is an empty prologue and no epilogue at all.
    #[test]
    fn a_leaf_with_no_frame_is_described_by_nothing() {
        let lines = lines(&["add w0, w0, #1", "ret"]);
        let plan = plan("f", &lines, None).unwrap();
        assert!(plan.empty);
        assert!(plan.before.iter().chain(&plan.after).all(Vec::is_empty));
    }

    /// A body that moves the stack pointer is only described when the prologue ends by setting the
    /// frame pointer, since everything after that code would be undone from the wrong place.
    #[test]
    fn a_body_that_moves_the_stack_pointer_needs_the_frame_pointer_last() {
        let body = ["mov x16, sp", "sub x16, x16, x0", "mov sp, x16", "bl g"];
        let early = [
            &["stp x29, x30, [sp, #-16]!", "mov x29, sp", "str x19, [sp, #-16]!"][..],
            &body[..],
            &["add sp, x29, #-16", "ldr x19, [sp], #16", "ldp x29, x30, [sp], #16", "ret"][..],
        ]
        .concat();
        assert!(plan("f", &lines(&early), Some(2)).is_err());
        let restated = [
            &["stp x29, x30, [sp, #-16]!", "mov x29, sp", "str x19, [sp, #-16]!"][..],
            &["add x29, sp, #16"][..],
            &body[..],
            &["add sp, x29, #-16", "ldr x19, [sp], #16", "ldp x29, x30, [sp], #16", "ret"][..],
        ]
        .concat();
        let got = written(&restated, Some(3));
        assert_eq!(got[6..9], ["add x29, sp, #16", ".seh_add_fp 16", ".seh_endprologue"]);
    }

    /// A pair of registers that are not next to each other has no code, and a prologue saving
    /// one is refused rather than described with a `nop` that would leave both unrestored.
    #[test]
    fn a_pair_that_is_not_two_registers_in_a_row_is_refused() {
        assert_eq!(save_pair("x19", "x20", 16, true), Some(Code::RegpX(19, 16)));
        let text = ["stp x29, x30, [sp, #-16]!", "mov x29, sp", "stp x19, x21, [sp, #-16]!", "ret"];
        assert!(plan("f", &lines(&text), Some(2)).is_err());
    }
}
