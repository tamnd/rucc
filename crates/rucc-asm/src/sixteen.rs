//! Instructions after `.code16` and `.code16gcc`, which is the real mode code the kernel runs
//! before it reaches protected mode and when it comes back from suspend.
//!
//! Sixteen bit code is thirty two bit code with the defaults turned round. The operand size prefix
//! `0x66` means thirty two bits there rather than sixteen, and the address size prefix `0x67` means
//! an address made of thirty two bit registers. Everything else, the opcodes and the addressing
//! byte, is the same. So rather than a second encoder this asks the thirty two bit one and then
//! moves the two prefixes:
//!
//! - An instruction whose operands are thirty two bits wide gets `0x66`, and one whose operands are
//!   sixteen bits loses the `0x66` the thirty two bit encoding put on it. Which of the two an
//!   instruction is comes from writing it again the other width, `movw %ax, %bx` beside `movl %eax,
//!   %ebx`, and seeing which of the pair has the prefix. An instruction with no width, `cli` or
//!   `movb`, has the same bytes both ways and is left alone.
//! - An address through thirty two bit registers gets `0x67`. An address that is only a number or
//!   a name is two bytes rather than four, `movl pa_tr_cr4, %eax` being `66 a1` and a sixteen bit
//!   relocation, which is what gas writes and what the kernel's `relocs --realmode` expects.
//! - A jump or a call counts its distance in two bytes rather than four.
//!
//! `.code16gcc` is the same with one difference, which is that the stack is thirty two bits wide,
//! because the code gcc writes under `-m16` is thirty two bit code with a prefix on every line. So
//! `call`, `ret`, `push`, `pop`, `pushf`, `popf` and `leave` without a width letter are the thirty
//! two bit ones there.

use crate::instruction::{Written, addressed, one_in};
use rucc_target::x86_64::{Mode, Width, gpr_name, gpr_named};

/// The bytes of one instruction in sixteen bit code.
///
/// `gcc` is whether the file said `.code16gcc` rather than `.code16`.
///
/// # Errors
///
/// A sentence saying what about the line could not be written, the same as [`one_in`].
pub(crate) fn one_in16(word: &str, args: &[String], gcc: bool) -> Result<Written, String> {
    // `(%dx)` is the port of an `in` or an `out`, written like an address and not one.
    if let Some(arg) = args.iter().find(|arg| arg.trim() != "(%dx)" && addressed(arg, Width::Word))
    {
        return Err(format!(
            "'{}' is an address of sixteen bit registers, which this assembler does not write yet",
            arg.trim()
        ));
    }
    let mut written = sized(word, args, gcc)?;
    if args.iter().any(|arg| addressed(arg, Width::Long)) {
        crate::source::prefixed(&mut written, 0x67)?;
    }
    Ok(written)
}

/// [`one_in16`] before the address size prefix, which is the instruction with its operand size
/// prefix right and any address that is only a name cut down to two bytes.
fn sized(word: &str, args: &[String], gcc: bool) -> Result<Written, String> {
    if let Some(written) = stacked(word, args, gcc) {
        return written;
    }
    if let Some(written) = branch(word, args, gcc)? {
        return Ok(written);
    }
    if let Some(written) = segment(word, args, gcc) {
        return written;
    }
    let register = match args {
        [arg] => arg.trim().strip_prefix('%').and_then(gpr_named).map(|(_, width)| width),
        _ => None,
    };
    let word = match (word, register) {
        ("ljmp", _) => "ljmpw",
        ("lcall", _) => "lcallw",
        // A register says how wide it is, and anything else pushed in gcc's code is a word of its
        // thirty two bit stack.
        ("push", Some(Width::Long)) => "pushl",
        ("pop", Some(Width::Long)) => "popl",
        ("push", Some(Width::Word)) => "pushw",
        ("pop", Some(Width::Word)) => "popw",
        ("push", None) if gcc => "pushl",
        ("pop", None) if gcc => "popl",
        _ => word,
    };
    let mut written = one_in(word, args, Mode::Bits32)?;
    absolute(word, args, &mut written)?;
    // The four that load or store a table's place are always thirty two bits with the letter on,
    // and the four that widen into a register of thirty two bits say so with the last letter, which
    // the other width of the same line does not show, since there is no `movzww`.
    // `lea` is the same, for the other reason that the encoder has no sixteen bit row for it.
    let into_long = args
        .last()
        .and_then(|arg| arg.trim().strip_prefix('%').and_then(gpr_named))
        .is_some_and(|(_, width)| width == Width::Long);
    if matches!(
        word,
        "lgdtl" | "lidtl" | "sgdtl" | "sidtl" | "movzbl" | "movsbl" | "movzwl" | "movswl"
    ) || (matches!(word, "lea" | "leal") && into_long)
    {
        crate::source::prefixed(&mut written, 0x66)?;
        return Ok(written);
    }
    // Nothing that names a control or debug register has an operand size at all.
    if args.iter().any(|arg| {
        let arg = arg.trim();
        arg.starts_with("%cr") || arg.starts_with("%dr") || arg.starts_with("%db")
    }) {
        return Ok(written);
    }
    // Written with the prefix, the instruction is sixteen bits wide when it says so and the same
    // line at thirty two bits has no prefix. Written without, it is thirty two bits when the same
    // line at sixteen has one.
    let had = operand_prefix(&written.bytes);
    if let Some(other) = twin(word, args, if had { Width::Long } else { Width::Word }) {
        match (had, operand_prefix(&other.bytes)) {
            (false, true) => crate::source::prefixed(&mut written, 0x66)?,
            (true, false) => unprefixed(&mut written),
            _ => {}
        }
    }
    Ok(written)
}

