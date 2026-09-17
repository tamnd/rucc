//! One line of a file of assembly that is an instruction, as the bytes of one.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1.
//!
//! [`crate::source`] is the other half of the same reader and does the directives and the labels.
//! This does the lines between them, and it is a smaller file than it looks like it should be for
//! the reason section 11.1 gives: there is one description of this machine and it is in
//! `rucc-target`, so nothing here knows what an instruction is. `rucc_target::x86_64::encode` is
//! already told a mnemonic and a list of arguments and writes the bytes, because that is what the
//! compiler's own output goes through, and what is missing in front of it is the reading. So this
//! file is the operand syntax and nothing else: an AT&T operand turned into one of the values that
//! encoder takes, and a mnemonic with the width letter worked out when the text left it off.
//!
//! That is deliberate rather than convenient. A second table of instructions would be a second
//! description of the machine, and the two would disagree, and the way you would find out is an
//! object file that runs differently from the listing beside it.
//!
//! # What an instruction refers to that is not in it
//!
//! A jump goes to a label, and a label further down the file has no place yet. An address written
//! `message(%rip)` names something that may be in another object entirely. Both are four bytes the
//! encoder leaves empty and says where it left, and both come back from here as a [`Hole`] for the
//! caller to fill in or to turn into a relocation, because which of those it is depends on what the
//! name turns out to be and the whole file has to be read before that is known.
//!
//! # What is not read yet
//!
//! A branch is four bytes of distance whether it needs them or not. Choosing the two byte form
//! where it fits is relaxation, which is a pass over the whole section rather than a decision one
//! instruction makes, and until it is written the bytes are correct and longer than gas would have
//! written. A numbered local label, `1:` and `1b` and `1f`, is not read. A symbol as an immediate
//! is not read, nor a symbol as the displacement of an address that names a register, because both
//! want a relocation this does not write yet and a wrong guess about either is silent.

use rucc_target::x86_64::{Addr, Encoding, ImmSize, Value, Width, encode, encoding, gpr_named};
use rucc_target::{PhysReg, Segment};

/// Four bytes of an instruction whose value is not known while the instruction is being written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hole {
    /// Where it starts, counted from the start of the instruction.
    pub at: usize,
    /// How many bytes, which is four for everything this writes.
    pub width: u8,
    /// The name that goes there, as the source spelled it.
    pub name: String,
    /// What the linker is being asked for, which the shape of the instruction decides.
    pub sort: Sort,
}

/// Which of the three things a hole is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sort {
    /// Somewhere to jump or call, which a linker may satisfy with a stub that reaches further than
    /// four bytes would.
    Branch,
    /// A datum reached from the instruction pointer, which is what `message(%rip)` is.
    Near,
}

/// One instruction, written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Written {
    /// Its bytes.
    pub bytes: Vec<u8>,
    /// Every place in them that names something outside the instruction.
    pub holes: Vec<Hole>,
}

/// One operand, read off the text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Operand {
    /// A general purpose register and how much of it the name says.
    Reg(PhysReg, Width),
    /// The byte above the low byte of one of the first four registers.
    High(PhysReg),
    /// A vector register.
    Xmm(PhysReg),
    /// An address, and the name in its displacement when it has one.
    Mem(Addr, Option<String>),
    /// A number the instruction carries.
    Imm(i64),
    /// A name to jump or call to.
    Dest(String),
}

