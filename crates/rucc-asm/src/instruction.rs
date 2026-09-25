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
//! A symbol as an immediate is not read, nor a symbol as the displacement of an address that names
//! a register, because both want a relocation this does not write yet and a wrong guess about
//! either is silent.

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
    /// What the file added to the name, which is nothing at all nearly every time.
    pub addend: i64,
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
    /// A number the instruction carries that the file wrote as an expression, which is what
    /// `$4f-3b` is. The name is the whole of the expression and the bytes are the value of it,
    /// worked out once the labels in it have places, and never a relocation.
    Value,
    /// An entry in the global offset table, which is what `message@GOTPCREL(%rip)` is. The bytes
    /// hold the distance to a word the linker makes and fills with the address, so the instruction
    /// loads the address rather than computing it, and what it names has to be relocated even when
    /// this file defines it, since the entry is somewhere else whatever the name turns out to be.
    Table,
    /// The offset of a thread-local variable from the thread pointer, which is what
    /// `message@GOTTPOFF(%rip)` is and is a relocation for the same reason.
    Thread,
}

/// The name in a displacement, and which of the three ways of reaching it the suffix asks for.
///
/// A bare name is the datum itself. `@GOTPCREL` and `@GOTTPOFF` are the two suffixes this compiler
/// writes, and reading them back is what lets a file it emitted be assembled by it.
fn reached(named: &str) -> Result<(String, Sort), String> {
    let Some((name, how)) = named.split_once('@') else {
        return Ok((named.to_owned(), Sort::Near));
    };
    match how {
        "GOTPCREL" => Ok((name.to_owned(), Sort::Table)),
        "GOTTPOFF" => Ok((name.to_owned(), Sort::Thread)),
        _ => Err(format!("'@{how}' is not a way of reaching something this compiler reads")),
    }
}

/// One instruction, written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Written {
    /// Its bytes.
    pub bytes: Vec<u8>,
    /// Every place in them that names something outside the instruction.
    pub holes: Vec<Hole>,
}

/// A name written in a displacement, with whatever was added to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Named {
    /// The name as the source spelled it, suffix and all.
    name: String,
    /// What the file added to it, which is nothing at all nearly every time.
    addend: i64,
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
    /// A place on the x87 stack, by its depth.
    Stack(u8),
    /// An address, and the name in its displacement when it has one.
    Mem(Addr, Option<Named>),
    /// A number the instruction carries.
    Imm(i64),
    /// A number the instruction carries that is an expression over labels, as the file wrote it.
    Expr(String),
    /// A name to jump or call to, and whatever was added to it. The name is `.` when the file
    /// counted from the instruction itself, which is what `jmp .+6` does.
    Dest(Named),
}