/// The instructions that move the stack and take no width from their operands, written as the
/// thirty two bit instruction with the prefix or without it. Each is a row of the sixteen bit
/// spelling, the thirty two bit spelling and the one with no letter, whose width is the code's.
fn stacked(word: &str, args: &[String], gcc: bool) -> Option<Result<Written, String>> {
    const ROWS: [(&str, &str, &str, bool); 10] = [
        ("cbtw", "cwtl", "", false),
        ("cwtd", "cltd", "", false),
        ("iretw", "iretl", "iret", false),
        ("pushfw", "pushfl", "pushf", true),
        ("popfw", "popfl", "popf", true),
        ("retw", "retl", "ret", true),
        ("lretw", "lretl", "lret", false),
        ("leavew", "leavel", "leave", true),
        ("pushaw", "pushal", "pusha", true),
        ("popaw", "popal", "popa", true),
    ];
    let &(_, long, bare, stack) =
        ROWS.iter().find(|&&(short, long, bare, _)| [short, long, bare].contains(&word))?;
    let wide = word == long || (word == bare && gcc && stack);
    // `pusha` and `popa` are not instructions in sixty four bit mode, so the encoder has no row
    // for them, and they are one byte each.
    let mut written = match long {
        "pushal" => Ok(Written { bytes: vec![0x60], holes: Vec::new() }),
        "popal" => Ok(Written { bytes: vec![0x61], holes: Vec::new() }),
        _ => one_in(if bare.is_empty() { long } else { bare }, args, Mode::Bits32),
    };
    if let (true, Ok(written)) = (wide, &mut written) {
        if let Err(why) = crate::source::prefixed(written, 0x66) {
            return Some(Err(why));
        }
    }
    Some(written)
}

/// A push or pop of a segment register, which is one or two bytes of its own and not something
/// the encoder writes, since four of the six are not instructions in sixty four bit mode.
fn segment(word: &str, args: &[String], gcc: bool) -> Option<Result<Written, String>> {
    let [arg] = args else { return None };
    let (push, wide) = match word {
        "push" => (true, gcc),
        "pushw" => (true, false),
        "pushl" => (true, true),
        "pop" => (false, gcc),
        "popw" => (false, false),
        "popl" => (false, true),
        _ => return None,
    };
    let pushed: &[u8] = match arg.trim() {
        "%es" => &[0x06],
        "%cs" => &[0x0E],
        "%ss" => &[0x16],
        "%ds" => &[0x1E],
        "%fs" => &[0x0F, 0xA0],
        "%gs" => &[0x0F, 0xA8],
        _ => return None,
    };
    let mut bytes = pushed.to_vec();
    if !push {
        if bytes == [0x0E] {
            return Some(Err("'pop %cs' is not an instruction".to_owned()));
        }
        *bytes.last_mut()? += 1;
    }
    if wide {
        bytes.insert(0, 0x66);
    }
    Some(Ok(Written { bytes, holes: Vec::new() }))
}