/// That instruction, as bytes.
///
/// `word` is the mnemonic as the source spelled it and `args` are its operands in the order it
/// wrote them, which is AT&T order and is the order the encoder wants them in.
///
/// # Errors
///
/// A sentence saying what about the line could not be read, with no line number on it, since the
/// caller is the one that knows which line this was.
pub(crate) fn one(word: &str, args: &[String]) -> Result<Written, String> {
    let operands: Vec<Operand> =
        args.iter().map(|arg| operand(arg.trim())).collect::<Result<_, _>>()?;
    let values: Vec<Value> = operands.iter().map(value).collect();
    let (mnemonic, row) = spelled(word, &operands, &values)?;

    let mut bytes = Vec::with_capacity(16);
    let holes = encode(&mnemonic, &values, &mut bytes).map_err(|why| why.to_string())?;

    let mut wanted = Vec::new();
    if let Some(at) = holes.dest {
        let Some(Operand::Dest(name)) = operands.iter().find(|op| matches!(op, Operand::Dest(_)))
        else {
            return Err(format!("'{word}' left room for somewhere to go and was given nowhere"));
        };
        // How much room the instruction left, which is four for every branch but one. `jrcxz` has
        // a single byte and no longer form, so a destination further away than that is a mistake
        // in the file rather than something to relax, and the caller is the one that can tell.
        let width = match row.imm {
            ImmSize::Cb => 1,
            _ => 4,
        };
        wanted.push(Hole { at, width, name: name.clone(), sort: Sort::Branch });
    }
    if let Some(at) = holes.rip {
        // Only when the source put a name there. `8(%rip)` is a number the machine counts from the
        // end of the instruction and there is nothing for a linker to do about it.
        let named = operands.iter().find_map(|op| match op {
            Operand::Mem(_, Some(name)) => Some(name.clone()),
            _ => None,
        });
        if let Some(name) = named {
            wanted.push(Hole { at, width: 4, name, sort: Sort::Near });
        }
    }
    Ok(Written { bytes, holes: wanted })
}

/// The mnemonic with the width letter on it that the encoder knows this instruction by.
///
/// AT&T puts the width on the end of the mnemonic and lets a program leave it off when the
/// operands say it anyway, so `mov %rdi, %rax` is `movq` and `add $1, %eax` is `addl`. What the
/// program wrote is tried first, because a mnemonic that is already complete must not have a
/// letter glued onto it, and because two of them end in a letter that is also a width: `seta` is
/// not `set` at some width and neither is `cmovb`.
///
/// The letter is worked out last of all, after the spellings that do not need one, because a
/// mnemonic that already carries its width is allowed to name two widths of register at once and
/// `movzbl %cl, %edx` is exactly that. Asking the operands how wide they are would refuse it.
fn spelled(
    word: &str,
    operands: &[Operand],
    values: &[Value],
) -> Result<(String, &'static Encoding), String> {
    let kinds: Vec<_> = values.iter().map(|value| value.kind()).collect();
    let imm = values
        .iter()
        .find_map(|value| match value {
            Value::Imm(number) => Some(*number),
            _ => None,
        })
        .unwrap_or(0);
    let names = [Some(word.to_owned()), aliased(word)];
    for name in names.iter().flatten() {
        if let Some(row) = encoding(name, &kinds, imm) {
            return Ok((name.clone(), row));
        }
    }
    if let Some(width) = stated(operands)? {
        let letter = match width {
            Width::Byte => 'b',
            Width::Word => 'w',
            Width::Long => 'l',
            Width::Quad => 'q',
        };
        for name in names.iter().flatten() {
            let spelled = format!("{name}{letter}");
            if let Some(row) = encoding(&spelled, &kinds, imm) {
                return Ok((spelled, row));
            }
        }
    }
    // Said in terms of what the program wrote rather than in terms of the letter or the other
    // spelling this went looking for, because those are this file's business and the line is
    // theirs.
    Err(format!(
        "'{word}' with {} of those operands is not an instruction this compiler writes yet",
        kinds.len()
    ))
}

