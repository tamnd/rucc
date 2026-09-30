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
//! A name as an immediate, or as the displacement of an address that is not counted from the
//! instruction, is the address of the name, which is how code that is not position independent
//! reaches its data. Those come back as holes too, for the whole address rather than a distance.

use rucc_target::x86_64::{
    Addr, Encoding, ImmSize, Length, Mode, Opmask, RAX, RBX, RCX, RDX, Value, Width, encode_masked,
    encoding, gpr_name, gpr_named, xmm,
};
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
    /// worked out once the labels in it have places. A name from another section, or one this file
    /// does not define, makes it the address of that name, which the linker writes: four bytes of
    /// it for an instruction that uses them as they are and eight for `movabs`.
    Value,
    /// The same, for four bytes the machine sign extends to eight, which is an immediate of an
    /// instruction on sixty four bits and the displacement of every address that is not counted
    /// from the instruction. `R_X86_64_32S` rather than `R_X86_64_32` when it is an address, which
    /// is what gas asks for and what makes the linker check the address the right way: one above
    /// two gigabytes fits in four unsigned bytes and comes out negative once it is extended.
    Extended,
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
    /// A vector register only AVX-512 can name, which is `xmm16` and above, any `ymm` and any
    /// `zmm`. By number, since the numbers go past what [`PhysReg`] has for the vector file.
    Vector(u8, Length),
    /// A mask register, `k0` to `k7`.
    Mask(u8),
    /// A place on the x87 stack, by its depth.
    Stack(u8),
    /// A control register, `cr0` to `cr15`.
    Control(u8),
    /// A debug register, `dr0` to `dr15`, which gas also takes spelled `db0` and so on.
    Debug(u8),
    /// A segment register named on its own rather than in front of an address.
    Seg(Segment),
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
    let mut written = full(word, args)?;
    // An address made of thirty two bit registers is the same address with the top half of the
    // sum thrown away, which is one prefix byte in front of the instruction and otherwise the
    // same bytes. It goes behind a segment and in front of everything else, where gas puts it.
    if args.iter().any(|arg| narrow(arg)) {
        let at = written.bytes.iter().take_while(|&&byte| segment_prefix(byte)).count();
        written.bytes.insert(at, 0x67);
        for hole in &mut written.holes {
            hole.at += 1;
        }
    }
    Ok(written)
}

/// Whether the operand is an address whose registers are thirty two bits wide.
fn narrow(arg: &str) -> bool {
    let text = arg.trim();
    let Some(cut) = grouped(text) else { return false };
    text[cut + 1..text.len() - 1].split(',').take(2).any(|part| {
        part.trim()
            .strip_prefix('%')
            .and_then(gpr_named)
            .is_some_and(|(_, width)| width == Width::Long)
    })
}

/// Whether the byte is one of the six segment prefixes.
fn segment_prefix(byte: u8) -> bool {
    matches!(byte, 0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65)
}

/// [`one`], for an address of whole registers.
fn full(word: &str, args: &[String]) -> Result<Written, String> {
    let mut mask = Opmask::default();
    let mut operands = Vec::with_capacity(args.len());
    let ported = matches!(word, "in" | "inb" | "inw" | "inl" | "out" | "outb" | "outw" | "outl");
    for arg in args {
        let (text, said) = masked(arg.trim())?;
        if let Some(said) = said {
            if mask != Opmask::default() {
                return Err("an instruction has one mask and this one names two".to_owned());
            }
            mask = said;
        }
        // gas takes the port of an `in` or an `out` in brackets as well as bare, since it is where
        // the value comes from or goes to, and it is the register all the same.
        let text = if ported && text.replace(' ', "") == "(%dx)" { "%dx" } else { text };
        operands.push(operand(text)?);
    }
    if let Some(written) = segmented(word, &operands)? {
        return Ok(written);
    }
    implied(word, &operands)?;
    let predicated = predicated(word);
    let word = match &predicated {
        Some((name, which)) => {
            operands.insert(0, Operand::Imm(*which));
            name.as_str()
        }
        None => word,
    };
    // gas lets `sha256rnds2` and the three variable blends name the `xmm0` they read without being
    // told, and the row has no place for it.
    if matches!(word, "sha256rnds2" | "pblendvb" | "blendvps" | "blendvpd") && operands.len() == 3 {
        if operands[0] != Operand::Xmm(xmm(0)) {
            return Err(format!("'{word}' reads its third operand from xmm0 and no other"));
        }
        operands.remove(0);
    }
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
    // The row has one depth in its opcode, and every other depth of the same place is the same
    // opcode with the depth added to its last byte. The other place is always the top.
    let row_depths = depths(&mnemonic);
    let which = row_depths.iter().position(|&depth| depth != 0).unwrap_or(0);
    let fits = named.len() == row_depths.len()
        && named.iter().zip(row_depths).enumerate().all(|(at, (n, r))| at == which || n == r);
    if !named.is_empty() && !fits {
        return Err(format!(
            "'{word}' at those depths of the x87 stack is not one this compiler has"
        ));
    }
    if mnemonic == "fnstsw"
        && operands.iter().any(|op| matches!(op, Operand::Reg(reg, _) if *reg != RAX))
    {
        return Err(format!("'{word}' only writes the status word into ax"));
    }
    special(word, &mnemonic, &operands)?;

    let mut bytes = Vec::with_capacity(16);
    let holes =
        encode_masked(&mnemonic, &values, mask, &mut bytes).map_err(|why| why.to_string())?;
    if !named.is_empty() {
        if let Some(last) = bytes.last_mut() {
            *last = last.wrapping_add(named[which]).wrapping_sub(row_depths[which]);
        }
    }
    if holes.dest.is_none() {
        shorter(&mut bytes, &operands, Mode::Bits64);
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
    // A name in an address that is counted from registers or from nothing, which is the address of
    // the name added to them. See [`Sort::Extended`].
    if let Some(at) = holes.disp {
        let Some(named) = operands.iter().find_map(|op| match op {
            Operand::Mem(_, Some(named)) => Some(named.clone()),
            _ => None,
        }) else {
            return Err(format!("'{word}' left room for a name in an address and was given none"));
        };
        wanted.push(Hole {
            at,
            width: 4,
            name: named.name,
            addend: named.addend,
            sort: Sort::Extended,
        });
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
            ImmSize::Io => 8,
            _ => return Err(format!("'{word}' carries '{text}' somewhere this cannot write one")),
        };
        let at = bytes.len() - width;
        // The row was chosen with [`STANDING`] in place of the expression and the encoder wrote
        // it there. The hole is what goes in those bytes, and a hole the linker fills is read as
        // nothing plus its addend, which is what gas leaves: `pushq $sym` is `68 00 00 00 00`.
        bytes[at..].fill(0);
        let sort =
            if width == 4 && extends(&bytes, Mode::Bits64) { Sort::Extended } else { Sort::Value };
        wanted.push(Hole { at, width: width as u8, name: text, addend: 0, sort });
    }
    Ok(Written { bytes, holes: wanted })
}

/// A push or a pop of a segment register, written, or nothing for any other line.
///
/// Long mode pushes and pops `fs` and `gs` and no other segment register, and each of the four is
/// an opcode of its own rather than a register in an addressing byte, so the encoder has a row for
/// each under a name that says which register it is. The other four are refused here, since the
/// opcodes that pushed them on thirty two bits are not instructions on sixty four.
fn segmented(word: &str, operands: &[Operand]) -> Result<Option<Written>, String> {
    let [Operand::Seg(segment)] = operands else { return Ok(None) };
    let way = match word {
        "push" | "pushq" => "pushq",
        "pop" | "popq" => "popq",
        _ => return Ok(None),
    };
    if !matches!(segment, Segment::Fs | Segment::Gs) {
        return Err(format!(
            "'{word} %{}' is not an instruction in long mode, which pushes and pops fs and gs only",
            segment.name()
        ));
    }
    let mut bytes = Vec::with_capacity(2);
    let mnemonic = format!("{way} %{}", segment.name());
    encode_masked(&mnemonic, &[], Opmask::default(), &mut bytes).map_err(|why| why.to_string())?;
    Ok(Some(Written { bytes, holes: Vec::new() }))
}

/// Whether the operand is the general purpose register of that number and width.
fn is(operand: &Operand, reg: PhysReg, width: Width) -> bool {
    *operand == Operand::Reg(reg, width)
}

/// The registers an instruction reads or writes without an addressing byte to say so, checked
/// against the ones the line named.
///
/// These are instructions whose registers are fixed, so the bytes are the same whichever ones the
/// line names and the encoder's rows do not look at them. That makes this the only place a line
/// that names the wrong one is caught, and a line that names the wrong one means something other
/// than what the processor will do, so it is refused the way gas refuses it.
fn implied(word: &str, operands: &[Operand]) -> Result<(), String> {
    let fixed: &[(PhysReg, Width)] = match word {
        "monitor" | "monitorx" => &[(RAX, Width::Quad), (RCX, Width::Long), (RDX, Width::Long)],
        "mwait" => &[(RAX, Width::Long), (RCX, Width::Long)],
        "mwaitx" => &[(RAX, Width::Long), (RCX, Width::Long), (RBX, Width::Long)],
        "invlpga" => &[(RAX, Width::Quad), (RCX, Width::Long)],
        "vmrun" | "vmload" | "vmsave" => &[(RAX, Width::Quad)],
        "skinit" => &[(RAX, Width::Long)],
        _ => return ported(word, operands),
    };
    if operands.is_empty() {
        return Ok(());
    }
    let named = operands.len() == fixed.len()
        && operands.iter().zip(fixed).all(|(operand, &(reg, width))| is(operand, reg, width));
    if !named {
        let names: Vec<String> = fixed
            .iter()
            .map(|&(reg, width)| format!("%{}", gpr_name(reg, width).unwrap_or("?")))
            .collect();
        return Err(format!("'{word}' takes {} and no other registers", names.join(", ")));
    }
    Ok(())
}

/// The same for port I/O, whose port is `dx` or a byte the instruction carries and whose value is
/// in the accumulator, as wide as the letter says when there is one.
fn ported(word: &str, operands: &[Operand]) -> Result<(), String> {
    let (inward, letter) = if let Some(letter) = word.strip_prefix("in") {
        (true, letter)
    } else if let Some(letter) = word.strip_prefix("out") {
        (false, letter)
    } else {
        return Ok(());
    };
    let said = match letter {
        "" => None,
        "b" => Some(Width::Byte),
        "w" => Some(Width::Word),
        "l" => Some(Width::Long),
        _ => return Ok(()),
    };
    let [first, second] = operands else { return Ok(()) };
    let (port, value) = if inward { (first, second) } else { (second, first) };
    let port_ok = matches!(port, Operand::Imm(_) | Operand::Expr(_)) || is(port, RDX, Width::Word);
    let value_ok = match value {
        Operand::Reg(reg, width) => {
            *reg == RAX && *width != Width::Quad && said.is_none_or(|said| said == *width)
        }
        _ => false,
    };
    if !port_ok || !value_ok {
        return Err(format!(
            "'{word}' moves a value in %al, %ax or %eax as wide as it says, through a port in %dx \
             or a number"
        ));
    }
    Ok(())
}