/// A jump or call, whose distance is two bytes unless the instruction is the thirty two bit one,
/// which has the prefix and keeps four.
fn branch(word: &str, args: &[String], gcc: bool) -> Result<Option<Written>, String> {
    let wide = matches!(word, "calll" | "jmpl") || (gcc && word == "call");
    let plain = match word {
        "calll" | "callw" => "call",
        "jmpl" | "jmpw" => "jmp",
        _ => word,
    };
    if !(plain == "call" || plain.starts_with('j') || plain.starts_with("loop")) {
        return Ok(None);
    }
    let [arg] = args else { return Ok(None) };
    if let Some(target) = arg.trim().strip_prefix('*') {
        // Through a register or a word in memory, whose width is the register's or the letter's.
        let mut written = one_in(plain, args, Mode::Bits32)?;
        let target = target.trim();
        absolute(plain, args, &mut written)?;
        let long = target
            .strip_prefix('%')
            .and_then(gpr_named)
            .is_some_and(|(_, width)| width == Width::Long);
        if wide || long {
            crate::source::prefixed(&mut written, 0x66)?;
        }
        return Ok(Some(written));
    }
    let mut written = one_in(plain, args, Mode::Bits32)?;
    let relative = match written.bytes.as_slice() {
        [0xE8 | 0xE9, ..] => 1,
        [0x0F, 0x80..=0x8F, ..] => 2,
        _ => return Ok(Some(written)),
    };
    let fits = written.bytes.len() == relative + 4
        && matches!(written.holes.as_slice(), [hole] if hole.at == relative && hole.width == 4);
    if !fits {
        return Ok(Some(written));
    }
    if wide {
        crate::source::prefixed(&mut written, 0x66)?;
    } else {
        written.bytes.truncate(relative + 2);
        written.holes[0].width = 2;
    }
    Ok(Some(written))
}

/// The instruction written again at the width `to`, which is the one it is not, when there is
/// another width to write it at. The letter on the end and the registers are changed together
/// first, then the registers alone, then the letter alone, and the first of those the encoder takes
/// is the answer. Any immediate becomes `$0`.
fn twin(word: &str, args: &[String], to: Width) -> Option<Written> {
    let (from, letter, other) =
        if to == Width::Word { (Width::Long, 'l', 'w') } else { (Width::Word, 'w', 'l') };
    let lettered = word.strip_suffix(letter).map(|stem| format!("{stem}{other}"));
    let registers: Vec<String> = args
        .iter()
        .map(|arg| {
            let text = arg.trim();
            // An immediate too wide for sixteen bits is refused at sixteen, and only the prefix of
            // the other width matters, so any number does.
            if text.starts_with('$') {
                return "$0".to_owned();
            }
            let (star, rest) = match text.strip_prefix('*') {
                Some(rest) => ("*", rest),
                None => ("", text),
            };
            // The port of an `in` or an `out` is `%dx` at either width.
            if rest == "%dx" && (word.starts_with("in") || word.starts_with("out")) {
                return arg.clone();
            }
            rest.strip_prefix('%')
                .and_then(gpr_named)
                .filter(|&(_, width)| width == from)
                .and_then(|(reg, _)| gpr_name(reg, to))
                .map_or_else(|| arg.clone(), |name| format!("{star}%{name}"))
        })
        .collect();
    let immediate: Vec<String> = args
        .iter()
        .map(|arg| if arg.trim().starts_with('$') { "$0".to_owned() } else { arg.clone() })
        .collect();
    let mut tries: Vec<(&str, &[String])> = Vec::new();
    if let Some(lettered) = &lettered {
        tries.push((lettered, &registers));
    }
    if registers != immediate {
        tries.push((word, &registers));
    }
    if let Some(lettered) = &lettered {
        tries.push((lettered, &immediate));
    }
    tries.into_iter().find_map(|(word, args)| one_in(word, args, Mode::Bits32).ok())
}

/// Whether the bytes start with an operand size prefix among the others in front of the opcode.
fn operand_prefix(bytes: &[u8]) -> bool {
    bytes.iter().take_while(|&&byte| legacy(byte)).any(|&byte| byte == 0x66)
}