/// The two names of each condition a program can branch on, the other one first.
///
/// The machine has sixteen conditions and the assembly language has about thirty names for them,
/// because most of them are worth saying two ways round: the bit a subtraction leaves is the carry
/// when you are doing arithmetic and it is below when you are comparing, and `jc` and `jb` are one
/// instruction under two names because those are one question under two readings. gas takes every
/// name and so does every file written by hand, which is where `jnz` and `setc` come from.
const CONDITIONS: &[(&str, &str)] = &[
    ("z", "e"),
    ("nz", "ne"),
    ("c", "b"),
    ("nc", "ae"),
    ("nae", "b"),
    ("nb", "ae"),
    ("na", "be"),
    ("nbe", "a"),
    ("ng", "le"),
    ("nge", "l"),
    ("nl", "ge"),
    ("nle", "g"),
    ("pe", "p"),
    ("po", "np"),
];

/// The same instruction under the name the encoder knows the condition by, when there is one.
///
/// Three kinds of instruction read a condition and all three spell it the same way, so the name is
/// a prefix and a condition and, for a conditional move, a width letter behind it. Both readings of
/// the tail are tried because `jnb` ends in a letter that is also a width and is not one.
fn aliased(word: &str) -> Option<String> {
    let (prefix, rest) = ["cmov", "set", "j"]
        .iter()
        .find_map(|prefix| word.strip_prefix(prefix).map(|rest| (*prefix, rest)))?;
    let mut tails = vec![(rest, "")];
    if rest.len() > 1 && matches!(&rest[rest.len() - 1..], "b" | "w" | "l" | "q") {
        tails.push((&rest[..rest.len() - 1], &rest[rest.len() - 1..]));
    }
    tails.into_iter().find_map(|(condition, tail)| {
        let (_, known) = CONDITIONS.iter().find(|(written, _)| *written == condition)?;
        Some(format!("{prefix}{known}{tail}"))
    })
}

/// How wide the operands say the instruction is, when they say.
///
/// Every register named has to agree, since two that disagree are either a mnemonic that already
/// carries its width, which was tried before this, or a line no assembler would take.
fn stated(operands: &[Operand]) -> Result<Option<Width>, String> {
    let mut width = None;
    for operand in operands {
        let said = match operand {
            Operand::Reg(_, width) => *width,
            // A byte and no wider, and the mnemonic that names one never needs a letter, so this
            // is here to make a line that mixes it with a wider register say so.
            Operand::High(_) => Width::Byte,
            _ => continue,
        };
        match width {
            None => width = Some(said),
            Some(before) if before == said => {}
            Some(before) => {
                return Err(format!(
                    "the operands are {} bits and {} bits, so the instruction does not say how \
                     wide it is",
                    before.bits(),
                    said.bits()
                ));
            }
        }
    }
    Ok(width)
}

/// What the encoder is handed for one of these.
fn value(operand: &Operand) -> Value {
    match operand {
        Operand::Reg(reg, width) => Value::Reg(*reg, *width),
        Operand::High(reg) => Value::High(*reg),
        Operand::Xmm(reg) => Value::Xmm(*reg),
        Operand::Mem(addr, _) => Value::Mem(*addr),
        Operand::Imm(number) => Value::Imm(*number),
        Operand::Dest(_) => Value::Dest,
    }
}

/// One operand, read.
fn operand(text: &str) -> Result<Operand, String> {
    if text.is_empty() {
        return Err("an operand with nothing in it".to_owned());
    }
    // A jump or a call through a register or through memory, which is the same operand as any
    // other and a different instruction from a jump to a name. The star is how AT&T says which.
    if let Some(rest) = text.strip_prefix('*') {
        return match operand(rest.trim())? {
            it @ (Operand::Reg(_, _) | Operand::Mem(_, _)) => Ok(it),
            _ => Err(format!("'{text}' goes through something that is not a place")),
        };
    }
    if let Some(rest) = text.strip_prefix('$') {
        return Ok(Operand::Imm(number(rest.trim())?));
    }
    if text.starts_with('%') && !text.contains('(') && !text.contains(':') {
        return register(&text[1..]);
    }
    if text.starts_with('%') || text.contains('(') {
        return address(text);
    }
    // What is left is a bare name, which in an instruction is somewhere to go. An address written
    // as a bare name is refused inside `address` rather than here, so that the message is about
    // the address rather than about a jump the line never was.
    if text.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | '$' | '@')) {
        return Ok(Operand::Dest(text.to_owned()));
    }
    Err(format!("'{text}' is not an operand this compiler reads"))
}