/// What an expression the instruction carries stands for while the row is being chosen.
///
/// Big enough that no row which holds only a byte is chosen for it, since what the expression comes
/// to is not known yet and four bytes hold whatever it turns out to be. An instruction that has no
/// row for a number that size, which is a shift or `int`, is tried again with nothing, and then the
/// value is checked against the byte it has once it is known.
const STANDING: i64 = 0x1000_0000;

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
    let mut operands: Vec<Operand> =
        args.iter().map(|arg| operand(arg.trim())).collect::<Result<_, _>>()?;
    let predicated = predicated(word);
    let word = match &predicated {
        Some((name, which)) => {
            operands.insert(0, Operand::Imm(*which));
            name.as_str()
        }
        None => word,
    };
    if !BRANCHES.iter().any(|branch| word.starts_with(branch)) {
        for operand in &mut operands {
            outright(operand);
        }
    }
    let mut values: Vec<Value> = operands.iter().map(|op| value(op, STANDING)).collect();
    let (mnemonic, row) = match spelled(word, &operands, &values) {
        Ok(found) => found,
        Err(_) if operands.iter().any(|op| matches!(op, Operand::Expr(_))) => {
            values = operands.iter().map(|op| value(op, 0)).collect();
            spelled(word, &operands, &values)?
        }
        Err(why) => return Err(why),
    };
    let named: Vec<u8> = operands
        .iter()
        .filter_map(|op| match op {
            Operand::Stack(depth) => Some(*depth),
            _ => None,
        })
        .collect();
    if !named.is_empty() && named != depths(&mnemonic) {
        return Err(format!(
            "'{word}' at those depths of the x87 stack is not one this compiler has"
        ));
    }

    let mut bytes = Vec::with_capacity(16);
    let holes = encode(&mnemonic, &values, &mut bytes).map_err(|why| why.to_string())?;
    if holes.dest.is_none() {
        shorter(&mut bytes, &operands);
    }

    let mut wanted = Vec::new();
    if let Some(at) = holes.dest {
        let Some(Operand::Dest(Named { name, addend })) =
            operands.iter().find(|op| matches!(op, Operand::Dest(_)))
        else {
            return Err(format!("'{word}' left room for somewhere to go and was given nowhere"));
        };
        // Counted from the start of this instruction, which is known now and is not a name, so
        // the distance goes straight into the bytes. gas takes the two byte form of a jump when
        // the distance fits in one, and a file that counts its own bytes is counting that form:
        // `jmp .+6` in front of four bytes of data is a jump over them and nothing else.
        if name == "." {
            return Ok(Written { bytes: counted(bytes, at, *addend)?, holes: Vec::new() });
        }
        // How much room the instruction left, which is four for every branch but one. `jrcxz` has
        // a single byte and no longer form, so a destination further away than that is a mistake
        // in the file rather than something to relax, and the caller is the one that can tell.
        let width = match row.imm {
            ImmSize::Cb => 1,
            _ => 4,
        };
        // `@PLT` asks for the stub a call to another object's name may go through, which is the
        // relocation a branch gets here whether the file asks or not. So the suffix is nothing to
        // do and it is not part of the name: leaving it on would put a symbol in the table that
        // nothing anywhere defines and the link would fail on it.
        let name = match name.split_once('@') {
            None => name.clone(),
            Some((name, "PLT")) => name.to_owned(),
            Some((_, how)) => {
                return Err(format!("'@{how}' is not a way of reaching somewhere to go"));
            }
        };
        wanted.push(Hole { at, width, name, addend: *addend, sort: Sort::Branch });
    }
    if let Some(at) = holes.rip {
        // Only when the source put a name there. `8(%rip)` is a number the machine counts from the
        // end of the instruction and there is nothing for a linker to do about it.
        let named = operands.iter().find_map(|op| match op {
            Operand::Mem(_, Some(named)) => Some(named.clone()),
            _ => None,
        });
        if let Some(named) = named {
            let (name, sort) = reached(&named.name)?;
            wanted.push(Hole { at, width: 4, name, addend: named.addend, sort });
        }
    }
    // A number is the last thing in an instruction on this machine, so an expression the
    // instruction carries is the last bytes of it, however many the row gave it.
    if let Some(text) = operands.iter().find_map(|op| match op {
        Operand::Expr(text) => Some(text.clone()),
        _ => None,
    }) {
        let width = match row.imm {
            ImmSize::Ib => 1,
            ImmSize::Iw => 2,
            ImmSize::Id => 4,
            _ => return Err(format!("'{word}' carries '{text}' somewhere this cannot write one")),
        };
        let at = bytes.len() - width;
        wanted.push(Hole { at, width: width as u8, name: text, addend: 0, sort: Sort::Value });
    }
    Ok(Written { bytes, holes: wanted })
}

/// The two byte form of a jump written with four bytes of distance to a name, or nothing for any
/// other instruction.
///
/// Only `jmp` and the sixteen conditional jumps have one, and a call has none, which is why this
/// looks at the opcode rather than at the hole: `e9` becomes `eb` and `0f 8x` becomes `7x`, each
/// followed by one byte of distance. Whether the byte reaches is not known here, since the name
/// is somewhere in a file that has not been laid out yet, and [`crate::source`] is the one that
/// finds out and asks for the long form back when it does not.
pub(crate) fn short(long: &Written) -> Option<Written> {
    let [hole] = long.holes.as_slice() else { return None };
    if hole.sort != Sort::Branch || hole.width != 4 || hole.at + 4 != long.bytes.len() {
        return None;
    }
    let code = match long.bytes.as_slice() {
        [0xE9, ..] if hole.at == 1 => 0xEB,
        [0x0F, code @ 0x80..=0x8F, ..] if hole.at == 2 => code - 0x10,
        _ => return None,
    };
    Some(Written { bytes: vec![code, 0], holes: vec![Hole { at: 1, width: 1, ..hole.clone() }] })
}