/// The checks on a system instruction that the row it was written with cannot make, since the
/// encoder chooses a row by what kinds its operands are and not by how wide the registers are.
///
/// A control or a debug register is moved to and from a whole general purpose register and no
/// part of one. A segment register is read into, or loaded from, a register as wide as the letter
/// on the mnemonic says.
fn special(word: &str, mnemonic: &str, operands: &[Operand]) -> Result<(), String> {
    let system = operands.iter().any(|op| matches!(op, Operand::Control(_) | Operand::Debug(_)));
    let partial =
        operands.iter().any(|op| matches!(op, Operand::Reg(_, width) if *width != Width::Quad));
    if system && partial {
        return Err(format!(
            "'{word}' moves a control or a debug register to or from a sixty four bit register"
        ));
    }
    if operands.iter().any(|op| matches!(op, Operand::Seg(_))) {
        let wanted = match mnemonic {
            "movw" => Width::Word,
            "movl" => Width::Long,
            "movq" => Width::Quad,
            _ => return Ok(()),
        };
        if operands.iter().any(|op| matches!(op, Operand::Reg(_, width) if *width != wanted)) {
            return Err(format!(
                "'{word}' names a register of a different width from the one it moves"
            ));
        }
    }
    Ok(())
}

/// Whether an instruction sign extends a four byte immediate to eight bytes, which is one whose REX
/// byte asks for sixty four bits and `push`, which is sixty four bits without asking.
///
/// Read off the bytes rather than off the mnemonic, since the suffix is optional and the operand
/// size is what the encoding settled on either way. The prefixes this machine writes in front of an
/// instruction with an immediate are stepped over to get to the REX byte, which is last of them.
/// In thirty two bit mode there is no REX byte and nothing is eight bytes, so nothing extends.
fn extends(bytes: &[u8], mode: Mode) -> bool {
    let mut at = 0;
    while bytes
        .get(at)
        .is_some_and(|byte| matches!(byte, 0x66 | 0x67 | 0xF0 | 0xF2 | 0xF3 | 0x64 | 0x65))
    {
        at += 1;
    }
    let Some(&first) = bytes.get(at) else { return false };
    match mode.rex(first) {
        Some(rex) => rex & 0x08 != 0,
        None => mode == Mode::Bits64 && first == 0x68,
    }
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
/// accumulator. Anything else is left alone, which in thirty two bit mode includes `0x40` to `0x4F`,
/// since there they are `inc` and `dec` rather than a prefix.
fn shorter(bytes: &mut Vec<u8>, operands: &[Operand], mode: Mode) {
    let mut at = 0;
    let mut far = false;
    while let Some(&byte) = bytes.get(at) {
        match mode.rex(byte) {
            Some(rex) => far |= rex & 1 != 0,
            None if byte == 0x66 => {}
            None => break,
        }
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
///
/// A bare name is the same, `movl counter, %eax` being what gcc writes for a global under
/// `-fno-pie`: four bytes of address with no base and no index, which the linker fills in. A name
/// with a suffix is left alone, since every suffix asks for something reached from the instruction.
fn outright(operand: &mut Operand) {
    let Operand::Dest(Named { name, addend }) = operand else { return };
    if *addend == 0 {
        if let Some(disp) = number(name).ok().and_then(|value| i32::try_from(value).ok()) {
            *operand = Operand::Mem(Addr { disp, scale: 1, ..Addr::default() }, None);
            return;
        }
    }
    if number(name).is_ok() || name.contains('@') || name == "." {
        return;
    }
    let named = Named { name: std::mem::take(name), addend: *addend };
    *operand = Operand::Mem(Addr { scale: 1, linked: true, ..Addr::default() }, Some(named));
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
    // The byte form of the checksum step has two encodings, one into a thirty two bit register
    // and one into a sixty four bit register with REX.W. Both leave the same zero extended value
    // behind, but the encoder's rows are told apart by the kinds of their operands and not by
    // their widths, so the second would come out as the first under a different register name
    // and the bytes would not be the ones gas writes. Nothing a compiler writes needs it, gcc's
    // `_mm_crc32_u8` included, so it is refused rather than given a row of its own.
    if matches!(word, "crc32" | "crc32b")
        && matches!(operands.last(), Some(Operand::Reg(_, Width::Quad)))
        && (word == "crc32b"
            || matches!(operands.first(), Some(Operand::Reg(_, Width::Byte) | Operand::High(_))))
    {
        return Err(format!(
            "'{word}' of a byte into a sixty four bit register is not written yet, and the thirty \
             two bit register of the same number gives the same answer"
        ));
    }
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
    // Intel's name for a widening move, which gas takes in AT&T too and reads both widths off the
    // registers, so `movzx %bl, %edi` is `movzbl`.
    if let (Some(kind @ ("movzx" | "movsx")), [from, to]) = (Some(word), operands) {
        let width = |operand: &Operand| match operand {
            Operand::Reg(_, width) => Some(*width),
            Operand::High(_) => Some(Width::Byte),
            _ => None,
        };
        if let (Some(from), Some(to)) = (width(from), width(to)) {
            let spelled = format!("mov{}{}{}", &kind[3..4], letter(from), letter(to));
            if let Some(row) = encoding(&spelled, &kinds, imm) {
                return Ok((spelled, row));
            }
        }
    }
    if let Some(width) = stated(word, operands)? {
        let letter = letter(width);
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

/// The letter gas puts on a mnemonic for an operand of that width.
fn letter(width: Width) -> char {
    match width {
        Width::Byte => 'b',
        Width::Word => 'w',
        Width::Long => 'l',
        Width::Quad => 'q',
    }
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
    // The checksum step reads a byte, a word, a long or a quad and accumulates it into a register
    // that is thirty two bits whatever it read, so `crc32 %sil, %eax` names two widths and is not
    // a mistake either. The letter gas puts on it is the width of what is read, which is the first
    // operand, and a first operand that is an address says nothing, which gas refuses too.
    if word == "crc32" {
        return Ok(match operands.first() {
            Some(Operand::Reg(_, width)) => Some(*width),
            Some(Operand::High(_)) => Some(Width::Byte),
            _ => None,
        });
    }
    // Port I/O names `dx` for the port, which is a word whatever is moved through it, so the width
    // is the accumulator's, which is the operand that is not the port.
    if matches!(word, "in" | "out") {
        let accumulator = if word == "in" { operands.last() } else { operands.first() };
        return Ok(match accumulator {
            Some(Operand::Reg(_, width)) => Some(*width),
            _ => None,
        });
    }
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

/// An operand without the mask AVX-512 writes after it, and the mask.
///
/// `%zmm0{%k1}{z}` is `zmm0` written under `k1` with what the mask leaves out zeroed, and the
/// braces are after whichever operand the instruction writes, which is the destination of a load
/// and the address of a store. `{%k0}` is refused the way gas refuses it, since zero in the field is
/// what no mask at all is written as.
fn masked(text: &str) -> Result<(&str, Option<Opmask>), String> {
    let Some(cut) = text.find('{') else { return Ok((text, None)) };
    let mut mask = Opmask::default();
    let mut rest = &text[cut..];
    while let Some(inside) = rest.strip_prefix('{') {
        let Some(end) = inside.find('}') else {
            return Err(format!("'{text}' opens a brace it does not close"));
        };
        match inside[..end].trim() {
            "z" => mask.zero = true,
            name => match name.strip_prefix("%k").and_then(|number| number.parse::<u8>().ok()) {
                Some(number @ 1..=7) => mask.k = number,
                _ => {
                    return Err(format!(
                        "'{name}' is not a mask an instruction can be written under"
                    ));
                }
            },
        }
        rest = inside[end + 1..].trim_start();
    }
    if !rest.is_empty() || mask.k == 0 {
        return Err(format!("'{text}' is not a mask an instruction can be written under"));
    }
    Ok((text[..cut].trim_end(), Some(mask)))
}

/// What the encoder is handed for one of these, with `standing` for an expression not worked out.
fn value(operand: &Operand, standing: i64) -> Value {
    match operand {
        Operand::Reg(reg, width) => Value::Reg(*reg, *width),
        Operand::High(reg) => Value::High(*reg),
        Operand::Xmm(reg) => Value::Xmm(*reg),
        Operand::Vector(number, length) => Value::Vector(*number, *length),
        Operand::Mask(number) => Value::Mask(*number),
        Operand::Stack(_) => Value::Stack,
        Operand::Control(number) => Value::Control(*number),
        Operand::Debug(number) => Value::Debug(*number),
        Operand::Seg(segment) => Value::Seg(*segment),
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
        // Arithmetic on numbers alone, `$~31` or `$(1 << 4)`, is a number too, and gas picks the
        // short form for it the same way.
        let rest = rest.trim();
        return Ok(number(rest)
            .ok()
            .or_else(|| crate::source::constant(rest))
            .map_or_else(|| Operand::Expr(rest.to_owned()), Operand::Imm));
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
    // The registers AVX-512 added, and the longer names of all of them, which only an EVEX or VEX
    // row takes. `xmm0` to `xmm15` stay the value above, since every row that takes them already
    // knows that one.
    for (prefix, length) in [("xmm", Length::Xmm), ("ymm", Length::Ymm), ("zmm", Length::Zmm)] {
        let Some(rest) = name.strip_prefix(prefix) else { continue };
        if let Ok(number) = rest.parse::<u8>() {
            if number < 32 && (rest == "0" || !rest.starts_with('0')) {
                return Ok(Operand::Vector(number, length));
            }
        }
    }
    if let Some(Ok(number)) = name.strip_prefix('k').map(str::parse::<u8>) {
        if number < 8 {
            return Ok(Operand::Mask(number));
        }
    }
    if let Some(segment) = Segment::named(name) {
        return Ok(Operand::Seg(segment));
    }
    // The control and debug registers, which only a move to or from a general purpose register
    // names. Sixteen of each have a number, though most of them are not there on any processor,
    // and which ones are is the processor's to refuse rather than this.
    type Make = fn(u8) -> Operand;
    let special: [(&str, Make); 3] =
        [("cr", Operand::Control), ("dr", Operand::Debug), ("db", Operand::Debug)];
    for (prefix, make) in special {
        let Some(rest) = name.strip_prefix(prefix) else { continue };
        if let Ok(number) = rest.parse::<u8>() {
            if number < 16 && (rest == "0" || !rest.starts_with('0')) {
                return Ok(make(number));
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
        // All six, though only `fs` and `gs` move an address in long mode. The other four are a
        // prefix byte the processor ignores there, which a kernel writes anyway: `%ds:` in front of
        // a load is how an alternative instruction is padded to the length of the one it replaces.
        let segment = name.strip_prefix('%').and_then(Segment::named);
        let Some(segment) = segment else {
            return Err(format!("'{name}' is not a segment this machine reaches through"));
        };
        addr.segment = Some(segment);
        rest = rest[cut + 1..].trim();
    }

    // The registers are in the last pair of brackets, and only when what is in them is registers:
    // `(0*16)(%rsp)` and `(K_table-8)(%rip)` have arithmetic in brackets in front of them, and
    // `(4*8)` on its own is a displacement with no register at all.
    let (front, inside) = match rest.find('(') {
        Some(_) => {
            let Some(cut) = grouped(rest) else {
                return Err(format!("'{text}' is not an address this compiler reads"));
            };
            let end = rest.len() - 1;
            let inside = rest[cut + 1..end].trim();
            if inside.is_empty() || inside.starts_with(['%', ',']) {
                (rest[..cut].trim(), Some(inside))
            } else {
                (rest.trim(), None)
            }
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
    let widths: Vec<Width> = parts
        .iter()
        .take(2)
        .filter_map(|part| part.strip_prefix('%').and_then(gpr_named))
        .map(|(_, width)| width)
        .collect();
    if widths.len() == 2 && widths[0] != widths[1] {
        return Err(format!("'{text}' adds registers of two widths"));
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

    // A name in an address that is not counted from the instruction is the name's address, which
    // the linker writes into four bytes of displacement however small the rest of it is. A suffix
    // asks for a table slot or a thread's offset, which is only reached from the instruction.
    if let Some(named) = &named {
        if !addr.rip {
            if let Some((_, how)) = named.name.split_once('@') {
                return Err(format!(
                    "'@{how}' in an address that is not counted from the instruction, which is \
                     not a way this compiler reaches anything"
                ));
            }
            addr.linked = true;
        }
    }
    if !addr.rip && addr.base.is_none() && addr.index.is_none() && named.is_none() {
        // A bare number in brackets is an address the machine holds outright, which is legal and
        // is not what a file writing one usually means, so it goes through rather than being
        // guessed at. What is refused above this is a bare name, which is the one that would need
        // a relocation.
    }
    Ok(Operand::Mem(addr, named))
}

/// Where the bracket that the last one in the text closes was opened, when the text ends in one.
fn grouped(text: &str) -> Option<usize> {
    let text = text.trim_end();
    if !text.ends_with(')') {
        return None;
    }
    let mut depth = 0usize;
    for (at, ch) in text.char_indices().rev() {
        match ch {
            ')' => depth += 1,
            '(' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// A register inside the brackets of an address, which has to be a whole one.
fn whole(text: &str) -> Result<PhysReg, String> {
    let Some(name) = text.strip_prefix('%') else {
        return Err(format!("'{text}' is not a register"));
    };
    match gpr_named(name) {
        // A thirty two bit one is the same register under the prefix [`one`] puts in front.
        Some((reg, Width::Quad | Width::Long)) => Ok(reg),
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
    if let Some(value) = crate::source::constant(text) {
        return Ok((value, None));
    }
    // Brackets round the whole of it say nothing, and `(K_table-8)` is `K_table-8`.
    if text.starts_with('(') && grouped(text) == Some(0) {
        return parted(&text[1..text.len() - 1]);
    }
    let mut total: i64 = 0;
    let mut sign: i64 = 1;
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut named: Option<String> = None;
    // A term may be arithmetic of its own, as `K256+8*16` is.
    let reckon = |term: &str| number(term).or_else(|why| crate::source::constant(term).ok_or(why));
    let mut fold = |term: &str, sign: i64, named: &mut Option<String>| match reckon(term) {
        Ok(value) => {
            total = total.wrapping_add(sign.wrapping_mul(value));
            Ok(())
        }
        // A bracketed term with a name in it, `((s1) + (64*8))`, which a macro that builds one
        // table's address from the one before it writes, is read the same way inside.
        Err(_) if term.trim().starts_with('(') && grouped(term.trim()) == Some(0) => {
            let (value, inner) = parted(term)?;
            total = total.wrapping_add(sign.wrapping_mul(value));
            match inner {
                Some(_) if named.is_some() => {
                    Err("two names added together, which is not a place a linker can find"
                        .to_owned())
                }
                Some(_) if sign < 0 => Err(format!("'{term}' takes a name away")),
                Some(inner) => {
                    *named = Some(inner);
                    Ok(())
                }
                None => Ok(()),
            }
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
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        // Not at the start of a term, where a sign belongs to the number behind it rather than
        // joining it to anything, and not inside brackets, where it is part of one term.
        if at == start || depth > 0 || !matches!(ch, '+' | '-') {
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
        // Another depth is the same opcode with the depth added in, and the top has to stay the
        // top.
        assert_eq!(bytes("fstp %st(1)"), [0xdd, 0xd9]);
        assert_eq!(bytes("faddp %st, %st(3)"), [0xde, 0xc3]);
        assert_eq!(bytes("fucomip %st(2), %st"), [0xdf, 0xea]);
        assert!(refused("faddp %st(1), %st(2)").contains("depths"));
        // What gcc writes for `fmod`.
        assert_eq!(bytes("fprem"), [0xd9, 0xf8]);
        assert_eq!(bytes("fnstsw %ax"), [0xdf, 0xe0]);
        assert!(refused("fnstsw %bx").contains("ax"));
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
        // Products, which busybox's SHA code writes so the offset lines up with the round, and
        // everything else a directive's expression may hold.
        assert_eq!(bytes("movdqu 80+0*16(%rdi), %xmm1"), bytes("movdqu 80(%rdi), %xmm1"));
        assert_eq!(bytes("movq 1*8(%r12), %rax"), bytes("movq 8(%r12), %rax"));
        assert_eq!(bytes("movq -2*8(%rsp), %rax"), bytes("movq -16(%rsp), %rax"));
        assert_eq!(bytes("movq 1<<4(%rdi), %rax"), bytes("movq 16(%rdi), %rax"));
        // And a name with a product added to it, which is how the same code finds its table.
        let read = |arg: &str| one("leaq", &[arg.to_owned(), "%rax".to_owned()]).expect("read");
        let product = read("K256+8*16(%rip)");
        let sum = read("K256+128(%rip)");
        assert_eq!(product.bytes, sum.bytes);
        assert_eq!(product.holes[0].name, "K256");
        assert_eq!(product.holes[0].addend, sum.holes[0].addend);
    }

    /// Bytes checked against GNU as.
    #[test]
    fn the_ssse3_sse41_and_sha_instructions_hand_written_code_uses_are_read() {
        assert_eq!(bytes("pshufb %xmm7, %xmm0"), [0x66, 0x0f, 0x38, 0x00, 0xc7]);
        assert_eq!(bytes("palignr $4, %xmm3, %xmm7"), [0x66, 0x0f, 0x3a, 0x0f, 0xfb, 0x04]);
        assert_eq!(bytes("pinsrd $3, 80(%rdi), %xmm1"), [0x66, 0x0f, 0x3a, 0x22, 0x4f, 0x50, 0x03]);
        assert_eq!(bytes("pextrd $3, %xmm1, 80(%rdi)"), [0x66, 0x0f, 0x3a, 0x16, 0x4f, 0x50, 0x03]);
        assert_eq!(bytes("pextrd $1, %xmm2, %eax"), [0x66, 0x0f, 0x3a, 0x16, 0xd0, 0x01]);
        assert_eq!(bytes("pinsrq $1, %rax, %xmm9"), [0x66, 0x4c, 0x0f, 0x3a, 0x22, 0xc8, 0x01]);
        assert_eq!(bytes("sha1rnds4 $1, %xmm2, %xmm0"), [0x0f, 0x3a, 0xcc, 0xc2, 0x01]);
        assert_eq!(bytes("sha1nexte %xmm3, %xmm1"), [0x0f, 0x38, 0xc8, 0xcb]);
        assert_eq!(bytes("sha1msg2 %xmm6, %xmm3"), [0x0f, 0x38, 0xca, 0xde]);
        assert_eq!(bytes("sha256msg1 %xmm4, %xmm3"), [0x0f, 0x38, 0xcc, 0xdc]);
        assert_eq!(bytes("sha256rnds2 %xmm0, %xmm1, %xmm2"), [0x0f, 0x38, 0xcb, 0xd1]);
        assert_eq!(bytes("sha256rnds2 %xmm1, %xmm2"), [0x0f, 0x38, 0xcb, 0xd1]);
        assert!(one("sha256rnds2", &["%xmm3".into(), "%xmm1".into(), "%xmm2".into()]).is_err());
    }

    /// Every SSE3, SSSE3, SSE4.1 and SSE4.2 instruction the intrinsic headers write, each with a
    /// register that needs a REX byte and with an address. The bytes are GNU as 2.46's.
    #[test]
    fn the_sse3_to_sse4_2_instructions_the_intrinsics_write_are_read() {
        let table: &[(&str, &[u8])] = &[
            ("addsubps %xmm2, %xmm9", &[0xf2, 0x44, 0x0f, 0xd0, 0xca]),
            ("addsubps 16(%rdi), %xmm1", &[0xf2, 0x0f, 0xd0, 0x4f, 0x10]),
            ("addsubpd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0xd0, 0xca]),
            ("addsubpd 16(%rdi), %xmm1", &[0x66, 0x0f, 0xd0, 0x4f, 0x10]),
            ("haddps %xmm2, %xmm9", &[0xf2, 0x44, 0x0f, 0x7c, 0xca]),
            ("haddps 16(%rdi), %xmm1", &[0xf2, 0x0f, 0x7c, 0x4f, 0x10]),
            ("haddpd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x7c, 0xca]),
            ("haddpd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x7c, 0x4f, 0x10]),
            ("hsubps %xmm2, %xmm9", &[0xf2, 0x44, 0x0f, 0x7d, 0xca]),
            ("hsubps 16(%rdi), %xmm1", &[0xf2, 0x0f, 0x7d, 0x4f, 0x10]),
            ("hsubpd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x7d, 0xca]),
            ("hsubpd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x7d, 0x4f, 0x10]),
            ("movshdup %xmm2, %xmm9", &[0xf3, 0x44, 0x0f, 0x16, 0xca]),
            ("movshdup 16(%rdi), %xmm1", &[0xf3, 0x0f, 0x16, 0x4f, 0x10]),
            ("movsldup %xmm2, %xmm9", &[0xf3, 0x44, 0x0f, 0x12, 0xca]),
            ("movsldup 16(%rdi), %xmm1", &[0xf3, 0x0f, 0x12, 0x4f, 0x10]),
            ("movddup %xmm2, %xmm9", &[0xf2, 0x44, 0x0f, 0x12, 0xca]),
            ("movddup 16(%rdi), %xmm1", &[0xf2, 0x0f, 0x12, 0x4f, 0x10]),
            ("phaddw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x01, 0xca]),
            ("phaddw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x01, 0x4f, 0x10]),
            ("phaddd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x02, 0xca]),
            ("phaddd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x02, 0x4f, 0x10]),
            ("phaddsw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x03, 0xca]),
            ("phaddsw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x03, 0x4f, 0x10]),
            ("pmaddubsw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x04, 0xca]),
            ("pmaddubsw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x04, 0x4f, 0x10]),
            ("phsubw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x05, 0xca]),
            ("phsubw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x05, 0x4f, 0x10]),
            ("phsubd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x06, 0xca]),
            ("phsubd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x06, 0x4f, 0x10]),
            ("phsubsw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x07, 0xca]),
            ("phsubsw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x07, 0x4f, 0x10]),
            ("psignb %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x08, 0xca]),
            ("psignb 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x08, 0x4f, 0x10]),
            ("psignw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x09, 0xca]),
            ("psignw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x09, 0x4f, 0x10]),
            ("psignd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x0a, 0xca]),
            ("psignd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x0a, 0x4f, 0x10]),
            ("pmulhrsw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x0b, 0xca]),
            ("pmulhrsw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x0b, 0x4f, 0x10]),
            ("pabsb %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x1c, 0xca]),
            ("pabsb 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x1c, 0x4f, 0x10]),
            ("pabsw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x1d, 0xca]),
            ("pabsw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x1d, 0x4f, 0x10]),
            ("pabsd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x1e, 0xca]),
            ("pabsd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x1e, 0x4f, 0x10]),
            ("pblendvb %xmm0, %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x10, 0xca]),
            ("pblendvb %xmm0, 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x10, 0x4f, 0x10]),
            ("blendvps %xmm0, %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x14, 0xca]),
            ("blendvps %xmm0, 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x14, 0x4f, 0x10]),
            ("blendvpd %xmm0, %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x15, 0xca]),
            ("blendvpd %xmm0, 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x15, 0x4f, 0x10]),
            ("ptest %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x17, 0xca]),
            ("ptest 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x17, 0x4f, 0x10]),
            ("pmovsxbw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x20, 0xca]),
            ("pmovsxbw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x20, 0x4f, 0x10]),
            ("pmovsxbd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x21, 0xca]),
            ("pmovsxbd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x21, 0x4f, 0x10]),
            ("pmovsxbq %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x22, 0xca]),
            ("pmovsxbq 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x22, 0x4f, 0x10]),
            ("pmovsxwd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x23, 0xca]),
            ("pmovsxwd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x23, 0x4f, 0x10]),
            ("pmovsxwq %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x24, 0xca]),
            ("pmovsxwq 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x24, 0x4f, 0x10]),
            ("pmovsxdq %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x25, 0xca]),
            ("pmovsxdq 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x25, 0x4f, 0x10]),
            ("pmuldq %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x28, 0xca]),
            ("pmuldq 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x28, 0x4f, 0x10]),
            ("pcmpeqq %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x29, 0xca]),
            ("pcmpeqq 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x29, 0x4f, 0x10]),
            ("packusdw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x2b, 0xca]),
            ("packusdw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x2b, 0x4f, 0x10]),
            ("pmovzxbw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x30, 0xca]),
            ("pmovzxbw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x30, 0x4f, 0x10]),
            ("pmovzxbd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x31, 0xca]),
            ("pmovzxbd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x31, 0x4f, 0x10]),
            ("pmovzxbq %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x32, 0xca]),
            ("pmovzxbq 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x32, 0x4f, 0x10]),
            ("pmovzxwd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x33, 0xca]),
            ("pmovzxwd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x33, 0x4f, 0x10]),
            ("pmovzxwq %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x34, 0xca]),
            ("pmovzxwq 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x34, 0x4f, 0x10]),
            ("pmovzxdq %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x35, 0xca]),
            ("pmovzxdq 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x35, 0x4f, 0x10]),
            ("pcmpgtq %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x37, 0xca]),
            ("pcmpgtq 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x37, 0x4f, 0x10]),
            ("pminsb %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x38, 0xca]),
            ("pminsb 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x38, 0x4f, 0x10]),
            ("pminsd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x39, 0xca]),
            ("pminsd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x39, 0x4f, 0x10]),
            ("pminuw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x3a, 0xca]),
            ("pminuw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x3a, 0x4f, 0x10]),
            ("pminud %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x3b, 0xca]),
            ("pminud 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x3b, 0x4f, 0x10]),
            ("pmaxsb %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x3c, 0xca]),
            ("pmaxsb 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x3c, 0x4f, 0x10]),
            ("pmaxsd %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x3d, 0xca]),
            ("pmaxsd 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x3d, 0x4f, 0x10]),
            ("pmaxuw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x3e, 0xca]),
            ("pmaxuw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x3e, 0x4f, 0x10]),
            ("pmaxud %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x3f, 0xca]),
            ("pmaxud 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x3f, 0x4f, 0x10]),
            ("pmulld %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x40, 0xca]),
            ("pmulld 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x40, 0x4f, 0x10]),
            ("phminposuw %xmm2, %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0x41, 0xca]),
            ("phminposuw 16(%rdi), %xmm1", &[0x66, 0x0f, 0x38, 0x41, 0x4f, 0x10]),
            ("roundps $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x08, 0xda, 0x05]),
            ("roundps $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x08, 0x08, 0x01]),
            ("roundpd $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x09, 0xda, 0x05]),
            ("roundpd $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x09, 0x08, 0x01]),
            ("roundss $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x0a, 0xda, 0x05]),
            ("roundss $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x0a, 0x08, 0x01]),
            ("roundsd $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x0b, 0xda, 0x05]),
            ("roundsd $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x0b, 0x08, 0x01]),
            ("blendps $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x0c, 0xda, 0x05]),
            ("blendps $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x0c, 0x08, 0x01]),
            ("blendpd $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x0d, 0xda, 0x05]),
            ("blendpd $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x0d, 0x08, 0x01]),
            ("pblendw $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x0e, 0xda, 0x05]),
            ("pblendw $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x0e, 0x08, 0x01]),
            ("insertps $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x21, 0xda, 0x05]),
            ("insertps $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x21, 0x08, 0x01]),
            ("dpps $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x40, 0xda, 0x05]),
            ("dpps $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x40, 0x08, 0x01]),
            ("dppd $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x41, 0xda, 0x05]),
            ("dppd $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x41, 0x08, 0x01]),
            ("mpsadbw $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x42, 0xda, 0x05]),
            ("mpsadbw $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x42, 0x08, 0x01]),
            ("pcmpestrm $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x60, 0xda, 0x05]),
            ("pcmpestrm $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x60, 0x08, 0x01]),
            ("pcmpestri $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x61, 0xda, 0x05]),
            ("pcmpestri $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x61, 0x08, 0x01]),
            ("pcmpistrm $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x62, 0xda, 0x05]),
            ("pcmpistrm $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x62, 0x08, 0x01]),
            ("pcmpistri $5, %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x3a, 0x63, 0xda, 0x05]),
            ("pcmpistri $1, (%r8), %xmm1", &[0x66, 0x41, 0x0f, 0x3a, 0x63, 0x08, 0x01]),
            ("lddqu (%rsi), %xmm12", &[0xf2, 0x44, 0x0f, 0xf0, 0x26]),
            ("movntdqa (%rsi), %xmm12", &[0x66, 0x44, 0x0f, 0x38, 0x2a, 0x26]),
            ("extractps $2, %xmm1, %eax", &[0x66, 0x0f, 0x3a, 0x17, 0xc8, 0x02]),
            ("extractps $3, %xmm9, %r10d", &[0x66, 0x45, 0x0f, 0x3a, 0x17, 0xca, 0x03]),
            ("extractps $1, %xmm2, 8(%rdi)", &[0x66, 0x0f, 0x3a, 0x17, 0x57, 0x08, 0x01]),
        ];
        for &(line, want) in table {
            assert_eq!(bytes(line), want, "{line}");
        }
        assert_eq!(bytes("pblendvb %xmm2, %xmm9"), bytes("pblendvb %xmm0, %xmm2, %xmm9"));
        assert!(refused("blendvps %xmm1, %xmm2, %xmm3").contains("xmm0"));
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
        let why = refused("movq (%ax), %rbx");
        assert!(why.contains("16 bits"), "{why}");
        let why = refused("movq (%rax,%ecx), %rbx");
        assert!(why.contains("two widths"), "{why}");
    }

    #[test]
    fn an_address_of_thirty_two_bit_registers_has_the_prefix_in_front() {
        // The CRC code multiplies an index by three this way. The segment goes first, as gas has it.
        assert_eq!(bytes("leal (%eax,%eax,2), %eax"), [0x67, 0x8d, 0x04, 0x40]);
        assert_eq!(bytes("movq (%eax), %rbx"), [0x67, 0x48, 0x8b, 0x18]);
        assert_eq!(bytes("movl %gs:(%edx), %eax"), [0x65, 0x67, 0x8b, 0x02]);
    }

    #[test]
    fn brackets_in_front_of_the_registers_are_part_of_the_displacement() {
        assert_eq!(bytes("movdqa (0*16)(%rsp), %xmm0"), bytes("movdqa 0(%rsp), %xmm0"));
        assert_eq!(bytes("movl 0 +4*(3)(%r12), %eax"), bytes("movl 12(%r12), %eax"));
        assert_eq!(bytes("movl (4*8), %eax"), bytes("movl 32, %eax"));
        let with = written("movdqa (K_table-8)(%rip), %xmm0");
        assert_eq!((with.holes[0].name.as_str(), with.holes[0].addend), ("K_table", -8));
        let nested = written("movq (((s1) + (64*8)) + (64*8))(,%rax,8), %rbx");
        assert_eq!((nested.holes[0].name.as_str(), nested.holes[0].addend), ("s1", 1024));
    }

    #[test]
    fn arithmetic_on_numbers_in_an_immediate_takes_the_short_form() {
        assert_eq!(bytes("andq $~31, %rsp"), [0x48, 0x83, 0xe4, 0xe0]);
        assert_eq!(bytes("subq $(4*8), %rsp"), [0x48, 0x83, 0xec, 0x20]);
    }

    #[test]
    fn movzx_and_movsx_read_both_widths_off_the_registers() {
        assert_eq!(bytes("movzx %bl, %edi"), bytes("movzbl %bl, %edi"));
        assert_eq!(bytes("movzx %ah, %esi"), bytes("movzbl %ah, %esi"));
        assert_eq!(bytes("movsx %cx, %rax"), bytes("movswq %cx, %rax"));
    }

    /// What a line was read as, holes and all.
    fn written(line: &str) -> Written {
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let args = crate::source::split(rest, ',');
        one(word, &args).unwrap_or_else(|why| panic!("{line}: {why}"))
    }

    #[test]
    fn a_name_in_an_address_that_is_not_counted_from_the_instruction_is_four_bytes_of_address() {
        // Four bytes however near the name turns out to be, and none of them guessed now. With a
        // register, with an index and no base, and with nothing at all, which is every shape gcc
        // writes for `-fno-pie`.
        for (line, at) in [
            ("movq message+8(%rbx), %rax", 3),
            ("movl table(,%rax,4), %eax", 3),
            ("movl counter, %eax", 3),
        ] {
            let written = written(line);
            assert_eq!(written.bytes.len(), at + 4, "{line}");
            let hole = &written.holes[0];
            assert_eq!((hole.at, hole.width, hole.sort), (at, 4, Sort::Extended), "{line}");
        }
        let hole = &written("movq message+8(%rbx), %rax").holes[0];
        assert_eq!((hole.name.as_str(), hole.addend), ("message", 8));
    }

    #[test]
    fn a_name_as_an_immediate_is_sign_extended_only_where_the_machine_extends_it() {
        // `movl` writes four bytes as they are and `movq` sign extends them to eight, which is the
        // difference between `R_X86_64_32` and `R_X86_64_32S`. `movabs` has room for all eight.
        for (line, width, sort) in [
            ("movl $.LC0, %edi", 4, Sort::Value),
            ("movq $.LC0, %rdi", 4, Sort::Extended),
            ("pushq $.LC0", 4, Sort::Extended),
            ("movabsq $.LC0, %rax", 8, Sort::Value),
        ] {
            let written = written(line);
            let hole = &written.holes[0];
            assert_eq!(hole.at + hole.width as usize, written.bytes.len(), "{line}");
            assert_eq!((hole.width, hole.sort), (width, sort), "{line}");
        }
    }

    #[test]
    fn a_name_in_an_address_asking_for_a_table_slot_without_the_instruction_is_refused() {
        let why = refused("movq message@GOTPCREL(%rbx), %rax");
        assert!(why.contains("@GOTPCREL"), "{why}");
    }

    #[test]
    fn a_scale_the_machine_does_not_have_is_refused() {
        let why = refused("movq (%rsi,%rdi,3), %rax");
        assert!(why.contains("scale"), "{why}");
    }

    #[test]
    fn the_checksum_step_takes_its_letter_from_what_it_reads() {
        // Two widths in one line, which is the instruction and not a disagreement. The bytes are
        // what GNU as 2.46 writes for the same lines.
        assert_eq!(bytes("crc32 %sil, %eax"), [0xf2, 0x40, 0x0f, 0x38, 0xf0, 0xc6]);
        assert_eq!(bytes("crc32 %cx, %eax"), [0x66, 0xf2, 0x0f, 0x38, 0xf1, 0xc1]);
        assert_eq!(bytes("crc32 %edx, %eax"), [0xf2, 0x0f, 0x38, 0xf1, 0xc2]);
        assert_eq!(bytes("crc32 %r9, %rax"), [0xf2, 0x49, 0x0f, 0x38, 0xf1, 0xc1]);
        assert_eq!(bytes("crc32l (%rdi), %eax"), [0xf2, 0x0f, 0x38, 0xf1, 0x07]);
        assert_eq!(bytes("popcnt %rdi, %rax"), [0xf3, 0x48, 0x0f, 0xb8, 0xc7]);
        // An address says nothing about how much of it is read, so the letter has to be written.
        assert!(refused("crc32 (%rdi), %eax").contains("crc32"));
        // A byte into a sixty four bit register is a REX.W form gas has and this does not, and
        // writing the thirty two bit form in its place would be different bytes.
        assert!(refused("crc32b %al, %rcx").contains("sixty four"));
        assert!(refused("crc32 %al, %rcx").contains("sixty four"));
    }

    #[test]
    fn an_instruction_this_compiler_has_no_bytes_for_is_refused_by_name() {
        // One nothing in the compiler writes and nothing in the encoder has a row for, which is a
        // set that shrinks every time a hand written file needs another one. It was `bswap` until
        // tamnd/rucc#1329 gave that one bytes and a population count until tamnd/rucc#2003 did the
        // same, and a round of AES until the kernel's crypto code needed one. It is a VNNI dot
        // product now, which nothing the kernel builds writes.
        let why = refused("vpdpbusd %zmm1, %zmm2, %zmm3");
        assert!(why.contains("vpdpbusd"), "{why}");
    }

    /// The AVX-512 instructions the intrinsics in the runtime headers write, each checked against
    /// what GNU as writes for the same line.
    #[test]
    fn the_avx512_instructions_the_intrinsics_write_are_read() {
        let lines: &[(&str, &[u8])] = &[
            ("vmovdqu64 (%rdi), %zmm16", &[0x62, 0xe1, 0xfe, 0x48, 0x6f, 0x07]),
            ("vmovdqu64 64(%rdi), %zmm17", &[0x62, 0xe1, 0xfe, 0x48, 0x6f, 0x4f, 0x01]),
            ("vmovdqu64 -128(%rsp), %zmm31", &[0x62, 0x61, 0xfe, 0x48, 0x6f, 0x7c, 0x24, 0xfe]),
            ("vmovdqu64 100(%rdi), %zmm16", &[0x62, 0xe1, 0xfe, 0x48, 0x6f, 0x87, 0x64, 0, 0, 0]),
            (
                "vmovdqu64 %zmm20, 64(%rsp,%rcx,8)",
                &[0x62, 0xe1, 0xfe, 0x48, 0x7f, 0x64, 0xcc, 0x01],
            ),
            ("vmovdqu64 %zmm8, %zmm25", &[0x62, 0x41, 0xfe, 0x48, 0x6f, 0xc8]),
            ("vmovdqu64 (%r12), %zmm9", &[0x62, 0x51, 0xfe, 0x48, 0x6f, 0x0c, 0x24]),
            ("vmovdqu64 %xmm16, (%rax)", &[0x62, 0xe1, 0xfe, 0x08, 0x7f, 0x00]),
            ("vmovdqu64 %ymm16, 32(%rax)", &[0x62, 0xe1, 0xfe, 0x28, 0x7f, 0x40, 0x01]),
            ("vmovdqa64 %xmm1, %xmm18", &[0x62, 0xe1, 0xfd, 0x08, 0x6f, 0xd1]),
            ("vmovdqu8 (%rbx), %zmm16{%k1}{z}", &[0x62, 0xe1, 0x7f, 0xc9, 0x6f, 0x03]),
            ("vmovdqu8 (%r9,%r10), %zmm30{%k7}{z}", &[0x62, 0x01, 0x7f, 0xcf, 0x6f, 0x34, 0x11]),
            ("vmovdqu8 (%rbx), %zmm16{%k1}", &[0x62, 0xe1, 0x7f, 0x49, 0x6f, 0x03]),
            ("vmovdqu8 %zmm16, (%rbx){%k2}", &[0x62, 0xe1, 0x7f, 0x4a, 0x7f, 0x03]),
            ("kmovq %rax, %k1", &[0xc4, 0xe1, 0xfb, 0x92, 0xc8]),
            ("kmovq %r13, %k7", &[0xc4, 0xc1, 0xfb, 0x92, 0xfd]),
            ("kmovq %k3, %r9", &[0xc4, 0x61, 0xfb, 0x93, 0xcb]),
            ("vpaddq %zmm16, %zmm17, %zmm18", &[0x62, 0xa1, 0xf5, 0x40, 0xd4, 0xd0]),
            ("vpaddq 128(%rax), %zmm17, %zmm18", &[0x62, 0xe1, 0xf5, 0x40, 0xd4, 0x50, 0x02]),
            ("vpaddq %xmm1, %xmm2, %xmm3", &[0xc5, 0xe9, 0xd4, 0xd9]),
            ("vpaddq %ymm9, %ymm10, %ymm11", &[0xc4, 0x41, 0x2d, 0xd4, 0xd9]),
            ("vpandq 64(%rdx), %zmm17, %zmm18", &[0x62, 0xe1, 0xf5, 0x40, 0xdb, 0x52, 0x01]),
            ("vpxorq %xmm16, %xmm17, %xmm18", &[0x62, 0xa1, 0xf5, 0x00, 0xef, 0xd0]),
            (
                "vpternlogq $0x96, 64(%rax), %zmm17, %zmm18",
                &[0x62, 0xe3, 0xf5, 0x40, 0x25, 0x50, 0x01, 0x96],
            ),
            (
                "vpternlogq $150, %xmm16, %xmm17, %xmm18",
                &[0x62, 0xa3, 0xf5, 0x00, 0x25, 0xd0, 0x96],
            ),
            ("vpopcntq (%rax), %zmm17", &[0x62, 0xe2, 0xfd, 0x48, 0x55, 0x08]),
            ("vpclmulqdq $17, %zmm16, %zmm17, %zmm18", &[0x62, 0xa3, 0x75, 0x40, 0x44, 0xd0, 0x11]),
            ("vpclmulqdq $0, %xmm1, %xmm2, %xmm3", &[0xc4, 0xe3, 0x69, 0x44, 0xd9, 0x00]),
            ("vextracti32x4 $3, %zmm16, %xmm1", &[0x62, 0xe3, 0x7d, 0x48, 0x39, 0xc1, 0x03]),
            (
                "vextracti32x4 $2, %zmm16, 32(%rax)",
                &[0x62, 0xe3, 0x7d, 0x48, 0x39, 0x40, 0x02, 0x02],
            ),
            ("vextracti64x4 $1, %zmm16, %ymm17", &[0x62, 0xa3, 0xfd, 0x48, 0x3b, 0xc1, 0x01]),
            ("vbroadcasti32x4 32(%rax), %zmm16", &[0x62, 0xe2, 0x7d, 0x48, 0x5a, 0x40, 0x02]),
            ("vpbroadcastb %eax, %zmm1", &[0x62, 0xf2, 0x7d, 0x48, 0x7a, 0xc8]),
            (
                "vshufi64x2 $0xb1, (%rax), %zmm16, %zmm17",
                &[0x62, 0xe3, 0xfd, 0x40, 0x43, 0x08, 0xb1],
            ),
            ("vpshufd $0x4e, %zmm16, %zmm17", &[0x62, 0xa1, 0x7d, 0x48, 0x70, 0xc8, 0x4e]),
            ("vpshufd $0x4e, %xmm1, %xmm2", &[0xc5, 0xf9, 0x70, 0xd1, 0x4e]),
            ("vmovq %xmm16, %rax", &[0x62, 0xe1, 0xfd, 0x08, 0x7e, 0xc0]),
            ("vmovq %xmm1, %rax", &[0xc4, 0xe1, 0xf9, 0x7e, 0xc8]),
            ("vmovq %rax, %xmm16", &[0x62, 0xe1, 0xfd, 0x08, 0x6e, 0xc0]),
        ];
        for (line, expected) in lines {
            assert_eq!(bytes(line), *expected, "{line}");
        }
    }

    /// AVX, AVX2, AES-NI, carry-less multiplication and the two BMI instructions the kernel's x86
    /// crypto code is written in, each against the bytes llvm-mc writes for the same line.
    #[test]
    fn the_vector_instructions_the_crypto_code_writes_are_read() {
        let lines: &[(&str, &[u8])] = &[
            ("aesenc %xmm1, %xmm2", &[0x66, 0x0f, 0x38, 0xdc, 0xd1]),
            ("aesenclast (%rdi), %xmm9", &[0x66, 0x44, 0x0f, 0x38, 0xdd, 0x0f]),
            ("aesdec %xmm10, %xmm3", &[0x66, 0x41, 0x0f, 0x38, 0xde, 0xda]),
            ("aesdeclast %xmm4, %xmm5", &[0x66, 0x0f, 0x38, 0xdf, 0xec]),
            ("aesimc 16(%rsi), %xmm6", &[0x66, 0x0f, 0x38, 0xdb, 0x76, 0x10]),
            ("aeskeygenassist $0x1b, %xmm1, %xmm2", &[0x66, 0x0f, 0x3a, 0xdf, 0xd1, 0x1b]),
            ("pclmulqdq $0x11, %xmm12, %xmm13", &[0x66, 0x45, 0x0f, 0x3a, 0x44, 0xec, 0x11]),
            ("pclmulqdq $0, (%rax), %xmm0", &[0x66, 0x0f, 0x3a, 0x44, 0x00, 0x00]),
            ("vpxor %xmm1, %xmm2, %xmm3", &[0xc5, 0xe9, 0xef, 0xd9]),
            ("vpxor %ymm9, %ymm10, %ymm11", &[0xc4, 0x41, 0x2d, 0xef, 0xd9]),
            ("vpxor (%rdi), %ymm1, %ymm2", &[0xc5, 0xf5, 0xef, 0x17]),
            ("vpor %ymm1, %ymm2, %ymm3", &[0xc5, 0xed, 0xeb, 0xd9]),
            // llvm-mc swaps the two sources here to get the short prefix and gas does not unless it is
            // asked to optimize, which the kernel does not.
            ("vpand %xmm8, %xmm1, %xmm1", &[0xc4, 0xc1, 0x71, 0xdb, 0xc8]),
            // gas does take the store form of a move, and llvm-mc agrees.
            ("vmovdqu %ymm9, %ymm1", &[0xc5, 0x7e, 0x7f, 0xc9]),
            ("vpandn %ymm1, %ymm12, %ymm2", &[0xc5, 0x9d, 0xdf, 0xd1]),
            ("vpaddb %ymm1, %ymm2, %ymm3", &[0xc5, 0xed, 0xfc, 0xd9]),
            ("vpaddd 32(%rsp), %ymm4, %ymm4", &[0xc5, 0xdd, 0xfe, 0x64, 0x24, 0x20]),
            ("vpsubd %xmm1, %xmm2, %xmm3", &[0xc5, 0xe9, 0xfa, 0xd9]),
            ("vpsubq %ymm1, %ymm2, %ymm3", &[0xc5, 0xed, 0xfb, 0xd9]),
            ("vpmuludq %ymm1, %ymm2, %ymm3", &[0xc5, 0xed, 0xf4, 0xd9]),
            ("vpcmpeqd %ymm1, %ymm2, %ymm3", &[0xc5, 0xed, 0x76, 0xd9]),
            ("vpcmpeqq %xmm1, %xmm2, %xmm3", &[0xc4, 0xe2, 0x69, 0x29, 0xd9]),
            ("vpcmpgtb %ymm1, %ymm2, %ymm3", &[0xc5, 0xed, 0x64, 0xd9]),
            ("vpunpckhdq %xmm1, %xmm2, %xmm3", &[0xc5, 0xe9, 0x6a, 0xd9]),
            ("vpunpckldq %ymm1, %ymm2, %ymm3", &[0xc5, 0xed, 0x62, 0xd9]),
            ("vpunpckhqdq %ymm1, %ymm2, %ymm3", &[0xc5, 0xed, 0x6d, 0xd9]),
            ("vpunpcklqdq %xmm11, %xmm12, %xmm13", &[0xc4, 0x41, 0x19, 0x6c, 0xeb]),
            ("vpslld %xmm1, %ymm2, %ymm3", &[0xc5, 0xed, 0xf2, 0xd9]),
            ("vpsrlq %xmm1, %xmm2, %xmm3", &[0xc5, 0xe9, 0xd3, 0xd9]),
            ("vpshufb %ymm1, %ymm2, %ymm3", &[0xc4, 0xe2, 0x6d, 0x00, 0xd9]),
            ("vaesenc %ymm1, %ymm2, %ymm3", &[0xc4, 0xe2, 0x6d, 0xdc, 0xd9]),
            ("vaesenclast %xmm1, %xmm2, %xmm3", &[0xc4, 0xe2, 0x69, 0xdd, 0xd9]),
            ("vaesdec %xmm1, %xmm2, %xmm3", &[0xc4, 0xe2, 0x69, 0xde, 0xd9]),
            ("vaesdeclast %ymm9, %ymm2, %ymm3", &[0xc4, 0xc2, 0x6d, 0xdf, 0xd9]),
            ("vpabsb %ymm1, %ymm2", &[0xc4, 0xe2, 0x7d, 0x1c, 0xd1]),
            ("vptest %ymm1, %ymm2", &[0xc4, 0xe2, 0x7d, 0x17, 0xd1]),
            ("vpbroadcastd %xmm1, %ymm2", &[0xc4, 0xe2, 0x7d, 0x58, 0xd1]),
            ("vpbroadcastq (%rdi), %ymm2", &[0xc4, 0xe2, 0x7d, 0x59, 0x17]),
            ("vbroadcastss (%rdi), %ymm8", &[0xc4, 0x62, 0x7d, 0x18, 0x07]),
            ("vaesimc %xmm1, %xmm2", &[0xc4, 0xe2, 0x79, 0xdb, 0xd1]),
            ("vpbroadcastb %xmm1, %ymm2", &[0xc4, 0xe2, 0x7d, 0x78, 0xd1]),
            ("vpbroadcastb (%rdi), %xmm2", &[0xc4, 0xe2, 0x79, 0x78, 0x17]),
            ("vmovq %xmm1, %xmm2", &[0xc5, 0xfa, 0x7e, 0xd1]),
            ("vmovq %xmm9, %xmm1", &[0xc5, 0x79, 0xd6, 0xc9]),
            ("vmovq (%rdi), %xmm1", &[0xc5, 0xfa, 0x7e, 0x0f]),
            ("vmovq %xmm9, 8(%rsp)", &[0xc5, 0x79, 0xd6, 0x4c, 0x24, 0x08]),
            ("vbroadcasti128 (%rdi), %ymm3", &[0xc4, 0xe2, 0x7d, 0x5a, 0x1f]),
            ("vmovdqu (%rdi), %ymm0", &[0xc5, 0xfe, 0x6f, 0x07]),
            ("vmovdqu %ymm0, (%rdi)", &[0xc5, 0xfe, 0x7f, 0x07]),
            ("vmovdqa %xmm1, %xmm2", &[0xc5, 0xf9, 0x6f, 0xd1]),
            ("vmovdqa 64(%rsp), %ymm13", &[0xc5, 0x7d, 0x6f, 0x6c, 0x24, 0x40]),
            ("vmovdqa %ymm13, 64(%rsp)", &[0xc5, 0x7d, 0x7f, 0x6c, 0x24, 0x40]),
            ("vmovups (%rsi), %xmm1", &[0xc5, 0xf8, 0x10, 0x0e]),
            ("vmovups %ymm1, (%rsi)", &[0xc5, 0xfc, 0x11, 0x0e]),
            ("vmovd %eax, %xmm1", &[0xc5, 0xf9, 0x6e, 0xc8]),
            ("vmovd %xmm1, %r8d", &[0xc4, 0xc1, 0x79, 0x7e, 0xc8]),
            ("vmovd (%rdi), %xmm1", &[0xc5, 0xf9, 0x6e, 0x0f]),
            ("vmovd %xmm1, (%rdi)", &[0xc5, 0xf9, 0x7e, 0x0f]),
            ("vpalignr $8, %ymm1, %ymm2, %ymm3", &[0xc4, 0xe3, 0x6d, 0x0f, 0xd9, 0x08]),
            ("vpblendd $0xf0, %ymm1, %ymm2, %ymm3", &[0xc4, 0xe3, 0x6d, 0x02, 0xd9, 0xf0]),
            ("vinserti128 $1, %xmm1, %ymm2, %ymm3", &[0xc4, 0xe3, 0x6d, 0x38, 0xd9, 0x01]),
            ("vinserti128 $1, (%rdi), %ymm2, %ymm3", &[0xc4, 0xe3, 0x6d, 0x38, 0x1f, 0x01]),
            ("vinsertf128 $1, %xmm1, %ymm2, %ymm3", &[0xc4, 0xe3, 0x6d, 0x18, 0xd9, 0x01]),
            ("vperm2i128 $0x20, %ymm1, %ymm2, %ymm3", &[0xc4, 0xe3, 0x6d, 0x46, 0xd9, 0x20]),
            ("vperm2f128 $0x31, %ymm1, %ymm2, %ymm3", &[0xc4, 0xe3, 0x6d, 0x06, 0xd9, 0x31]),
            ("vpslld $7, %ymm1, %ymm2", &[0xc5, 0xed, 0x72, 0xf1, 0x07]),
            ("vpsrld $25, %xmm1, %xmm2", &[0xc5, 0xe9, 0x72, 0xd1, 0x19]),
            ("vpsrad $3, %ymm1, %ymm2", &[0xc5, 0xed, 0x72, 0xe1, 0x03]),
            ("vpsllq $1, %ymm1, %ymm2", &[0xc5, 0xed, 0x73, 0xf1, 0x01]),
            ("vpsrlq $63, %xmm9, %xmm10", &[0xc4, 0xc1, 0x29, 0x73, 0xd1, 0x3f]),
            ("vpslldq $4, %ymm1, %ymm2", &[0xc5, 0xed, 0x73, 0xf9, 0x04]),
            ("vpsrldq $8, %xmm1, %xmm2", &[0xc5, 0xe9, 0x73, 0xd9, 0x08]),
            ("vextracti128 $1, %ymm1, %xmm2", &[0xc4, 0xe3, 0x7d, 0x39, 0xca, 0x01]),
            ("vextracti128 $1, %ymm1, (%rdi)", &[0xc4, 0xe3, 0x7d, 0x39, 0x0f, 0x01]),
            ("vpextrq $1, %xmm1, %rax", &[0xc4, 0xe3, 0xf9, 0x16, 0xc8, 0x01]),
            ("vpextrq $1, %xmm1, (%rdi)", &[0xc4, 0xe3, 0xf9, 0x16, 0x0f, 0x01]),
            ("vpinsrq $1, %rax, %xmm1, %xmm2", &[0xc4, 0xe3, 0xf1, 0x22, 0xd0, 0x01]),
            ("vpinsrq $1, (%rdi), %xmm1, %xmm2", &[0xc4, 0xe3, 0xf1, 0x22, 0x17, 0x01]),
            ("vzeroupper", &[0xc5, 0xf8, 0x77]),
            ("vzeroall", &[0xc5, 0xfc, 0x77]),
            ("rorx $2, %eax, %ebx", &[0xc4, 0xe3, 0x7b, 0xf0, 0xd8, 0x02]),
            ("rorx $13, %r9, %r10", &[0xc4, 0x43, 0xfb, 0xf0, 0xd1, 0x0d]),
            ("rorxq $6, (%rdi), %rax", &[0xc4, 0xe3, 0xfb, 0xf0, 0x07, 0x06]),
            ("andn %eax, %ebx, %ecx", &[0xc4, 0xe2, 0x60, 0xf2, 0xc8]),
            ("andn (%rdi), %r8, %r9", &[0xc4, 0x62, 0xb8, 0xf2, 0x0f]),
        ];
        for (line, expected) in lines {
            assert_eq!(bytes(line), *expected, "{line}");
        }
        // Only an EVEX prefix can name either of these, and these rows are VEX.
        assert!(refused("vpxor %ymm16, %ymm1, %ymm2").contains("VEX prefix"));
        assert!(refused("vmovdqu %zmm1, (%rdi)").contains("VEX prefix"));
    }

    #[test]
    fn a_mask_that_is_not_one_is_refused() {
        assert!(refused("vmovdqu8 (%rbx), %zmm16{%k0}").contains("mask"));
        assert!(refused("vmovdqu8 (%rbx), %zmm16{%k8}").contains("mask"));
        assert!(refused("vmovdqu8 (%rbx), %zmm16{%k1").contains("brace"));
        assert!(refused("addq %rax, %rbx{%k1}").contains("mask"));
        assert!(refused("paddq %xmm16, %xmm1").contains("argument"));
        assert!(refused("vmovdqu64 %zmm32, %zmm1").contains("register"));
    }

    /// The system instructions a kernel writes by hand, each against the bytes llvm-mc writes for
    /// the same line, except `int $3`, which gas writes as the two byte form and llvm-mc as `int3`.
    /// This follows gas.
    #[test]
    fn the_system_instructions_a_kernel_writes_come_out_as_gas_and_llvm_write_them() {
        let lines: &[(&str, &[u8])] = &[
            ("mov %cr0, %rax", &[0x0f, 0x20, 0xc0]),
            ("mov %rax, %cr0", &[0x0f, 0x22, 0xc0]),
            ("mov %cr2, %rdx", &[0x0f, 0x20, 0xd2]),
            ("mov %cr3, %r9", &[0x41, 0x0f, 0x20, 0xd9]),
            ("mov %r9, %cr3", &[0x41, 0x0f, 0x22, 0xd9]),
            ("mov %cr8, %rax", &[0x44, 0x0f, 0x20, 0xc0]),
            ("mov %rax, %cr8", &[0x44, 0x0f, 0x22, 0xc0]),
            ("movq %rax, %cr3", &[0x0f, 0x22, 0xd8]),
            ("mov %db0, %rax", &[0x0f, 0x21, 0xc0]),
            ("mov %dr7, %rax", &[0x0f, 0x21, 0xf8]),
            ("mov %rax, %dr7", &[0x0f, 0x23, 0xf8]),
            ("mov %r10, %dr1", &[0x41, 0x0f, 0x23, 0xca]),
            ("mov %ds, %eax", &[0x8c, 0xd8]),
            ("mov %ds, %ax", &[0x66, 0x8c, 0xd8]),
            ("mov %ds, %rax", &[0x48, 0x8c, 0xd8]),
            ("movl %ds, %eax", &[0x8c, 0xd8]),
            ("mov %es, %ecx", &[0x8c, 0xc1]),
            ("mov %ss, %eax", &[0x8c, 0xd0]),
            ("mov %fs, %eax", &[0x8c, 0xe0]),
            ("mov %gs, %r8d", &[0x41, 0x8c, 0xe8]),
            ("mov %cs, %eax", &[0x8c, 0xc8]),
            ("mov %eax, %ds", &[0x8e, 0xd8]),
            ("mov %ax, %ds", &[0x8e, 0xd8]),
            ("movl %eax, %ds", &[0x8e, 0xd8]),
            ("mov %rax, %ss", &[0x48, 0x8e, 0xd0]),
            ("mov %r8d, %gs", &[0x41, 0x8e, 0xe8]),
            ("movw %ax, %es", &[0x8e, 0xc0]),
            ("mov %ds, (%rax)", &[0x8c, 0x18]),
            ("movw %ds, (%rax)", &[0x8c, 0x18]),
            ("mov (%rax), %ds", &[0x8e, 0x18]),
            ("movw (%rax), %ds", &[0x8e, 0x18]),
            ("push %fs", &[0x0f, 0xa0]),
            ("push %gs", &[0x0f, 0xa8]),
            ("pop %fs", &[0x0f, 0xa1]),
            ("popq %gs", &[0x0f, 0xa9]),
            ("lgdt (%rax)", &[0x0f, 0x01, 0x10]),
            ("lidt (%rax)", &[0x0f, 0x01, 0x18]),
            ("sgdt (%rax)", &[0x0f, 0x01, 0x00]),
            ("sidt (%rax)", &[0x0f, 0x01, 0x08]),
            ("lidtq 8(%rsp)", &[0x0f, 0x01, 0x5c, 0x24, 0x08]),
            ("lldt %ax", &[0x0f, 0x00, 0xd0]),
            ("lldt (%rax)", &[0x0f, 0x00, 0x10]),
            ("sldt %ax", &[0x66, 0x0f, 0x00, 0xc0]),
            ("sldt %eax", &[0x0f, 0x00, 0xc0]),
            ("sldt %rax", &[0x48, 0x0f, 0x00, 0xc0]),
            ("sldt (%rax)", &[0x0f, 0x00, 0x00]),
            ("ltr %ax", &[0x0f, 0x00, 0xd8]),
            ("ltr %di", &[0x0f, 0x00, 0xdf]),
            ("ltr (%rax)", &[0x0f, 0x00, 0x18]),
            ("str %ax", &[0x66, 0x0f, 0x00, 0xc8]),
            ("str %eax", &[0x0f, 0x00, 0xc8]),
            ("str %rax", &[0x48, 0x0f, 0x00, 0xc8]),
            ("str (%rax)", &[0x0f, 0x00, 0x08]),
            ("verw (%rax)", &[0x0f, 0x00, 0x28]),
            ("verw %ax", &[0x0f, 0x00, 0xe8]),
            ("verr %ax", &[0x0f, 0x00, 0xe0]),
            ("lmsw %ax", &[0x0f, 0x01, 0xf0]),
            ("smsw %eax", &[0x0f, 0x01, 0xe0]),
            ("smsw %rax", &[0x48, 0x0f, 0x01, 0xe0]),
            ("smsw (%rax)", &[0x0f, 0x01, 0x20]),
            ("lsl %eax, %eax", &[0x0f, 0x03, 0xc0]),
            ("lsl %ax, %ax", &[0x66, 0x0f, 0x03, 0xc0]),
            ("lsl (%rax), %eax", &[0x0f, 0x03, 0x00]),
            ("lar %eax, %eax", &[0x0f, 0x02, 0xc0]),
            ("larl (%rdi), %eax", &[0x0f, 0x02, 0x07]),
            ("clts", &[0x0f, 0x06]),
            ("rdmsr", &[0x0f, 0x32]),
            ("wrmsr", &[0x0f, 0x30]),
            ("wrmsrns", &[0x0f, 0x01, 0xc6]),
            ("rdpmc", &[0x0f, 0x33]),
            ("rdtsc", &[0x0f, 0x31]),
            ("rdtscp", &[0x0f, 0x01, 0xf9]),
            ("swapgs", &[0x0f, 0x01, 0xf8]),
            ("sysretq", &[0x48, 0x0f, 0x07]),
            ("sysretl", &[0x0f, 0x07]),
            ("sysexitq", &[0x48, 0x0f, 0x35]),
            ("syscall", &[0x0f, 0x05]),
            ("sysenter", &[0x0f, 0x34]),
            ("iretq", &[0x48, 0xcf]),
            ("iret", &[0xcf]),
            ("iretw", &[0x66, 0xcf]),
            ("cli", &[0xfa]),
            ("sti", &[0xfb]),
            ("hlt", &[0xf4]),
            ("invlpg (%rax)", &[0x0f, 0x01, 0x38]),
            ("invlpg 8(%r12)", &[0x41, 0x0f, 0x01, 0x7c, 0x24, 0x08]),
            ("invpcid (%rax), %rdx", &[0x66, 0x0f, 0x38, 0x82, 0x10]),
            ("invpcid (%r8), %r9", &[0x66, 0x45, 0x0f, 0x38, 0x82, 0x08]),
            ("wbinvd", &[0x0f, 0x09]),
            ("wbnoinvd", &[0xf3, 0x0f, 0x09]),
            ("invd", &[0x0f, 0x08]),
            ("clflush (%rax)", &[0x0f, 0xae, 0x38]),
            ("clflushopt (%rax)", &[0x66, 0x0f, 0xae, 0x38]),
            ("clwb (%rax)", &[0x66, 0x0f, 0xae, 0x30]),
            ("clflush 64(%r13)", &[0x41, 0x0f, 0xae, 0x7d, 0x40]),
            ("xsave (%rdi)", &[0x0f, 0xae, 0x27]),
            ("xsave64 (%rdi)", &[0x48, 0x0f, 0xae, 0x27]),
            ("xsaveopt (%rdi)", &[0x0f, 0xae, 0x37]),
            ("xsaveopt64 (%rdi)", &[0x48, 0x0f, 0xae, 0x37]),
            ("xsaves (%rdi)", &[0x0f, 0xc7, 0x2f]),
            ("xsaves64 (%rdi)", &[0x48, 0x0f, 0xc7, 0x2f]),
            ("xsavec (%rdi)", &[0x0f, 0xc7, 0x27]),
            ("xsavec64 (%rdi)", &[0x48, 0x0f, 0xc7, 0x27]),
            ("xrstor (%rdi)", &[0x0f, 0xae, 0x2f]),
            ("xrstor64 (%rdi)", &[0x48, 0x0f, 0xae, 0x2f]),
            ("xrstors (%rdi)", &[0x0f, 0xc7, 0x1f]),
            ("xrstors64 (%rdi)", &[0x48, 0x0f, 0xc7, 0x1f]),
            ("fxsave (%rdi)", &[0x0f, 0xae, 0x07]),
            ("fxsave64 (%rdi)", &[0x48, 0x0f, 0xae, 0x07]),
            ("fxrstor (%rdi)", &[0x0f, 0xae, 0x0f]),
            ("fxrstorq (%rdi)", &[0x48, 0x0f, 0xae, 0x0f]),
            ("ldmxcsr 4(%rsp)", &[0x0f, 0xae, 0x54, 0x24, 0x04]),
            ("stmxcsr (%rdi)", &[0x0f, 0xae, 0x1f]),
            ("fnsave (%rdi)", &[0xdd, 0x37]),
            ("fsave (%rdi)", &[0x9b, 0xdd, 0x37]),
            ("frstor (%rdi)", &[0xdd, 0x27]),
            ("fldenv (%rdi)", &[0xd9, 0x27]),
            ("fnclex", &[0xdb, 0xe2]),
            ("fwait", &[0x9b]),
            ("emms", &[0x0f, 0x77]),
            ("stac", &[0x0f, 0x01, 0xcb]),
            ("clac", &[0x0f, 0x01, 0xca]),
            ("monitor", &[0x0f, 0x01, 0xc8]),
            ("mwait", &[0x0f, 0x01, 0xc9]),
            ("monitor %rax, %ecx, %edx", &[0x0f, 0x01, 0xc8]),
            ("mwait %eax, %ecx", &[0x0f, 0x01, 0xc9]),
            ("monitorx %rax, %ecx, %edx", &[0x0f, 0x01, 0xfa]),
            ("mwaitx %eax, %ecx, %ebx", &[0x0f, 0x01, 0xfb]),
            ("lfence", &[0x0f, 0xae, 0xe8]),
            ("sfence", &[0x0f, 0xae, 0xf8]),
            ("ud1 %eax, %ecx", &[0x0f, 0xb9, 0xc8]),
            ("ud1l (%rax), %ecx", &[0x0f, 0xb9, 0x08]),
            ("int3", &[0xcc]),
            ("int $0x80", &[0xcd, 0x80]),
            ("int $3", &[0xcd, 0x03]),
            ("int1", &[0xf1]),
            ("ljmp *(%rax)", &[0xff, 0x28]),
            ("ljmpq *(%rax)", &[0x48, 0xff, 0x28]),
            ("lcall *(%rax)", &[0xff, 0x18]),
            ("lcallq *8(%rsp)", &[0x48, 0xff, 0x5c, 0x24, 0x08]),
            ("lret", &[0xcb]),
            ("lretq", &[0x48, 0xcb]),
            ("lret $8", &[0xca, 0x08, 0x00]),
            ("lretq $8", &[0x48, 0xca, 0x08, 0x00]),
            ("rdfsbase %rax", &[0xf3, 0x48, 0x0f, 0xae, 0xc0]),
            ("rdfsbase %eax", &[0xf3, 0x0f, 0xae, 0xc0]),
            ("rdgsbase %rax", &[0xf3, 0x48, 0x0f, 0xae, 0xc8]),
            ("wrfsbase %rax", &[0xf3, 0x48, 0x0f, 0xae, 0xd0]),
            ("wrgsbase %r9", &[0xf3, 0x49, 0x0f, 0xae, 0xd9]),
            ("wrfsbase %edi", &[0xf3, 0x0f, 0xae, 0xd7]),
            ("endbr32", &[0xf3, 0x0f, 0x1e, 0xfb]),
            ("serialize", &[0x0f, 0x01, 0xe8]),
            ("rdrand %rax", &[0x48, 0x0f, 0xc7, 0xf0]),
            ("rdrand %ax", &[0x66, 0x0f, 0xc7, 0xf0]),
            ("rdrand %r10", &[0x49, 0x0f, 0xc7, 0xf2]),
            ("rdseed %eax", &[0x0f, 0xc7, 0xf8]),
            ("rdpid %r9", &[0xf3, 0x41, 0x0f, 0xc7, 0xf9]),
            ("rdpkru", &[0x0f, 0x01, 0xee]),
            ("wrpkru", &[0x0f, 0x01, 0xef]),
            ("vmcall", &[0x0f, 0x01, 0xc1]),
            ("vmmcall", &[0x0f, 0x01, 0xd9]),
            ("vmlaunch", &[0x0f, 0x01, 0xc2]),
            ("vmresume", &[0x0f, 0x01, 0xc3]),
            ("vmxoff", &[0x0f, 0x01, 0xc4]),
            ("vmfunc", &[0x0f, 0x01, 0xd4]),
            ("vmrun", &[0x0f, 0x01, 0xd8]),
            ("vmload %rax", &[0x0f, 0x01, 0xda]),
            ("vmsave", &[0x0f, 0x01, 0xdb]),
            ("stgi", &[0x0f, 0x01, 0xdc]),
            ("clgi", &[0x0f, 0x01, 0xdd]),
            ("skinit", &[0x0f, 0x01, 0xde]),
            ("invlpga %rax, %ecx", &[0x0f, 0x01, 0xdf]),
            ("vmgexit", &[0xf3, 0x0f, 0x01, 0xd9]),
            ("vmxon (%rax)", &[0xf3, 0x0f, 0xc7, 0x30]),
            ("vmclear (%rax)", &[0x66, 0x0f, 0xc7, 0x30]),
            ("vmptrld (%rax)", &[0x0f, 0xc7, 0x30]),
            ("vmptrst (%rax)", &[0x0f, 0xc7, 0x38]),
            ("vmread %rax, %rbx", &[0x0f, 0x78, 0xc3]),
            ("vmread %rax, (%rbx)", &[0x0f, 0x78, 0x03]),
            ("vmwrite %rbx, %rax", &[0x0f, 0x79, 0xc3]),
            ("vmwrite (%rbx), %rax", &[0x0f, 0x79, 0x03]),
            ("invept (%rax), %rdx", &[0x66, 0x0f, 0x38, 0x80, 0x10]),
            ("invvpid (%rax), %rdx", &[0x66, 0x0f, 0x38, 0x81, 0x10]),
            ("tdcall", &[0x66, 0x0f, 0x01, 0xcc]),
            ("seamcall", &[0x66, 0x0f, 0x01, 0xcf]),
            ("seamret", &[0x66, 0x0f, 0x01, 0xcd]),
            ("seamops", &[0x66, 0x0f, 0x01, 0xce]),
            ("encls", &[0x0f, 0x01, 0xcf]),
            ("enclu", &[0x0f, 0x01, 0xd7]),
            ("enclv", &[0x0f, 0x01, 0xc0]),
            ("pconfig", &[0x0f, 0x01, 0xc5]),
            ("pvalidate", &[0xf2, 0x0f, 0x01, 0xff]),
            ("rmpadjust", &[0xf3, 0x0f, 0x01, 0xfe]),
            ("rmpupdate", &[0xf2, 0x0f, 0x01, 0xfe]),
            ("psmash", &[0xf3, 0x0f, 0x01, 0xff]),
            ("enqcmds (%rsi), %rdi", &[0xf3, 0x0f, 0x38, 0xf8, 0x3e]),
            ("enqcmd (%rsi), %rdi", &[0xf2, 0x0f, 0x38, 0xf8, 0x3e]),
            ("movdir64b (%rsi), %rdi", &[0x66, 0x0f, 0x38, 0xf8, 0x3e]),
            ("movdiri %eax, (%rdi)", &[0x0f, 0x38, 0xf9, 0x07]),
            ("movdiri %rax, (%rdi)", &[0x48, 0x0f, 0x38, 0xf9, 0x07]),
            ("wrussq %rax, (%rdi)", &[0x66, 0x48, 0x0f, 0x38, 0xf5, 0x07]),
            ("wrussd %eax, (%rdi)", &[0x66, 0x0f, 0x38, 0xf5, 0x07]),
            ("wrssq %rax, (%rdi)", &[0x48, 0x0f, 0x38, 0xf6, 0x07]),
            ("wrssd %eax, (%rdi)", &[0x0f, 0x38, 0xf6, 0x07]),
            ("rdsspq %rax", &[0xf3, 0x48, 0x0f, 0x1e, 0xc8]),
            ("rdsspd %eax", &[0xf3, 0x0f, 0x1e, 0xc8]),
            ("incsspq %rax", &[0xf3, 0x48, 0x0f, 0xae, 0xe8]),
            ("incsspd %eax", &[0xf3, 0x0f, 0xae, 0xe8]),
            ("rstorssp (%rax)", &[0xf3, 0x0f, 0x01, 0x28]),
            ("saveprevssp", &[0xf3, 0x0f, 0x01, 0xea]),
            ("setssbsy", &[0xf3, 0x0f, 0x01, 0xe8]),
            ("clrssbsy (%rax)", &[0xf3, 0x0f, 0xae, 0x30]),
            ("eretu", &[0xf3, 0x0f, 0x01, 0xca]),
            ("erets", &[0xf2, 0x0f, 0x01, 0xca]),
            ("xsetbv", &[0x0f, 0x01, 0xd1]),
            ("xend", &[0x0f, 0x01, 0xd5]),
            ("xtest", &[0x0f, 0x01, 0xd6]),
            ("tpause %ecx", &[0x66, 0x0f, 0xae, 0xf1]),
            ("umwait %ecx", &[0xf2, 0x0f, 0xae, 0xf1]),
            ("umonitor %rax", &[0xf3, 0x0f, 0xae, 0xf0]),
            ("rsm", &[0x0f, 0xaa]),
            ("lahf", &[0x9f]),
            ("sahf", &[0x9e]),
            ("cld", &[0xfc]),
            ("std", &[0xfd]),
            ("inb %dx, %al", &[0xec]),
            ("inw %dx, %ax", &[0x66, 0xed]),
            ("inl %dx, %eax", &[0xed]),
            ("inb $0x80, %al", &[0xe4, 0x80]),
            ("inw $0x80, %ax", &[0x66, 0xe5, 0x80]),
            ("inl $0x80, %eax", &[0xe5, 0x80]),
            ("in %dx, %al", &[0xec]),
            ("in $0x71, %al", &[0xe4, 0x71]),
            ("outb %al, %dx", &[0xee]),
            ("outw %ax, %dx", &[0x66, 0xef]),
            ("outl %eax, %dx", &[0xef]),
            ("outb %al, $0x80", &[0xe6, 0x80]),
            ("outw %ax, $0x80", &[0x66, 0xe7, 0x80]),
            ("outl %eax, $0x80", &[0xe7, 0x80]),
            ("out %al, $0x80", &[0xe6, 0x80]),
            ("out %eax, %dx", &[0xef]),
            ("inb (%dx), %al", &[0xec]),
            ("outb %al, (%dx)", &[0xee]),
            ("insb", &[0x6c]),
            ("insw", &[0x66, 0x6d]),
            ("insl", &[0x6d]),
            ("outsb", &[0x6e]),
            ("outsw", &[0x66, 0x6f]),
            ("outsl", &[0x6f]),
            ("cs", &[0x2e]),
            ("ds", &[0x3e]),
            ("es", &[0x26]),
            ("ss", &[0x36]),
            ("fs", &[0x64]),
            ("gs", &[0x65]),
            ("data16", &[0x66]),
            ("addr32", &[0x67]),
            ("rex64", &[0x48]),
            ("xacquire", &[0xf2]),
            ("xrelease", &[0xf3]),
            ("notrack", &[0x3e]),
            ("movl %ds:8(%rax), %eax", &[0x3e, 0x8b, 0x40, 0x08]),
            ("movl %es:(%rdi), %eax", &[0x26, 0x8b, 0x07]),
            ("movl %cs:(%rax), %eax", &[0x2e, 0x8b, 0x00]),
            ("movl %ss:(%rsp), %eax", &[0x36, 0x8b, 0x04, 0x24]),
        ];
        for (line, expected) in lines {
            assert_eq!(bytes(line), *expected, "{line}");
        }
    }

    /// A system instruction that names a register the processor does not use for it is refused,
    /// since the bytes would be the same and the line would say something they do not do.
    #[test]
    fn a_system_instruction_naming_the_wrong_register_is_refused() {
        assert!(refused("monitor %rbx, %ecx, %edx").contains("%rax, %ecx, %edx"));
        assert!(refused("mwait %eax, %edx").contains("%eax, %ecx"));
        assert!(refused("inb %dx, %bl").contains("%al"));
        assert!(refused("inb %cx, %al").contains("%dx"));
        assert!(refused("inw %dx, %al").contains("as wide"));
        assert!(refused("outl %rax, %dx").contains("%eax"));
        assert!(refused("movq %cr0, %eax").contains("sixty four"));
        assert!(refused("movw %ds, %eax").contains("width"));
        assert!(refused("push %ds").contains("fs and gs"));
        assert!(refused("pop %es").contains("fs and gs"));
        assert!(refused("movl %xs:(%rax), %eax").contains("segment"));
    }

    #[test]
    fn a_byte_from_0x40_to_0x4f_is_read_as_a_prefix_only_where_it_is_one() {
        // `addq $big, %rax` sign extends its four bytes in sixty four bit mode because of the
        // REX byte in front. The same bytes in thirty two bit mode are `decl %eax` and then an
        // add of four bytes into `eax`, and nothing there is eight bytes.
        let add = [0x48, 0x81, 0xC0, 0, 0, 0, 0];
        assert!(extends(&add, Mode::Bits64));
        assert!(!extends(&add, Mode::Bits32));
        assert!(extends(&[0x68, 0, 0, 0, 0], Mode::Bits64));
        assert!(!extends(&[0x68, 0, 0, 0, 0], Mode::Bits32));

        // `cmpq $1000, %rax` takes the accumulator's short form behind its REX byte, and in
        // thirty two bit mode the byte in front is an instruction and the rest is left alone.
        let cmp = vec![0x48, 0x81, 0xF8, 0xE8, 0x03, 0, 0];
        let mut long = cmp.clone();
        shorter(&mut long, &[], Mode::Bits64);
        assert_eq!(long, [0x48, 0x3D, 0xE8, 0x03, 0, 0]);
        let mut legacy = cmp.clone();
        shorter(&mut legacy, &[], Mode::Bits32);
        assert_eq!(legacy, cmp);
    }
}