/// A register, without its sigil.
fn register(name: &str) -> Result<Operand, String> {
    if let Some((reg, width)) = gpr_named(name) {
        return Ok(Operand::Reg(reg, width));
    }
    // Numbered like `spl` and told apart from it by the instruction having no REX byte, which is
    // why the encoder has a value of its own for one.
    if let Some(number) = ["ah", "ch", "dh", "bh"].iter().position(|&known| known == name) {
        return Ok(Operand::High(PhysReg::new(number as u8)));
    }
    if let Some(rest) = name.strip_prefix("xmm") {
        if let Ok(number) = rest.parse::<u8>() {
            if number < 16 {
                return Ok(Operand::Xmm(PhysReg::new(number)));
            }
        }
    }
    Err(format!("'%{name}' is not a register this compiler has"))
}

/// An address, which is a displacement and up to three things in brackets.
///
/// `segment:displacement(base, index, scale)`, with any of them left out, and the displacement
/// either a number or `%rip`, which is what makes an address a distance from the end of the
/// instruction rather than a place a register points at.
fn address(text: &str) -> Result<Operand, String> {
    let mut rest = text;
    let mut addr = Addr { scale: 1, ..Addr::default() };

    if let Some(cut) = rest.find(':') {
        let name = rest[..cut].trim();
        addr.segment = Some(match name {
            "%fs" => Segment::Fs,
            "%gs" => Segment::Gs,
            _ => return Err(format!("'{name}' is not a segment this machine reaches through")),
        });
        rest = rest[cut + 1..].trim();
    }

    let (front, inside) = match rest.find('(') {
        Some(cut) => {
            let Some(end) = rest.rfind(')') else {
                return Err(format!("'{text}' opens a bracket and does not close it"));
            };
            if end < cut || rest[end + 1..].trim() != "" {
                return Err(format!("'{text}' is not an address this compiler reads"));
            }
            (rest[..cut].trim(), Some(rest[cut + 1..end].trim()))
        }
        None => (rest.trim(), None),
    };

    // The displacement, which is a number when the file says one and a name when it names one. A
    // name is only read in front of `(%rip)`, since every other shape of address wants a
    // relocation against a place rather than against a distance and this does not write one.
    let mut named = None;
    if !front.is_empty() {
        match number(front) {
            Ok(value) => {
                addr.disp = i32::try_from(value).map_err(|_| {
                    format!("'{front}' does not fit in the four bytes of an address")
                })?;
            }
            Err(_) => named = Some(front.to_owned()),
        }
    }

    let parts: Vec<&str> = inside.map_or_else(Vec::new, |inside| {
        if inside.is_empty() { Vec::new() } else { inside.split(',').map(str::trim).collect() }
    });
    if parts.len() > 3 {
        return Err(format!("'{text}' has more than a base, an index and a scale in it"));
    }
    if let Some(base) = parts.first().filter(|base| !base.is_empty()) {
        if *base == "%rip" {
            addr.rip = true;
        } else {
            addr.base = Some(whole(base)?);
        }
    }
    if let Some(index) = parts.get(1).filter(|index| !index.is_empty()) {
        addr.index = Some(whole(index)?);
    }
    if let Some(scale) = parts.get(2).filter(|scale| !scale.is_empty()) {
        let by = number(scale)?;
        if !matches!(by, 1 | 2 | 4 | 8) {
            return Err(format!("{by} is not a scale this machine has"));
        }
        addr.scale = u8::try_from(by).unwrap_or(1);
    }

    if named.is_some() && !addr.rip {
        return Err(format!(
            "'{text}' names something in an address that is not counted from the instruction, \
             which wants a relocation this compiler does not write yet"
        ));
    }
    if !addr.rip && addr.base.is_none() && addr.index.is_none() && named.is_none() {
        // A bare number in brackets is an address the machine holds outright, which is legal and
        // is not what a file writing one usually means, so it goes through rather than being
        // guessed at. What is refused above this is a bare name, which is the one that would need
        // a relocation.
    }
    Ok(Operand::Mem(addr, named))
}