/// The instruction without the operand size prefix it has, every place in it moving back a byte.
fn unprefixed(written: &mut Written) {
    let Some(at) = written.bytes.iter().take_while(|&&byte| legacy(byte)).position(|&b| b == 0x66)
    else {
        return;
    };
    written.bytes.remove(at);
    for hole in &mut written.holes {
        hole.at -= 1;
    }
}

/// Whether the byte is a prefix that is not REX, which in thirty two bit code is `inc` and `dec`.
fn legacy(byte: u8) -> bool {
    matches!(byte, 0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x66 | 0x67 | 0xF0 | 0xF2 | 0xF3)
}

/// The name in an operand that is an address made of nothing but a name or a number, with the
/// segment and the star in front of it taken off, or nothing for any other operand.
fn bare(arg: &str) -> Option<&str> {
    let text = arg.trim();
    let text = text.strip_prefix('*').unwrap_or(text).trim();
    let text = match text.split_once(':') {
        Some((segment, rest)) if segment.starts_with('%') && !segment.contains('(') => rest.trim(),
        _ => text,
    };
    (!text.is_empty() && !text.starts_with('$') && !text.contains('%')).then_some(text)
}

/// The displacement of an address that is only a name or a number, cut from four bytes to two.
///
/// Where the four bytes are is found by writing the instruction again with a name in the place of
/// the address, since a number leaves no hole behind to find. The byte before them is either the
/// addressing byte, whose four byte form `mod 00 rm 101` becomes the two byte `mod 00 rm 110`, or
/// one of the four `mov` opcodes that carry the address after them with no addressing byte at all.
fn absolute(word: &str, args: &[String], written: &mut Written) -> Result<(), String> {
    const MARK: &str = "rucc.sixteen.address";
    let Some(which) = args.iter().position(|arg| bare(arg).is_some()) else { return Ok(()) };
    let text = args[which].trim();
    let name = bare(text).unwrap_or_default();
    let marked: Vec<String> = args
        .iter()
        .enumerate()
        .map(|(at, arg)| {
            if at == which {
                format!("{}{MARK}", &text[..text.len() - name.len()])
            } else {
                arg.clone()
            }
        })
        .collect();
    let Ok(probe) = one_in(word, &marked, Mode::Bits32) else { return Ok(()) };
    let Some(hole) = probe.holes.iter().find(|hole| hole.name == MARK && hole.width == 4) else {
        return Ok(());
    };
    let at = hole.at;
    if probe.bytes.len() != written.bytes.len() || at == 0 {
        return Ok(());
    }
    let before = written.bytes[at - 1];
    let moffs = matches!(before, 0xA0..=0xA3) && written.bytes[..at - 1].iter().all(|&b| legacy(b));
    if !moffs {
        if before & 0xC7 != 0x05 {
            return Ok(());
        }
        written.bytes[at - 1] = (before & 0x38) | 0x06;
    }
    match written.holes.iter_mut().find(|hole| hole.at == at) {
        Some(hole) => hole.width = 2,
        None => {
            if written.bytes[at + 2..at + 4] != [0, 0] {
                return Err(format!("'{name}' is an address past the first sixty four kilobytes"));
            }
        }
    }
    written.bytes.drain(at + 2..at + 4);
    for hole in &mut written.holes {
        if hole.at > at {
            hole.at -= 2;
        }
    }
    Ok(())
}

