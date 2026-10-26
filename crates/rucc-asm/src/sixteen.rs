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

use crate::instruction::{Written, addressed, number, one_in};
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
    if let Some(which) =
        args.iter().position(|arg| arg.trim() != "(%dx)" && addressed(arg, Width::Word))
    {
        return registers16(word, args, which, gcc);
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
        ("jmp", _) if args.len() == 2 => "ljmpw",
        // A far call pushes a return address, so in gcc's code it pushes one the width of its
        // thirty two bit stack, as gas does. A far jump pushes nothing and stays at sixteen.
        ("lcall", _) if gcc => "lcalll",
        ("lcall", _) => "lcallw",
        ("call", _) if args.len() == 2 && gcc => "lcalll",
        ("call", _) if args.len() == 2 => "lcallw",
        // A register says how wide it is, and anything else pushed in gcc's code is a word of its
        // thirty two bit stack.
        ("push", Some(Width::Long)) => "pushl",
        ("pop", Some(Width::Long)) => "popl",
        ("push", Some(Width::Word)) => "pushw",
        ("pop", Some(Width::Word)) => "popw",
        ("push", None) if gcc => "pushl",
        ("pop", None) if gcc => "popl",
        // Anything else is a word of the sixteen bit stack, and a number pushed is two bytes.
        ("push", None) => "pushw",
        ("pop", None) => "popw",
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

/// An instruction whose address is made of sixteen bit registers, `(%bx)` or `4(%bp,%si)`.
///
/// Those have an addressing byte of their own, with eight places a register pair can be and no
/// index byte, so the instruction is written with a name in the place of the address, which gives
/// the two byte `mod 00 rm 110` form, and that byte is then turned into the pair. The `mov` opcodes
/// that carry an address with no addressing byte become the ordinary `mov` with one.
fn registers16(word: &str, args: &[String], which: usize, gcc: bool) -> Result<Written, String> {
    const MARK: &str = "rucc.sixteen.registers";
    let text = args[which].trim();
    let cut = text.rfind('(').unwrap_or_default();
    let inside = &text[cut + 1..text.len() - 1];
    let parts: Vec<&str> = inside.split(',').map(str::trim).collect();
    let pair = match parts[..] {
        [base] => (base, ""),
        [base, index] | [base, index, "1"] => (base, index),
        _ => ("", "?"),
    };
    let rm: u8 = match pair {
        ("%bx", "%si") | ("%si", "%bx") => 0,
        ("%bx", "%di") | ("%di", "%bx") => 1,
        ("%bp", "%si") | ("%si", "%bp") => 2,
        ("%bp", "%di") | ("%di", "%bp") => 3,
        ("%si", "") | ("", "%si") => 4,
        ("%di", "") | ("", "%di") => 5,
        ("%bp", "") => 6,
        ("%bx", "") => 7,
        _ => return Err(format!("'{text}' is not one of the eight sixteen bit addresses")),
    };
    let front = &text[..cut];
    let star = if front.starts_with('*') { "*" } else { "" };
    let front = front.trim_start_matches('*').trim();
    let (segment, disp) = match front.split_once(':') {
        Some((segment, disp)) if segment.starts_with('%') => {
            (&front[..=segment.len()], disp.trim())
        }
        _ => ("", front),
    };
    let value = if disp.is_empty() { Some(0) } else { number(disp).ok() };
    let with = |place: &str| -> Vec<String> {
        let mut list = args.to_vec();
        list[which] = format!("{star}{segment}{place}");
        list
    };
    let probe = sized(word, &with(MARK), gcc)?;
    let mut written = if value.is_some() { probe.clone() } else { sized(word, &with(disp), gcc)? };
    let Some(mut at) = probe.holes.iter().find(|hole| hole.name == MARK).map(|hole| hole.at) else {
        return Err(format!("'{text}' is an address this instruction cannot have"));
    };
    if probe.bytes.len() != written.bytes.len() || at == 0 {
        return Err(format!("'{text}' is an address this instruction cannot have"));
    }
    let before = written.bytes[at - 1];
    if matches!(before, 0xA0..=0xA3) && written.bytes[..at - 1].iter().all(|&b| legacy(b)) {
        // `a0` to `a3` load and store the accumulator, which is the register field left at nought.
        written.bytes[at - 1] = [0x8A, 0x8B, 0x88, 0x89][usize::from(before - 0xA0)];
        written.bytes.insert(at, 0x06);
        for hole in &mut written.holes {
            if hole.at >= at {
                hole.at += 1;
            }
        }
        at += 1;
    }
    let field = written.bytes[at - 1];
    if field & 0xC7 != 0x06 {
        return Err(format!("'{text}' is an address this instruction cannot have"));
    }
    let (mode, keep) = match value {
        None => (0x80, 2),
        Some(0) if rm != 6 => (0x00, 0),
        Some(value) if (-128..=127).contains(&value) => (0x40, 1),
        Some(value) if (-32768..=65535).contains(&value) => (0x80, 2),
        Some(_) => return Err(format!("'{disp}' does not fit in a sixteen bit address")),
    };
    written.bytes[at - 1] = mode | (field & 0x38) | rm;
    if let Some(value) = value {
        written.holes.retain(|hole| hole.at != at);
        let bytes = (value as u16).to_le_bytes();
        written.bytes[at..at + 2].copy_from_slice(&bytes);
    }
    written.bytes.drain(at + keep..at + 2);
    for hole in &mut written.holes {
        if hole.at > at {
            hole.at -= 2 - keep;
        }
    }
    Ok(written)
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
        // The commas between operands, and not the ones inside the brackets of an address.
        let mut args: Vec<String> = Vec::new();
        let mut depth = 0;
        for ch in rest.chars() {
            match ch {
                ',' if depth == 0 => args.push(String::new()),
                _ => {
                    depth += i32::from(ch == '(') - i32::from(ch == ')');
                    match args.last_mut() {
                        Some(arg) => arg.push(ch),
                        None => args.push(ch.to_string()),
                    }
                }
            }
        }
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
            ("lcall $0x10,$0x20", "9a20001000"),
            ("jmp $0x10,$0x20", "ea20001000"),
            ("call $0x10,$0x20", "9a20001000"),
            ("lcallw *(%esi)", "67ff1e"),
            ("lcalll *(%esi)", "6766ff1e"),
            ("orl $0x60000000,%edx", "6681ca00000060"),
            ("andl $0x0ff00f00,%eax", "6625000ff00f"),
            ("movl 4(%esp),%eax", "67668b442404"),
            ("leal 4(%esp),%eax", "67668d442404"),
            ("smsw (%bx)", "0f0127"),
            ("sidt (%bx)", "0f010f"),
            ("movw (%bp),%ax", "8b4600"),
            ("movw %ax,-2(%bp,%si)", "8942fe"),
            ("movl 0x1234(%bx,%di),%eax", "668b813412"),
            ("movw %es:(%si),%ax", "268b04"),
            ("movw (%di),%ax", "8b05"),
            ("addb $1,(%bx)", "800701"),
            ("lcallw *(%bx)", "ff1f"),
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
            ("lcall $0x10,$0x20", "669a200000001000"),
            ("lcall *(%esi)", "6766ff1e"),
            ("lcallw *(%esi)", "67ff1e"),
            ("ljmp $0x10,$0x20", "ea20001000"),
            ("call $0x10,$0x20", "669a200000001000"),
            ("jmp $0x10,$0x20", "ea20001000"),
            ("ljmpw *(%esi)", "67ff2e"),
        ] {
            assert_eq!(hex(line, true), bytes, "{line}");
        }
    }

    #[test]
    fn a_name_pushed_is_a_word_as_gas_writes_it() {
        for word in ["pushw", "push"] {
            let written = one_in16(word, &["$later".to_owned()], false).unwrap();
            assert_eq!(written.bytes, [0x68, 0, 0], "{word}");
            assert_eq!((written.holes[0].at, written.holes[0].width), (1, 2), "{word}");
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
    fn a_name_beside_sixteen_bit_registers_is_two_bytes() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let written = one_in16("movw", &args(&["b+2(%bx)", "%ax"]), false).unwrap();
        assert_eq!(written.bytes, [0x8B, 0x87, 0, 0]);
        assert_eq!((written.holes[0].at, written.holes[0].width), (2, 2));
        assert!(one_in16("movw", &args(&["(%ax)", "%bx"]), false).is_err());
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
