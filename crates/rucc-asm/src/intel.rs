//! Reading an x86 instruction written in Intel syntax, by writing it again in AT&T syntax.
//!
//! gas reads both, and `.intel_syntax` switches between them in the middle of a file. A C file
//! reaches this through an `asm` statement that switches for its own template and back with
//! `.att_syntax` at the end, which is what bcachefs does for its key comparison. So the rest of
//! the assembler reads one syntax, and a line in the other is turned round before it gets there.
//!
//! What is turned round is the part of Intel syntax a hand written template uses: registers with
//! or without their sigil, numbers, names, addresses in brackets with a base, an index, a scale
//! and a displacement, a segment in front, and `byte ptr` and the like where nothing else says
//! how wide an access is. The operands come in the other order.

use crate::instruction::named_register;

/// The instruction in AT&T syntax, as a mnemonic and its operands in AT&T order.
pub(crate) fn att(word: &str, args: &[String]) -> Result<(String, Vec<String>), String> {
    let word = word.to_ascii_lowercase();
    let jump = is_jump(&word);
    let mut width = None;
    let mut any_register = false;
    let mut out = Vec::with_capacity(args.len());
    for arg in args.iter().rev() {
        let (text, decoration) = decorated(arg.trim());
        let (text, stated) = sized(text);
        if stated.is_some() {
            width = stated;
        }
        let operand = operand(text, jump)?;
        any_register |= sigiled(text).is_some();
        out.push(operand + &decoration);
    }
    let word = match word.as_str() {
        // The two widening moves carry both widths in the AT&T name, the source's first.
        "movzx" | "movsx" => {
            let destination = args.first().and_then(|arg| sigiled(arg.trim())).flatten();
            let source =
                width.or_else(|| args.get(1).and_then(|arg| sigiled(arg.trim())).flatten());
            match (source, destination) {
                (Some(source), Some(destination)) => {
                    format!("mov{}{source}{destination}", if word == "movzx" { 'z' } else { 's' })
                }
                _ => return Err(format!("'{word}' needs the widths of both its operands")),
            }
        }
        "movsxd" => "movslq".to_owned(),
        // A width only needs saying when no register says it. A vector register says it as
        // well, so `movd xmm0, dword ptr [rsp]` is `movd` and not `movdl`.
        _ if !any_register && !jump => match width {
            Some(letter) => format!("{word}{letter}"),
            None => word,
        },
        _ => word,
    };
    Ok((word, out))
}

/// Whether the instruction's operand is somewhere to go rather than something to read.
fn is_jump(word: &str) -> bool {
    word == "call" || word == "jmp" || word.starts_with("loop") || word.starts_with('j')
}

/// The operand without a `byte ptr` in front, and the letter that says. The widths of a vector
/// have no letter, since the vector register the instruction also names says the width.
fn sized(text: &str) -> (&str, Option<char>) {
    let lower = text.to_ascii_lowercase();
    for (name, letter) in [
        ("byte", Some('b')),
        ("word", Some('w')),
        ("dword", Some('l')),
        ("qword", Some('q')),
        ("xmmword", None),
        ("ymmword", None),
        ("zmmword", None),
    ] {
        let Some(rest) = lower.strip_prefix(name) else { continue };
        let Some(after) = rest.trim_start().strip_prefix("ptr") else { continue };
        let cut = text.len() - after.len();
        return (text[cut..].trim(), letter);
    }
    (text, None)
}

/// The operand without the AVX-512 marks in braces after it, and the marks as AT&T writes them.
/// A mask register gets its sigil, and a zeroing or a broadcast such as `{1to16}` stays as it is.
/// Both go after the operand in AT&T as well, so `zmm2 {k1}` is `%zmm2{%k1}` and
/// `dword ptr [rax] {1to16}` is `(%rax){1to16}`.
fn decorated(text: &str) -> (&str, String) {
    let mut rest = text;
    let mut marks = Vec::new();
    while let Some(inner) = rest.strip_suffix('}') {
        let Some(open) = inner.rfind('{') else { break };
        let mark = inner[open + 1..].trim();
        let bare = mark.strip_prefix('%').unwrap_or(mark);
        marks.push(if named_register(bare).is_some() {
            format!("{{%{bare}}}")
        } else {
            format!("{{{mark}}}")
        });
        rest = inner[..open].trim_end();
    }
    marks.reverse();
    (rest, marks.concat())
}

/// Whether the operand is a register, and its width letter if it has one.
fn sigiled(text: &str) -> Option<Option<char>> {
    named_register(text.strip_prefix('%').unwrap_or(text))
}