/// The bytes that fill a gap in sixteen bit code, which gas writes as `lea 0(%si), %si` and the
/// other instructions that do nothing in this mode, the longest five bytes.
pub(crate) fn nops(mut need: usize, out: &mut Vec<u8>) {
    const NOPS: [&[u8]; 5] = [
        &[0x90],
        &[0x89, 0xF6],
        &[0x8D, 0x74, 0x00],
        &[0x8D, 0xB4, 0x00, 0x00],
        &[0x2E, 0x8D, 0xB4, 0x00, 0x00],
    ];
    while need > 0 {
        let take = need.min(NOPS.len());
        out.extend_from_slice(NOPS[take - 1]);
        need -= take;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(line: &str, gcc: bool) -> String {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let args: Vec<String> =
            if rest.is_empty() { Vec::new() } else { rest.split(',').map(str::to_owned).collect() };
        let written = one_in16(word, &args, gcc).unwrap();
        written.bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn the_operand_size_prefix_is_turned_round() {
        for (line, bytes) in [
            ("movw $0x1000,%ax", "b80010"),
            ("movl $1,%eax", "66b801000000"),
            ("xorl %ecx,%ecx", "6631c9"),
            ("pushw %ds", "1e"),
            ("popl %gs", "660fa9"),
            ("push %ax", "50"),
            ("pushl $0", "666a00"),
            ("push $0", "6a00"),
            ("movl %cr0,%eax", "0f20c0"),
            ("movzwl %ax,%eax", "660fb7c0"),
            ("movzbl %al,%eax", "660fb6c0"),
            ("movzbw %al,%ax", "0fb6c0"),
            ("inc %ax", "40"),
            ("incl %eax", "6640"),
            ("movsl", "66a5"),
            ("lodsw", "ad"),
            ("pushfl", "669c"),
            ("pushf", "9c"),
            ("iret", "cf"),
            ("ret", "c3"),
            ("retl", "66c3"),
            ("lretl", "66cb"),
            ("outw %ax,(%dx)", "ef"),
            ("outw %ax,%dx", "ef"),
            ("outl %eax,%dx", "66ef"),
            ("call *b+4", "ff160000"),
            ("inl (%dx),%eax", "66ed"),
            ("cli", "fa"),
            ("ljmpl $0x10,$0x20", "66ea200000001000"),
            ("orl $0x60000000,%edx", "6681ca00000060"),
            ("andl $0x0ff00f00,%eax", "6625000ff00f"),
            ("movl 4(%esp),%eax", "67668b442404"),
            ("leal 4(%esp),%eax", "67668d442404"),
        ] {
            assert_eq!(hex(line, false), bytes, "{line}");
        }
    }

    #[test]
    fn code16gcc_has_a_thirty_two_bit_stack() {
        for (line, bytes) in [
            ("ret", "66c3"),
            ("pushl %eax", "6650"),
            ("push %eax", "6650"),
            ("call *%eax", "66ffd0"),
            ("leave", "66c9"),
            ("pushf", "669c"),
            ("push $0", "666a00"),
        ] {
            assert_eq!(hex(line, true), bytes, "{line}");
        }
    }

    #[test]
    fn an_address_that_is_only_a_name_is_two_bytes() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let written = one_in16("movl", &args(&["b", "%eax"]), false).unwrap();
        assert_eq!(written.bytes, [0x66, 0xA1, 0, 0]);
        assert_eq!((written.holes[0].at, written.holes[0].width), (2, 2));
        let written = one_in16("movl", &args(&["$5", "b"]), false).unwrap();
        assert_eq!(written.bytes, [0x66, 0xC7, 0x06, 0, 0, 5, 0, 0, 0]);
        assert_eq!((written.holes[0].at, written.holes[0].width), (3, 2));
        let written = one_in16("lidtl", &args(&["%cs:b"]), false).unwrap();
        assert_eq!(written.bytes, [0x2E, 0x66, 0x0F, 0x01, 0x1E, 0, 0]);
        let written = one_in16("lgdt", &args(&["b"]), false).unwrap();
        assert_eq!(written.bytes, [0x0F, 0x01, 0x16, 0, 0]);
    }

    #[test]
    fn a_jump_counts_in_two_bytes_and_a_long_call_in_four() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let written = one_in16("call", &args(&["a"]), false).unwrap();
        assert_eq!((written.bytes.len(), written.holes[0].width), (3, 2));
        let written = one_in16("jne", &args(&["a"]), false).unwrap();
        assert_eq!((written.bytes.len(), written.holes[0].width), (4, 2));
        let written = one_in16("calll", &args(&["a"]), false).unwrap();
        assert_eq!(written.bytes[..2], [0x66, 0xE8]);
        assert_eq!(written.holes[0].width, 4);
        let written = one_in16("call", &args(&["a"]), true).unwrap();
        assert_eq!(written.bytes[..2], [0x66, 0xE8]);
    }

    #[test]
    fn a_gap_is_filled_the_way_gas_fills_it() {
        let mut out = Vec::new();
        nops(14, &mut out);
        assert_eq!(out, [0x2E, 0x8D, 0xB4, 0, 0, 0x2E, 0x8D, 0xB4, 0, 0, 0x8D, 0xB4, 0, 0]);
    }
}