/// The shorter of two encodings gas would pick for the same instruction, where the table wrote the
/// longer one.
///
/// Two of them, and both are choices gas makes on every line it reads, so a file assembled here has
/// to make them too to come out the same size. A shift or rotate by a written `$1` is the opcode
/// that shifts by one and has no count byte, `C1 /4 01` becoming `D1 /4`. And arithmetic or `test`
/// with a four byte immediate into `%eax`, `%rax`, `%ax` or `%al` has a form with no addressing
/// byte, `81 /7` with `%eax` becoming `3D`, which the table leaves out because it is a row for one
/// register (see section 11.1 of the spec). An immediate that fits in a byte is already the shorter
/// `83` form and is left as it is, since gas takes that one too.
///
/// Only the prefixes this machine puts in front of these, the operand size one and a REX byte, are
/// stepped over, and a REX byte that moves the register past the first eight means it is not the
/// accumulator. Anything else is left alone.
fn shorter(bytes: &mut Vec<u8>, operands: &[Operand]) {
    let mut at = 0;
    let mut far = false;
    while at < bytes.len() && (bytes[at] == 0x66 || (bytes[at] & 0xF0 == 0x40)) {
        far |= bytes[at] & 0xF0 == 0x40 && bytes[at] & 1 != 0;
        at += 1;
    }
    let (Some(&code), Some(&modrm)) = (bytes.get(at), bytes.get(at + 1)) else { return };
    let digit = (modrm >> 3) & 7;
    if matches!(code, 0xC0 | 0xC1)
        && operands.first() == Some(&Operand::Imm(1))
        && bytes.last() == Some(&1)
    {
        bytes[at] = code + 0x10;
        bytes.pop();
        return;
    }
    if far || modrm != 0xC0 | (digit << 3) {
        return;
    }
    let short = match (code, digit) {
        (0x80, _) => (digit << 3) | 0x04,
        (0x81, _) => (digit << 3) | 0x05,
        (0xF6, 0) => 0xA8,
        (0xF7, 0) => 0xA9,
        _ => return,
    };
    bytes[at] = short;
    bytes.remove(at + 1);
}

/// A branch whose distance is counted from its own first byte, written with that distance in it.
///
/// `long` is the four byte form the encoder wrote and `at` is where its distance starts. The two
/// byte form is the same condition with a one byte distance, `E9` becoming `EB` for a plain jump
/// and `0F 8x` becoming `7x` for a conditional one, and it is taken whenever the distance fits.
fn counted(mut long: Vec<u8>, at: usize, from_start: i64) -> Result<Vec<u8>, String> {
    let short = match long.as_slice() {
        [0xE9, ..] if at == 1 => Some(0xEB),
        [0x0F, code @ 0x80..=0x8F, ..] if at == 2 => Some(code - 0x10),
        _ => None,
    };
    if let Some(code) = short {
        if let Ok(distance) = i8::try_from(from_start - 2) {
            return Ok(vec![code, distance as u8]);
        }
    }
    let distance = i32::try_from(from_start - long.len() as i64)
        .map_err(|_| format!("'.+{from_start}' is further than a branch reaches"))?;
    long[at..at + 4].copy_from_slice(&distance.to_le_bytes());
    Ok(long)
}

/// The eight comparisons a float comparison can be asked for, in the order of the immediate that
/// asks for each.
const PREDICATES: [&str; 8] = ["eq", "lt", "le", "unord", "neq", "nlt", "nle", "ord"];

/// A float comparison written with its predicate in the name, as the mnemonic that takes it as an
/// immediate and the immediate.
///
/// gas takes `cmpnlesd %xmm0, %xmm2` for `cmpsd $6, %xmm0, %xmm2` and gcc writes only the first.
/// `cmpsd` with no predicate is the string comparison, which is not this, so a name that has none
/// of the eight in it is left alone.
fn predicated(word: &str) -> Option<(String, i64)> {
    let rest = word.strip_prefix("cmp")?;
    let (predicate, format) = rest.split_at(rest.len().checked_sub(2)?);
    if !matches!(format, "ss" | "sd" | "ps" | "pd") {
        return None;
    }
    let which = PREDICATES.iter().position(|&known| known == predicate)?;
    Some((format!("cmp{format}"), which as i64))
}

/// The mnemonics whose bare name operand is somewhere to go rather than an address.
const BRANCHES: [&str; 4] = ["j", "call", "loop", "xbegin"];