fn operand(text: &str, jump: bool) -> Result<String, String> {
    let bare = text.strip_prefix('%').unwrap_or(text);
    if named_register(bare).is_some() {
        return Ok(if jump { format!("*%{bare}") } else { format!("%{bare}") });
    }
    let lower = text.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("offset ") {
        let cut = text.len() - rest.len();
        return Ok(format!("${}", text[cut..].trim()));
    }
    if text.contains('[') {
        let address = address(text)?;
        return Ok(if jump { format!("*{address}") } else { address });
    }
    // A segment and a number with no brackets, as in `fs:0`.
    if let Some((segment, rest)) = text.split_once(':') {
        if named_register(segment.trim().strip_prefix('%').unwrap_or(segment.trim())).is_some() {
            return Ok(format!("%{}:{}", segment.trim().trim_start_matches('%'), rest.trim()));
        }
    }
    // A number is a number and a name is the memory at it, except that where the instruction
    // goes somewhere, a name is the place it goes.
    let number = text.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '~' | '('));
    if jump || !number { Ok(text.to_owned()) } else { Ok(format!("${text}")) }
}

/// `seg:disp[base + index*scale + disp]`, as `%seg:disp(%base,%index,scale)`.
fn address(text: &str) -> Result<String, String> {
    let open = text.find('[').unwrap_or_default();
    let close = text.rfind(']').ok_or_else(|| format!("'{text}' has no ']'"))?;
    let mut outside = text[..open].trim().to_owned();
    let mut segment = None;
    if let Some((name, rest)) = outside.split_once(':') {
        segment = Some(name.trim().trim_start_matches('%').to_owned());
        outside = rest.trim().to_owned();
    }
    let mut inside = text[open + 1..close].trim();
    if let Some((name, rest)) = inside.split_once(':') {
        if named_register(name.trim().trim_start_matches('%')).is_some() {
            segment = Some(name.trim().trim_start_matches('%').to_owned());
            inside = rest.trim();
        }
    }
    let (mut base, mut index, mut scale) = (None, None, None);
    let mut displacement = outside;
    for (negative, term) in terms(inside) {
        let term = term.trim();
        let register = |name: &str| {
            let name = name.trim().trim_start_matches('%');
            let known = named_register(name).is_some() || matches!(name, "rip" | "eip");
            known.then(|| name.to_owned())
        };
        // A product of two numbers, as in `[rsp+0x1*0x10]`, is part of the displacement below.
        if let Some((left, right)) = term.split_once('*') {
            let scaled = match (register(left), register(right)) {
                (Some(name), _) => Some((name, right.trim())),
                (None, Some(name)) => Some((name, left.trim())),
                (None, None) => None,
            };
            if let Some((name, times)) = scaled {
                index = Some(name);
                scale = Some(times.to_owned());
                continue;
            }
        }
        if let Some(name) = register(term) {
            if negative {
                return Err(format!("'{text}' takes a register away"));
            }
            if base.is_none() {
                base = Some(name);
            } else {
                index = Some(name);
            }
            continue;
        }
        if displacement.is_empty() {
            displacement = if negative { format!("-{term}") } else { term.to_owned() };
        } else {
            displacement = format!("{displacement}{}{term}", if negative { '-' } else { '+' });
        }
    }
    let mut out = String::new();
    if let Some(segment) = segment {
        out.push_str(&format!("%{segment}:"));
    }
    out.push_str(&displacement);
    if base.is_some() || index.is_some() {
        out.push('(');
        if let Some(base) = base {
            out.push_str(&format!("%{base}"));
        }
        if let Some(index) = index {
            out.push_str(&format!(",%{index}"));
            if let Some(scale) = scale {
                out.push_str(&format!(",{scale}"));
            }
        }
        out.push(')');
    } else if out.ends_with(':') || out.is_empty() {
        out.push('0');
    }
    Ok(out)
}