/// A register inside the brackets of an address, which has to be a whole one.
fn whole(text: &str) -> Result<PhysReg, String> {
    let Some(name) = text.strip_prefix('%') else {
        return Err(format!("'{text}' is not a register"));
    };
    match gpr_named(name) {
        Some((reg, Width::Quad)) => Ok(reg),
        Some((_, width)) => Err(format!(
            "'%{name}' is {} bits, and an address on this machine is made of whole registers",
            width.bits()
        )),
        None => Err(format!("'%{name}' is not a register this compiler has")),
    }
}

/// A number written the way an assembler writes one.
fn number(text: &str) -> Result<i64, String> {
    let text = text.trim();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1i64, rest.trim()),
        None => (1, text.strip_prefix('+').map_or(text, str::trim)),
    };
    let value = if let Some(hex) = digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
        // As unsigned first, because a file writes a sixty four bit mask as a positive hex number
        // and that number is negative when it is read as signed, which is the same bits.
        u64::from_str_radix(hex, 16).map(|value| value as i64)
    } else if digits.len() > 1 && digits.starts_with('0') {
        i64::from_str_radix(&digits[1..], 8)
    } else {
        digits.parse::<i64>()
    };
    value.map(|value| sign.wrapping_mul(value)).map_err(|_| format!("'{text}' is not a number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One instruction, as the bytes of it.
    fn bytes(line: &str) -> Vec<u8> {
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let args: Vec<String> =
            if rest.trim().is_empty() { Vec::new() } else { crate::source::split(rest, ',') };
        match one(word, &args) {
            Ok(written) => {
                assert!(written.holes.is_empty(), "this one names something: {:?}", written.holes);
                written.bytes
            }
            Err(why) => panic!("{line}: {why}"),
        }
    }

    /// What a line this could not read said about it.
    fn refused(line: &str) -> String {
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let args: Vec<String> =
            if rest.trim().is_empty() { Vec::new() } else { crate::source::split(rest, ',') };
        one(word, &args)
            .err()
            .unwrap_or_else(|| panic!("'{line}' was read and should not have been"))
    }

    #[test]
    fn a_move_between_registers() {
        assert_eq!(bytes("movq %rdi, %rax"), vec![0x48, 0x89, 0xf8]);
        assert_eq!(bytes("movl %edi, %eax"), vec![0x89, 0xf8]);
    }

    #[test]
    fn the_width_letter_the_operands_already_said() {
        // What gas does with a mnemonic that left it off, and what a hand written file relies on
        // constantly. The same bytes as the line above spelled out.
        assert_eq!(bytes("mov %rdi, %rax"), bytes("movq %rdi, %rax"));
        assert_eq!(bytes("mov %edi, %eax"), bytes("movl %edi, %eax"));
        assert_eq!(bytes("and %rdx, %rcx"), bytes("andq %rdx, %rcx"));
    }

    #[test]
    fn the_other_name_of_a_condition_is_the_same_instruction() {
        // One question under two readings, which is why both names exist. Every hand written file
        // uses some of them and GMP's uses two.
        assert_eq!(bytes("setc %al"), bytes("setb %al"));
        assert_eq!(bytes("cmovz %rdx, %rax"), bytes("cmove %rdx, %rax"));
        assert_eq!(bytes("cmovnzq %rdx, %rax"), bytes("cmovneq %rdx, %rax"));
    }

    #[test]
    fn a_mnemonic_that_ends_in_a_letter_that_is_also_a_width() {
        // `seta` is not `set` at some width and `cmovb` is not `cmov` at byte width, which is why
        // what the program wrote is looked up before anything is glued onto it.
        assert_eq!(bytes("seta %al"), vec![0x0f, 0x97, 0xc0]);
    }

    #[test]
    fn operands_that_disagree_about_the_width_are_refused() {
        let why = refused("mov %eax, %rbx");
        assert!(why.contains("32 bits") && why.contains("64 bits"), "{why}");
    }

    #[test]
    fn a_number_on_the_instruction() {
        assert_eq!(bytes("subq $24, %rsp"), vec![0x48, 0x83, 0xec, 0x18]);
        // Over what one sign extended byte holds, so the four byte form rather than the short one,
        // which is the encoder's choice and is made from the number.
        assert_eq!(bytes("subq $4096, %rsp"), vec![0x48, 0x81, 0xec, 0x00, 0x10, 0x00, 0x00]);
    }

    #[test]
    fn the_three_ways_a_file_writes_a_number() {
        assert_eq!(bytes("addq $0x10, %rax"), bytes("addq $16, %rax"));
        assert_eq!(bytes("addq $020, %rax"), bytes("addq $16, %rax"));
        assert_eq!(bytes("addq $-1, %rax"), vec![0x48, 0x83, 0xc0, 0xff]);
    }

    #[test]
    fn an_address_with_everything_in_it() {
        assert_eq!(bytes("movq 8(%rbp), %rax"), vec![0x48, 0x8b, 0x45, 0x08]);
        assert_eq!(bytes("movq (%rax), %rbx"), vec![0x48, 0x8b, 0x18]);
        assert_eq!(bytes("movq 16(%rsi,%rdi,8), %rax"), vec![0x48, 0x8b, 0x44, 0xfe, 0x10]);
    }

    #[test]
    fn a_store_and_a_load_are_different_instructions_under_one_mnemonic() {
        assert_eq!(bytes("movq %rbx, 0(%rsp)"), vec![0x48, 0x89, 0x1c, 0x24]);
        assert_ne!(bytes("movq %rbx, 0(%rsp)"), bytes("movq 0(%rsp), %rbx"));
    }

    #[test]
    fn the_segment_a_thread_keeps_its_own_block_in() {
        assert_eq!(bytes("movq %fs:40, %rax"), vec![0x64, 0x48, 0x8b, 0x04, 0x25, 40, 0, 0, 0]);
    }

    #[test]
    fn a_name_counted_from_the_end_of_the_instruction_is_a_hole() {
        let written = one("movq", &["message(%rip)".to_owned(), "%rax".to_owned()]).expect("read");
        assert_eq!(written.holes.len(), 1);
        assert_eq!(written.holes[0].name, "message");
        assert_eq!(written.holes[0].sort, Sort::Near);
        // The four bytes are on the end, which is what makes the addend the distance from there to
        // the end of the instruction.
        assert_eq!(written.holes[0].at, written.bytes.len() - 4);
    }

    #[test]
    fn a_number_counted_from_the_end_of_the_instruction_is_not_one() {
        // `8(%rip)` is a distance the machine works out and there is nothing for a linker to do.
        let written = one("movq", &["8(%rip)".to_owned(), "%rax".to_owned()]).expect("read");
        assert!(written.holes.is_empty(), "{:?}", written.holes);
    }

    #[test]
    fn somewhere_to_go_is_a_hole_whatever_kind_of_branch_it_is() {
        for line in ["jmp there", "je there", "jnz there", "call there"] {
            let (word, rest) = line.split_once(' ').expect("two words");
            let written = one(word, &[rest.to_owned()]).expect("read");
            assert_eq!(written.holes.len(), 1, "{line}");
            assert_eq!(written.holes[0].name, "there", "{line}");
            assert_eq!(written.holes[0].sort, Sort::Branch, "{line}");
            assert_eq!(written.holes[0].width, 4, "{line}");
            assert_eq!(written.holes[0].at, written.bytes.len() - 4, "{line}");
        }
    }

    #[test]
    fn the_one_branch_that_leaves_a_byte_says_a_byte() {
        // `jrcxz` has no form with four bytes of distance in it, so the hole is one byte wide and
        // says so. A caller that took four from every branch would write over whatever the file
        // put behind this instruction, which is a corruption nothing downstream could notice.
        let written = one("jrcxz", &["there".to_owned()]).expect("read");
        assert_eq!(written.bytes, vec![0xe3, 0x00]);
        assert_eq!(written.holes.len(), 1);
        assert_eq!(written.holes[0].width, 1);
        assert_eq!(written.holes[0].sort, Sort::Branch);
        assert_eq!(written.holes[0].at, 1);
    }

    #[test]
    fn the_instructions_a_hand_written_file_writes_without_a_width_letter() {
        // The rows added for somebody else's assembly, reached the way that assembly spells them,
        // which is with the width left off wherever the operands say it. What is being checked
        // here is the reading rather than the bytes, which `rucc-target` pins against gas.
        assert_eq!(bytes("adc (%rdx), %r8"), vec![0x4c, 0x13, 0x02]);
        assert_eq!(bytes("adc %eax, %eax"), vec![0x11, 0xc0]);
        assert_eq!(bytes("bt $0, %r8"), vec![0x49, 0x0f, 0xba, 0xe0, 0x00]);
        assert_eq!(bytes("dec %rcx"), vec![0x48, 0xff, 0xc9]);
        assert_eq!(bytes("inc %eax"), vec![0xff, 0xc0]);
        assert_eq!(bytes("lea 32(%rsi), %rsi"), vec![0x48, 0x8d, 0x76, 0x20]);
        // `setc` is `setb` under its other name, which the conditions table already handled and
        // which the file this was all for uses on the line after the additions.
        assert_eq!(bytes("setc %al"), vec![0x0f, 0x92, 0xc0]);
    }

    #[test]
    fn a_branch_through_a_register_is_a_different_instruction_and_names_nothing() {
        let written = one("jmp", &["*%rax".to_owned()]).expect("read");
        assert_eq!(written.bytes, vec![0xff, 0xe0]);
        assert!(written.holes.is_empty());
    }

    #[test]
    fn an_instruction_with_no_operands() {
        assert_eq!(bytes("ret"), vec![0xc3]);
        assert_eq!(bytes("nop"), vec![0x90]);
    }

    #[test]
    fn a_register_this_machine_does_not_have_is_refused() {
        let why = refused("movq %rax, %r99");
        assert!(why.contains("r99"), "{why}");
    }

    #[test]
    fn an_address_made_of_a_register_that_is_not_whole_is_refused() {
        // The machine has no such addressing mode on this target, and reading it as the whole
        // register would be an address off by whatever the top half holds.
        let why = refused("movq (%eax), %rbx");
        assert!(why.contains("32 bits"), "{why}");
    }

    #[test]
    fn a_name_in_an_address_that_is_not_counted_from_the_instruction_is_refused() {
        // Rather than assembled as a zero displacement, which links and reads the wrong address.
        let why = refused("movq message(%rbx), %rax");
        assert!(why.contains("relocation"), "{why}");
    }

    #[test]
    fn a_scale_the_machine_does_not_have_is_refused() {
        let why = refused("movq (%rsi,%rdi,3), %rax");
        assert!(why.contains("scale"), "{why}");
    }

    #[test]
    fn an_instruction_this_compiler_has_no_bytes_for_is_refused_by_name() {
        let why = refused("bswap %rax");
        assert!(why.contains("bswap"), "{why}");
    }
}