/// A bare number where an address goes, read as the address it is.
///
/// `movq %rax, 0` stores to address zero, which is what a program writes to crash on purpose, and
/// gas takes it as an address with no base and no index. A bare number is read as somewhere to go
/// because in front of a jump that is what it is, so anything that is not a jump reads it again.
fn outright(operand: &mut Operand) {
    let Operand::Dest(Named { name, addend: 0 }) = operand else { return };
    let Some(disp) = number(name).ok().and_then(|value| i32::try_from(value).ok()) else {
        return;
    };
    *operand = Operand::Mem(Addr { disp, scale: 1, ..Addr::default() }, None);
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
    if let Some(width) = stated(word, operands)? {
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
    // Shifting left arithmetically and shifting left logically are one instruction under two
    // names, because the two differ only in what is put back at the bottom and neither puts
    // anything back at the bottom. gas takes both and the encoder knows one.
    if let Some(rest) = word.strip_prefix("sal") {
        if rest.is_empty() || matches!(rest, "b" | "w" | "l" | "q") {
            return Some(format!("shl{rest}"));
        }
    }
    // A push and a pop move eight bytes in long mode and there is no other width of either, so the
    // letter is not a thing a file has to write and mostly is not written. The letter cannot be
    // worked out from the operands the way every other one is, because an address says nothing
    // about how wide the access is, so these are named here rather than guessed at. The same for
    // the two about the flags, whose operand is not written at all.
    if let Some(known) = ["push", "pop", "pushf", "popf"].iter().find(|&&known| known == word) {
        return Some(format!("{known}q"));
    }
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

/// The instructions whose first operand is a count and says nothing about how wide they are.
///
/// A shift takes its count in `cl` and nowhere else, so `shr %cl, %rax` names a byte and eight
/// bytes in one line and is not a mistake: the byte is where the count lives and the instruction is
/// eight bytes wide. Every other instruction on this machine that names two registers of different
/// widths says so in the mnemonic, which is why disagreement is an error anywhere but here.
const COUNTED: &[&str] = &["shl", "shr", "sar", "sal", "rol", "ror", "rcl", "rcr", "shld", "shrd"];

/// How wide the operands say the instruction is, when they say.
///
/// Every register named has to agree, since two that disagree are either a mnemonic that already
/// carries its width, which was tried before this, or a shift counted in `cl`, or a line no
/// assembler would take.
fn stated(word: &str, operands: &[Operand]) -> Result<Option<Width>, String> {
    let mut width = None;
    let counted = COUNTED.contains(&word) && operands.len() > 1;
    for operand in operands.iter().skip(usize::from(counted)) {
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

/// What the encoder is handed for one of these, with `standing` for an expression not worked out.
fn value(operand: &Operand, standing: i64) -> Value {
    match operand {
        Operand::Reg(reg, width) => Value::Reg(*reg, *width),
        Operand::High(reg) => Value::High(*reg),
        Operand::Xmm(reg) => Value::Xmm(*reg),
        Operand::Stack(_) => Value::Stack,
        Operand::Mem(addr, _) => Value::Mem(*addr),
        Operand::Imm(number) => Value::Imm(*number),
        Operand::Expr(_) => Value::Imm(standing),
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
        // A number where it is one, and otherwise an expression the file works out once its
        // labels have places, which is what `$4f-3b` is.
        return Ok(number(rest.trim())
            .map_or_else(|_| Operand::Expr(rest.trim().to_owned()), Operand::Imm));
    }
    if let Some(depth) = stack(text) {
        return Ok(Operand::Stack(depth));
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
        return Ok(Operand::Dest(Named { name: text.to_owned(), addend: 0 }));
    }
    // Somewhere counted from a name, which is `.+6` as often as it is anything.
    if let Ok((addend, Some(name))) = parted(text) {
        return Ok(Operand::Dest(Named { name, addend }));
    }
    Err(format!("'{text}' is not an operand this compiler reads"))
}

/// The depth of a place on the x87 stack, for `%st` and `%st(N)`.
fn stack(text: &str) -> Option<u8> {
    let rest = text.strip_prefix("%st")?;
    if rest.is_empty() {
        return Some(0);
    }
    let depth = rest.strip_prefix('(')?.strip_suffix(')')?.trim().parse::<u8>().ok()?;
    (depth < 8).then_some(depth)
}

/// The depths an x87 instruction of that mnemonic is encoded with.
///
/// The encoder has no depth in its arguments: each row is the one pair of places the compiler
/// writes that instruction with, and the depth is part of its opcode. So a line naming any other
/// depth is refused here rather than written as the one the row has.
fn depths(mnemonic: &str) -> &'static [u8] {
    match mnemonic {
        "faddp" | "fsubp" | "fsubrp" | "fmulp" | "fdivp" | "fdivrp" => &[0, 1],
        "fucomip" => &[1, 0],
        "fstp" => &[0],
        _ => &[],
    }
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
        let (value, name) = parted(front)?;
        match name {
            // The number goes with the name rather than into the bytes, because what the bytes end
            // up holding is the linker's business and it is told the whole sum at once.
            Some(name) => named = Some(Named { name, addend: value }),
            None => {
                addr.disp = i32::try_from(value).map_err(|_| {
                    format!("'{front}' does not fit in the four bytes of an address")
                })?;
            }
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

/// The displacement of an address, which is terms added together as often as it is one term.
///
/// gas takes a whole expression in front of the bracket and a file written by hand uses that to
/// write a displacement as the things it is made of rather than as the total. `56+8(%rsp)` is an
/// offset into a frame with the return address that was pushed on top of it counted in, and the
/// reader of that file is meant to see both halves. `-512+table(%rip)` is a lookup that reaches
/// its table from the middle, because the value it indexes by starts at two fifty six rather than
/// at zero, and the number is as much a part of what the linker is asked for as the name is.
///
/// So what comes back is the numbers folded together and the name if there was one. At most one
/// term may be a name and it may not be the subtracted one, since the distance back from something
/// is not a thing a relocation says.
fn parted(text: &str) -> Result<(i64, Option<String>), String> {
    let text = text.trim();
    let mut total: i64 = 0;
    let mut sign: i64 = 1;
    let mut start = 0usize;
    let mut named: Option<String> = None;
    let mut fold = |term: &str, sign: i64, named: &mut Option<String>| match number(term) {
        Ok(value) => {
            total = total.wrapping_add(sign.wrapping_mul(value));
            Ok(())
        }
        Err(why) => {
            if named.is_some() {
                return Err(
                    "two names added together, which is not a place a linker can find".to_owned()
                );
            }
            if sign < 0 {
                return Err(why);
            }
            *named = Some(term.trim().to_owned());
            Ok(())
        }
    };
    for (at, ch) in text.char_indices() {
        // Not at the start of a term, where a sign belongs to the number behind it rather than
        // joining it to anything.
        if at == start || !matches!(ch, '+' | '-') {
            continue;
        }
        fold(&text[start..at], sign, &mut named)?;
        sign = if ch == '-' { -1 } else { 1 };
        start = at + 1;
    }
    fold(&text[start..], sign, &mut named)?;
    Ok((total, named))
}

/// A number written the way an assembler writes one.
fn number(text: &str) -> Result<i64, String> {
    let text = text.trim();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1i64, rest.trim()),
        None => (1, text.strip_prefix('+').map_or(text, str::trim)),
    };
    // As unsigned first, because a file writes a sixty four bit mask as a positive hex number and
    // that number is negative when it is read as signed, which is the same bits. The same goes for
    // the most negative number there is, whose digits are one more than the largest positive one
    // and which a compiler writes as a minus sign in front of them.
    let value =
        if let Some(hex) = digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
            u64::from_str_radix(hex, 16)
        } else if digits.len() > 1 && digits.starts_with('0') {
            u64::from_str_radix(&digits[1..], 8)
        } else {
            digits.parse::<u64>()
        }
        .map(|value| value as i64);
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
    fn the_most_negative_number_is_a_number() {
        assert_eq!(
            bytes("movabsq $-9223372036854775808, %rax"),
            [0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0x80]
        );
    }

    #[test]
    fn a_conditional_jump_counted_from_itself_is_short_when_it_fits() {
        assert_eq!(bytes("jne .+2"), [0x75, 0x00]);
        assert_eq!(bytes("jmp .+1000"), [0xe9, 0xe3, 0x03, 0x00, 0x00]);
    }

    #[test]
    fn the_x87_stack_at_the_depths_the_compiler_writes_it() {
        assert_eq!(bytes("fucomip %st(1), %st(0)"), [0xdf, 0xe9]);
        assert_eq!(bytes("fstp %st(0)"), [0xdd, 0xd8]);
        assert_eq!(bytes("fstp %st"), [0xdd, 0xd8]);
        assert_eq!(bytes("faddp %st(0), %st(1)"), [0xde, 0xc1]);
        // Another depth is another opcode, which no row here has.
        assert!(refused("fstp %st(1)").contains("depths"));
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
    fn a_line_gas_writes_shorter_is_written_shorter_here_too() {
        // Every byte here is what gas 2.42 writes for the same line.
        assert_eq!(bytes("shl $1, %eax"), [0xd1, 0xe0]);
        assert_eq!(bytes("sarq $1, %rdx"), [0x48, 0xd1, 0xfa]);
        assert_eq!(bytes("shrb $1, %r9b"), [0x41, 0xd0, 0xe9]);
        assert_eq!(bytes("shl $2, %eax"), [0xc1, 0xe0, 0x02]);
        assert_eq!(bytes("cmp $1000000, %eax"), [0x3d, 0x40, 0x42, 0x0f, 0x00]);
        assert_eq!(bytes("addq $4096, %rax"), [0x48, 0x05, 0x00, 0x10, 0x00, 0x00]);
        assert_eq!(bytes("andw $4095, %ax"), [0x66, 0x25, 0xff, 0x0f]);
        assert_eq!(bytes("xorb $15, %al"), [0x34, 0x0f]);
        assert_eq!(bytes("test $256, %eax"), [0xa9, 0x00, 0x01, 0x00, 0x00]);
        assert_eq!(bytes("testb $1, %al"), [0xa8, 0x01]);
        // A byte of immediate is shorter the ordinary way, and a register other than the first
        // has no form of its own.
        assert_eq!(bytes("cmp $1, %eax"), [0x83, 0xf8, 0x01]);
        assert_eq!(bytes("cmp $1000000, %ecx"), [0x81, 0xf9, 0x40, 0x42, 0x0f, 0x00]);
        assert_eq!(bytes("cmp $1000000, %r8d"), [0x41, 0x81, 0xf8, 0x40, 0x42, 0x0f, 0x00]);
    }

    #[test]
    fn shifting_left_arithmetically_is_shifting_left_and_the_table_knows_one_name_for_it() {
        // Two names for one instruction, because there is nothing to put back at the bottom and so
        // nothing for the two to disagree about. GMP writes the arithmetic spelling and gcc writes
        // the logical one, and both mean the same three bytes.
        assert_eq!(bytes("sal $11, %eax"), bytes("shl $11, %eax"));
        assert_eq!(bytes("salq $1, %rdx"), bytes("shlq $1, %rdx"));
        assert_eq!(bytes("sal %cl, %rax"), bytes("shl %cl, %rax"));
    }

    /// Lines from gcc's output that name memory where the compiler's own code names a register,
    /// with the bytes gas writes for each.
    #[test]
    fn the_integer_forms_gcc_writes_with_memory_in_them() {
        assert_eq!(bytes("sall -4(%rbp)"), [0xd1, 0x65, 0xfc]);
        assert_eq!(bytes("shrl $1, -4(%rbp)"), [0xd1, 0x6d, 0xfc]);
        assert_eq!(bytes("shrq %cl, -8(%rbp)"), [0x48, 0xd3, 0x6d, 0xf8]);
        assert_eq!(bytes("shrw $8, 16(%rdi)"), [0x66, 0xc1, 0x6f, 0x10, 0x08]);
        assert_eq!(bytes("roll $13, -4(%rbp)"), [0xc1, 0x45, 0xfc, 0x0d]);
        assert_eq!(bytes("sete 78(%rsp)"), [0x0f, 0x94, 0x44, 0x24, 0x4e]);
        assert_eq!(bytes("pushq $112"), [0x6a, 0x70]);
        assert_eq!(bytes("pushq $1000"), [0x68, 0xe8, 0x03, 0, 0]);
        assert_eq!(bytes("imull $-1640531535, (%rsi), %eax"), [0x69, 0x06, 0xb1, 0x79, 0x37, 0x9e]);
        assert_eq!(bytes("imulq $40, 8(%rsp), %rax"), [0x48, 0x6b, 0x44, 0x24, 0x08, 0x28]);
    }

    /// A float comparison with its predicate in the name is the one with it as an immediate.
    #[test]
    fn a_comparison_named_for_its_predicate_is_the_one_with_an_immediate() {
        assert_eq!(bytes("cmpnlesd %xmm0, %xmm2"), [0xf2, 0x0f, 0xc2, 0xd0, 0x06]);
        assert_eq!(bytes("cmpltss (%rax), %xmm1"), [0xf3, 0x0f, 0xc2, 0x08, 0x01]);
        assert_eq!(bytes("cmpnlesd %xmm0, %xmm2"), bytes("cmpsd $6, %xmm0, %xmm2"));
    }

    /// A bare number where an address goes is that address, with no base and no index.
    #[test]
    fn a_bare_number_is_an_address_outright() {
        assert_eq!(bytes("movq %rax, 0"), [0x48, 0x89, 0x04, 0x25, 0, 0, 0, 0]);
        assert_eq!(bytes("movl %eax, 8"), [0x89, 0x04, 0x25, 0x08, 0, 0, 0]);
    }

    #[test]
    fn a_count_in_a_byte_register_says_nothing_about_how_wide_the_shift_is() {
        // The one place on this machine where two registers of different widths on one line is not
        // a mistake: a shift takes its count in `cl` and nowhere else, so the byte says where the
        // count is and the other operand says how wide the instruction is. Everywhere else the
        // disagreement is still refused, which the test above this one checks.
        assert_eq!(bytes("shr %cl, %rax"), bytes("shrq %cl, %rax"));
        assert_eq!(bytes("shl %cl, %edx"), bytes("shll %cl, %edx"));
        assert_eq!(bytes("rcr %cl, %rbx"), bytes("rcrq %cl, %rbx"));
        // And the three operand shifts, whose count is in the same place and whose other two
        // operands are the pair the window moves across.
        assert_eq!(bytes("shld %cl, %rsi, %rdi"), bytes("shldq %cl, %rsi, %rdi"));
    }

    #[test]
    fn a_shift_that_takes_its_count_anywhere_says_its_width_the_ordinary_way() {
        // The three BMI2 shifts are the one group of counted instructions that is not in `COUNTED`,
        // and that is right rather than an omission. What that list is for is a count in `cl` on a
        // line whose other operands are wider, and the whole point of these is that the count is a
        // register of the same width as everything else, so all three operands agree and the width
        // is read off the line the way it is read off every other one.
        assert_eq!(bytes("shlx %rdx, %rax, %rax"), bytes("shlxq %rdx, %rax, %rax"));
        assert_eq!(bytes("shrx %rdx, %rax, %rax"), bytes("shrxq %rdx, %rax, %rax"));
        assert_eq!(bytes("sarx %edx, %eax, %eax"), bytes("sarxl %edx, %eax, %eax"));
        // The lines zstd's huffman decoder is built out of, which are what this is all for. The
        // bytes are pinned in `rucc-target` against gas, so what is being checked here is that the
        // reader gets to that row at all.
        assert_eq!(bytes("shrxq %r8, %rax, %rdx"), vec![0xc4, 0xe2, 0xbb, 0xf7, 0xd0]);
        assert_eq!(bytes("shlx %r15, %rax, %rax"), vec![0xc4, 0xe2, 0x81, 0xf7, 0xc0]);
        // A count in a byte register is a mistake on one of these rather than the one place it is
        // not, since there is nothing about `cl` in a shift that reads a whole register.
        let why = refused("shlx %cl, %rax, %rax");
        assert!(why.contains("8 bits") && why.contains("64 bits"), "{why}");
    }

    #[test]
    fn a_displacement_that_is_written_as_a_sum_is_the_sum() {
        // A file written by hand says where a thing is by adding up what it is made of, so
        // `56+8(%rsp)` is the seventh word of a frame rather than a symbol nothing defines. What
        // the reader did before this was take the whole of it as a name and then fail to find one.
        assert_eq!(bytes("movl 56+8(%rsp), %ecx"), bytes("movl 64(%rsp), %ecx"));
        assert_eq!(bytes("lea -512+128(%rsp), %rdi"), bytes("lea -384(%rsp), %rdi"));
        assert_eq!(bytes("movq 8+8+8(%rdi), %rax"), bytes("movq 24(%rdi), %rax"));
        assert_eq!(bytes("movq 32-8(%rdi), %rax"), bytes("movq 24(%rdi), %rax"));
    }

    #[test]
    fn a_name_reached_through_the_global_offset_table_says_which_kind_of_hole_it_is() {
        let arg = "table@GOTPCREL(%rip)".to_owned();
        let written = one("movq", &[arg, "%rdx".to_owned()]).expect("read");
        assert_eq!(written.holes.len(), 1);
        // The suffix is how the address is reached and not part of what is being reached, so what
        // goes in the symbol table is the name without it.
        assert_eq!(written.holes[0].name, "table");
        assert_eq!(written.holes[0].sort, Sort::Table);
        assert_eq!(written.holes[0].at, written.bytes.len() - 4);
        // The other suffix this compiler writes, which is the same shape and a different relocation
        // because what the slot holds is an offset rather than an address.
        let arg = "counter@GOTTPOFF(%rip)".to_owned();
        let written = one("movq", &[arg, "%rax".to_owned()]).expect("read");
        assert_eq!(written.holes[0].name, "counter");
        assert_eq!(written.holes[0].sort, Sort::Thread);
        // And one nobody writes, which is said rather than guessed at.
        let why = refused("movq away@TPOFF(%rip), %rax");
        assert!(why.contains("@TPOFF"), "{why}");
    }

    #[test]
    fn a_call_through_a_stub_is_the_relocation_a_call_already_gets() {
        // `@PLT` asks for the thing this was going to do anyway, so it means nothing here and is
        // not part of the name. GMP writes it on the one call it makes out of assembly, and leaving
        // it on put a symbol called `__gmpn_invert_limb@PLT` in the table, which nothing defines.
        let written = one("call", &["work@PLT".to_owned()]).expect("read");
        assert_eq!(written.holes.len(), 1);
        assert_eq!(written.holes[0].name, "work");
        assert_eq!(written.holes[0].sort, Sort::Branch);
        assert_eq!(written.bytes, one("call", &["work".to_owned()]).expect("read").bytes);
        let why = refused("call work@GOTPCREL");
        assert!(why.contains("@GOTPCREL"), "{why}");
    }

    #[test]
    fn a_branch_through_a_register_is_a_different_instruction_and_names_nothing() {
        let written = one("jmp", &["*%rax".to_owned()]).expect("read");
        assert_eq!(written.bytes, vec![0xff, 0xe0]);
        assert!(written.holes.is_empty());
    }

    #[test]
    fn a_branch_through_a_table_is_the_same_instruction_with_an_address_in_it() {
        // `jmp *72(%r8,%rsi,8)` in libgmp's `mpn/x86_64/mod_34lsub1.asm`, which is a table of
        // places at a fixed offset from a register, indexed by a count, eight bytes to an entry. A
        // compiler that owns both halves loads the entry and jumps through the register, and a file
        // written by hand writes the load and the jump as one instruction because it can.
        let written = one("jmp", &["*72(%r8,%rsi,8)".to_owned()]).expect("read");
        // The REX byte carries the base being one of the high eight, the addressing byte names the
        // four bits that tell this from the other seven instructions sharing the opcode and says a
        // scaled index follows, and the byte after it is the scale, the index and the base.
        assert_eq!(written.bytes, vec![0x41, 0xff, 0x64, 0xf0, 0x48]);
        // Nothing for a linker to do, since where it goes is a number the machine works out.
        assert!(written.holes.is_empty());
        // And the call, which is the same row one place along. `call *(%rax)` in libgmp's
        // `tests/amd64call.asm` calls whatever the table entry it just loaded points at.
        let written = one("call", &["*(%rax)".to_owned()]).expect("read");
        assert_eq!(written.bytes, vec![0xff, 0x10]);
        assert!(written.holes.is_empty());
    }

    #[test]
    fn a_constant_written_straight_into_memory() {
        // `movq $0, -8(%rsp)` in libgmp's `tests/amd64call.asm`, which clears the word it is about
        // to read the control register into. This compiler writes no such instruction, because a
        // store it made has the value in a register by the time it reaches here, and a file written
        // by hand puts the number in the slot and is done.
        assert_eq!(bytes("movq $0, -8(%rsp)"), vec![0x48, 0xc7, 0x44, 0x24, 0xf8, 0, 0, 0, 0]);
        assert_eq!(bytes("movl $1, -8(%rsp)"), vec![0xc7, 0x44, 0x24, 0xf8, 1, 0, 0, 0]);
        assert_eq!(bytes("movw $1, -8(%rsp)"), vec![0x66, 0xc7, 0x44, 0x24, 0xf8, 1, 0]);
        assert_eq!(bytes("movb $1, -8(%rsp)"), vec![0xc6, 0x44, 0x24, 0xf8, 1]);
        // There is no form of it that carries eight bytes, so a number that does not fit in four is
        // said rather than truncated.
        let why = refused("movq $0x1122334455, -8(%rsp)");
        assert!(why.contains("movq"), "{why}");
    }

    #[test]
    fn a_push_and_a_pop_need_no_letter_because_there_is_only_one_width_of_them() {
        // Long mode has no other width of either, so the letter says nothing and a file written by
        // hand mostly leaves it off. It cannot be worked out from the operands the way every other
        // one is, because an address says nothing about how wide the access is.
        assert_eq!(bytes("pop 120(%rax)"), vec![0x8f, 0x40, 0x78]);
        assert_eq!(bytes("push 120(%rcx)"), vec![0xff, 0x71, 0x78]);
        assert_eq!(bytes("push %rbx"), bytes("pushq %rbx"));
        // And the two about the flags, which name what they move and take no operand at all.
        assert_eq!(bytes("pushf"), vec![0x9c]);
        assert_eq!(bytes("popf"), vec![0x9d]);
    }

    #[test]
    fn an_x87_instruction_written_with_a_wait_in_front_of_it() {
        // The three that have two names, where the one with the `n` in it is the instruction and
        // the one without is that instruction with `fwait` written first. libgmp's
        // `tests/amd64call.asm` writes all three of the waiting names, since what it is doing is
        // looking at the state a call left behind rather than racing it.
        assert_eq!(bytes("fnstcw -8(%rsp)"), vec![0xd9, 0x7c, 0x24, 0xf8]);
        assert_eq!(bytes("fstcw -8(%rsp)"), vec![0x9b, 0xd9, 0x7c, 0x24, 0xf8]);
        assert_eq!(bytes("fnstenv (%rcx)"), vec![0xd9, 0x31]);
        assert_eq!(bytes("fstenv (%rcx)"), vec![0x9b, 0xd9, 0x31]);
        assert_eq!(bytes("fninit"), vec![0xdb, 0xe3]);
        assert_eq!(bytes("finit"), vec![0x9b, 0xdb, 0xe3]);
        // `fwait` is an instruction rather than a prefix, so a REX byte goes behind it and not in
        // front: a prefix is about whatever follows it, and what follows the wait is the store.
        assert_eq!(bytes("fstcw (%r8)"), vec![0x9b, 0x41, 0xd9, 0x38]);
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
        // One nothing in the compiler writes and nothing in the encoder has a row for, which is a
        // set that shrinks every time a hand written file needs another one. It was `bswap` until
        // tamnd/rucc#1329 gave that one bytes, and it is a population count now.
        let why = refused("popcnt %rax, %rdx");
        assert!(why.contains("popcnt"), "{why}");
    }
}