/// The terms of a sum, each with whether it is taken away.
fn terms(text: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let mut negative = false;
    for (at, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            '+' | '-' if depth == 0 => {
                let term = text[start..at].trim();
                if !term.is_empty() {
                    out.push((negative, term));
                }
                negative = c == '-';
                start = at + 1;
            }
            _ => {}
        }
    }
    let term = text[start..].trim();
    if !term.is_empty() {
        out.push((negative, term));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(word: &str, args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let (word, args) = att(word, &args).expect("the line reads");
        if args.is_empty() { word } else { format!("{word} {}", args.join(", ")) }
    }

    #[test]
    fn registers_and_numbers_come_in_the_other_order() {
        assert_eq!(line("xor", &["eax", "eax"]), "xor %eax, %eax");
        assert_eq!(line("sub", &["ecx", "64"]), "sub $64, %ecx");
        assert_eq!(line("shr", &["r8", "cl"]), "shr %cl, %r8");
        assert_eq!(line("mov", &["%rax", "%rbx"]), "mov %rbx, %rax");
        assert_eq!(line("mov", &["eax", "offset n"]), "mov $n, %eax");
        assert_eq!(line("seta", &["al"]), "seta %al");
    }

    #[test]
    fn an_address_in_brackets_is_a_displacement_and_registers() {
        assert_eq!(line("mov", &["r8", "[rdi]"]), "mov (%rdi), %r8");
        assert_eq!(line("lea", &["rdi", "[rdi - 8]"]), "lea -8(%rdi), %rdi");
        assert_eq!(line("mov", &["eax", "[rdi + rsi*4 + 16]"]), "mov 16(%rdi,%rsi,4), %eax");
        assert_eq!(line("mov", &["eax", "8[rbp]"]), "mov 8(%rbp), %eax");
        assert_eq!(line("mov", &["rax", "fs:[0]"]), "mov %fs:0, %rax");
        assert_eq!(line("mov", &["rax", "[rip + n]"]), "mov n(%rip), %rax");
    }

    #[test]
    fn a_width_is_said_only_where_no_register_says_it() {
        assert_eq!(line("inc", &["dword ptr [rax]"]), "incl (%rax)");
        assert_eq!(line("mov", &["byte ptr [rax]", "1"]), "movb $1, (%rax)");
        assert_eq!(line("mov", &["qword ptr [rax]", "rbx"]), "mov %rbx, (%rax)");
        assert_eq!(line("movzx", &["eax", "byte ptr [rdi]"]), "movzbl (%rdi), %eax");
        assert_eq!(line("movsx", &["rax", "cx"]), "movswq %cx, %rax");
    }

    #[test]
    fn a_jump_goes_to_a_label_or_through_a_register() {
        assert_eq!(line("jl", &["2f"]), "jl 2f");
        assert_eq!(line("jmp", &["1b"]), "jmp 1b");
        assert_eq!(line("call", &["rax"]), "call *%rax");
        assert_eq!(line("jmp", &["qword ptr [rax + 8]"]), "jmp *8(%rax)");
    }

    /// Lines of the BLAKE3 code in OpenZFS, which FreeBSD's kernel builds. GNU as 2.42 writes the
    /// same bytes for each AT&T line as for the Intel line it comes from.
    #[test]
    fn the_vector_lines_of_blake3_read_as_att() {
        assert_eq!(
            line("vpand", &["ymm1", "ymm0", "ymmword ptr [ADD0+rip]"]),
            "vpand ADD0(%rip), %ymm0, %ymm1"
        );
        assert_eq!(
            line("vmovdqa", &["ymmword ptr [rsp+0x280]", "ymm0"]),
            "vmovdqa %ymm0, 0x280(%rsp)"
        );
        assert_eq!(line("movd", &["xmm13", "dword ptr [rsp+0x110]"]), "movd 0x110(%rsp), %xmm13");
        assert_eq!(
            line("pinsrd", &["xmm13", "dword ptr [BLAKE3_BLOCK_LEN+rip]", "2"]),
            "pinsrd $2, BLAKE3_BLOCK_LEN(%rip), %xmm13"
        );
        assert_eq!(
            line("vmovdqa32", &["xmm0 {k1}", "xmmword ptr [rsp+0x1*0x10]"]),
            "vmovdqa32 0x1*0x10(%rsp), %xmm0{%k1}"
        );
        assert_eq!(
            line("vpaddd", &["zmm2{k1}", "zmm0", "dword ptr [ADD16+rip] {1to16}"]),
            "vpaddd ADD16(%rip){1to16}, %zmm0, %zmm2{%k1}"
        );
        assert_eq!(
            line("vinserti32x4", &["zmm8", "zmm8", "xmmword ptr [r10+rdx-0x2*0x10]", "0x02"]),
            "vinserti32x4 $0x02, -0x2*0x10(%r10,%rdx), %zmm8, %zmm8"
        );
        assert_eq!(
            line("mov", &["r10d", "dword ptr [rsp+0x110+8*rax]"]),
            "mov 0x110(%rsp,%rax,8), %r10d"
        );
        assert_eq!(line("cmp", &["byte ptr [rax]", "0"]), "cmpb $0, (%rax)");
    }
}
