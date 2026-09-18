//! An `asm` template read back into the instructions of this machine.
//!
//! Design: `spec/11-asm-objects-debug.md` sections 11.1 and 11.2.
//!
//! A compiler that prints assembly for somebody else to assemble copies a template into its output
//! and never looks at it. This one writes the object file, so there is nobody else to hand the
//! text to and a template has to be read. Section 11.2 says as much about the templates at file
//! scope, and the same line applies to the ones inside a function: what the program wrote is an
//! instruction, and an instruction is something this compiler already has a description of.
//!
//! So this is not a second description of the machine. It is [`written`] walked backwards:
//! the text says a mnemonic and some arguments, [`machine`] finds the one opcode that is written
//! that way, and what comes back is that opcode with the template's operands put where the opcode
//! holds them. The listing, the bytes and the template are then three readings of one table, and
//! the thing section 11.1 is written to prevent, which is two descriptions drifting apart, cannot
//! happen here because there is still only one.
//!
//! # What is read and what is not
//!
//! One instruction per line, separated by newlines or semicolons, in AT&T syntax. An argument is a
//! register the template named, an operand of the statement written `%0`, a number written `$5`,
//! or an address written `8(%rbp)` with an optional `%fs:` or `%gs:` in front of it.
//!
//! Everything else is nothing at all rather than a guess, and the caller turns that into a refusal
//! that names the statement. Labels are not read, because a label inside a function is a place
//! something can jump to and the block layout has already decided where the places are. Directives
//! are not read either, with one exception, which is [`alignment`] and has a section of its own
//! below. The scaled index of an addressing mode is not read. Neither is an instruction whose opcode has
//! an operand nothing at all says anything about, which [`machine`] explains. Two things do say.
//! An operand the description fixes to a register is read, because there is only one register it
//! could be, and it comes back as a [`Piece::Implicit`] for the caller to say whether anything of
//! the program's is in. An operand the description ties to another is read as whatever that other
//! one is, because a tie is the description saying the two are one register: that is what makes
//! `addq %1, %0` and `cmova %3, %0` instructions rather than half of one. The rule throughout is
//! that an instruction this compiler cannot place is refused out loud, since the alternative is a
//! program that assembles into something other than what it says.
//!
//! # The suffix a template leaves off
//!
//! AT&T puts the width on the mnemonic and an assembler lets a program leave it off when the
//! arguments say it anyway, so `cmp %1, %2` is `cmpl` when those two operands are `int`. That is
//! read here too, and the width comes from the caller: the statement's operands have C types, the
//! caller knows them, and it passes their widths in. Every register argument has to agree on one
//! width, since two that disagree are an instruction the program will have to spell out itself.
//!
//! # The one directive that is read
//!
//! An alignment, which is `.p2align`, `.align` and `.balign`, and which is not an instruction at
//! all. It says that whatever comes after it begins at an address that is a multiple of a number,
//! and it is the one thing a template asks for that is about where an instruction is rather than
//! about what one does.
//!
//! It is read rather than refused because a program that writes one measured something. zstd puts
//! `.p2align 5` in front of the match loop of `ZSTD_compressBlock_lazy_generic` with a comment
//! saying it measured a five per cent loss on two compression levels when the loop moved across a
//! cache line boundary, which is a program asking to be insulated from a compiler's layout rather
//! than asking for anything to be computed. The right answer to that is to do what it says.
//!
//! What comes back is [`ALIGN`] with the boundary in bytes on it, and everything downstream treats
//! it the way it treats a fence: an instruction moved across one is an alignment of something other
//! than what the program pointed at.

use crate::operand::{Constraint, OperandDesc};
use crate::regs::{PhysReg, Segment};
use crate::x86_64::insts::{ALIGN, form};
use crate::x86_64::text::{Arg, Shape, Width, gpr_named, machine, written};

/// What one operand of an instruction in a template is filled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    /// `%0`, which is the statement's operand at that index.
    ///
    /// Which register that is has not been decided here and is not this file's business. The
    /// constraint said a register and said nothing about which one, so the answer comes from the
    /// allocator like every other answer of that shape.
    Operand {
        /// Its place in the list the constraints number, which is the outputs and then the inputs.
        index: usize,
        /// How much of the register the instruction uses, which the opcode says and the template
        /// does not.
        width: Width,
    },
    /// A register the instruction uses without its text saying so.
    ///
    /// The description fixes the operand to it, which is the whole of why this can be filled in at
    /// all, and `cpuid` is six of them: the leaf in `eax`, the subleaf in `ecx` and the answer in
    /// all four registers. Whether the statement has anything in that register is not read here
    /// and is not this file's business, since the thing that says so is the constraint beside the
    /// template rather than the template.
    Implicit {
        /// The register, which is the whole of what the description says about it.
        reg: PhysReg,
    },
    /// `%rax`, a register the template named itself.
    ///
    /// The allocator is told about it as a fixed register rather than being free to choose, which
    /// is the whole reason a program writes one down.
    Reg {
        /// The register.
        reg: PhysReg,
        /// How much of it, which here is both what the opcode says and what the name said, and the
        /// two have to agree or the instruction is not the one the opcode describes.
        width: Width,
    },
}

/// An address an instruction in a template reads or writes.
///
/// No scaled index. See the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct At {
    /// Which storage it is counted from, when it is not the flat one.
    pub segment: Option<Segment>,
    /// How far into it.
    pub disp: i32,
    /// The register the distance is from, when there is one. An address with none is a number.
    pub base: Option<Piece>,
}

/// One instruction of a template, as the machine holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The opcode, spelled the way the machine IR spells it, so `mov_rm_64` and not `x64.mov_rm_64`.
    pub opcode: &'static str,
    /// What fills each of the opcode's operands, in the opcode's order and not the template's.
    pub operands: Vec<Piece>,
    /// The address, for an opcode that has one.
    pub at: Option<At>,
    /// The number on the instruction, for an opcode that has one.
    pub imm: Option<i64>,
}

/// Every instruction in that template, or nothing when any of them is not one this can place.
///
/// Nothing rather than a partial answer, because half a template is not a smaller program, it is a
/// different one.
///
/// The widths are the statement's operands, in the order the constraints number them, and they are
/// what a mnemonic written without its suffix is read at. Nothing for an operand whose type is no
/// width a register here has, which is a `long double` or anything larger than a register, and an
/// operand like that is one a template has to spell the suffix out for. A statement with no
/// operands at all passes an empty slice and is in the same position.
#[must_use]
pub fn read(template: &str, widths: &[Option<Width>]) -> Option<Vec<Line>> {
    let mut lines = Vec::new();
    let mut carried = false;
    for text in template.split(['\n', ';']) {
        let text = uncommented(text).trim();
        if text.is_empty() {
            continue;
        }
        // A prefix on a line of its own, which is how a template written with semicolons between
        // its parts arrives here: `rep; nop` is two of these and one instruction. A second prefix
        // in a row is not something this reads, since the only combination it knows is the one
        // below and that one takes a single prefix.
        if is_repeat(text) {
            if carried {
                return None;
            }
            carried = true;
            continue;
        }
        // A directive before an instruction, because a directive is not a mnemonic and would be
        // refused by the one below. Only the alignments are read and everything else falls through
        // to that refusal, which is what a template asking for a section or a symbol gets. A repeat
        // prefix in front of one is half an instruction and is refused like any other.
        if let Some(line) = alignment(text) {
            if carried {
                return None;
            }
            lines.push(line);
            continue;
        }
        lines.push(instruction(text, carried, widths)?);
        carried = false;
    }
    // A prefix with nothing behind it is half an instruction, and half a template is refused for
    // the reason the whole of one is.
    if carried { None } else { Some(lines) }
}

/// The alignment that line asks for, or nothing for a line that is not one of the three that ask.
///
/// The three are one request spelled three ways and the difference between them is what the number
/// means. `.p2align` counts in powers of two, `.balign` counts in bytes, and `.align` is whichever
/// of the two the target decided, which on x86 with an ELF assembler is bytes. What comes back is
/// always the boundary in bytes, so the three spellings are one [`Line`] and nothing downstream has
/// to know which was written.
///
/// A second argument is refused rather than ignored, and there are two of them an assembler takes.
/// The first is the byte to fill with, which this decides rather than the program: what a gap in the
/// middle of a function is reached by is falling into it, so it has to be filled with something that
/// does nothing and a program that asked for anything else asked for an instruction stream this
/// cannot write. The second is a limit on how far to skip, which says to align only when it is cheap
/// enough, and that is a different request from this one rather than a decoration on it.
fn alignment(text: &str) -> Option<Line> {
    let (name, rest) = text.split_once(char::is_whitespace)?;
    let rest = rest.trim();
    if rest.contains(',') {
        return None;
    }
    let number: u32 = rest.parse().ok()?;
    let bytes = match name {
        // A power of two, and one too large to be a boundary on any machine is refused here rather
        // than overflowing into one that is.
        ".p2align" => 1u32.checked_shl(number).filter(|&bytes| bytes <= MOST)?,
        ".align" | ".balign" => number,
        _ => return None,
    };
    if !bytes.is_power_of_two() || bytes > MOST {
        return None;
    }
    Some(Line { opcode: ALIGN, operands: Vec::new(), at: None, imm: Some(i64::from(bytes)) })
}

/// The largest boundary a template may ask for, which is a page.
///
/// A number rather than no limit at all, because the padding is written into the function and a
/// boundary larger than the pages the program is loaded in is a request the section alignment cannot
/// carry anyway. Nothing real asks for more: what programs ask for is a cache line or two.
const MOST: u32 = 4096;

/// Whether a word is the repeat prefix, in any of the spellings that mean the same thing.
///
/// `repne` and `repnz` are not here. They are the other repeat prefix, they mean something
/// different, and the one instruction this compiler reads a prefix in front of is not one they
/// are ever written with.
fn is_repeat(text: &str) -> bool {
    matches!(text.trim(), "rep" | "repe" | "repz")
}

/// One line with anything a comment starts taken off the end of it.
fn uncommented(text: &str) -> &str {
    let end = text.find('#').into_iter().chain(text.find("//")).min();
    end.map_or(text, |at| &text[..at])
}

/// One instruction, or nothing for one this cannot place.
///
/// The flag says a repeat prefix was written in front of it, either on a line of its own or as
/// the first word of this one. Exactly one combination of a prefix and an instruction is read,
/// and it is `rep nop`: that is the encoding of `pause`, assemblers have always taken it as one,
/// and it is what a program writes when it wants the hint on a processor whose assembler is older
/// than the mnemonic. libuv writes it that way and puts `a.k.a. PAUSE` in the comment beside it.
/// A prefix on anything else is refused, `lock` included, because a prefix that changes what an
/// instruction does is not something to guess at: dropping the `lock` off a read modify write
/// would turn a program that is correct into one that is nearly always correct.
fn instruction(text: &str, prefixed: bool, widths: &[Option<Width>]) -> Option<Line> {
    let (mnemonic, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    // The same prefix written on the same line as what it applies to, which is the other way a
    // template writes it and is the same instruction.
    if is_repeat(mnemonic) {
        if prefixed {
            return None;
        }
        return instruction(rest.trim(), true, widths);
    }
    let mnemonic = if prefixed {
        if mnemonic != "nop" || !rest.trim().is_empty() {
            return None;
        }
        "pause"
    } else {
        mnemonic
    };
    // A label ends in a colon and a directive starts with a dot, and neither is an instruction.
    // Both are caught here rather than being looked for, because a mnemonic is letters and digits
    // and nothing else, so anything carrying punctuation is already not one.
    if mnemonic.is_empty() || !mnemonic.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }

    let given: Vec<Given> =
        arguments(rest).iter().map(|text| given(text)).collect::<Option<_>>()?;
    let shapes: Vec<Shape> = given.iter().map(Given::shape).collect();
    let opcode = match machine(mnemonic, &shapes) {
        Some(opcode) => opcode,
        None => machine(&suffixed(mnemonic, &given, widths)?, &shapes)?,
    };
    let [only] = written(opcode)? else { return None };

    // The opcode's own order, which is not the order the arguments were written in: AT&T puts the
    // source first and an instruction may name one operand twice. Which argument goes where is
    // what the table says, and reading it is the whole of the translation.
    let described = form(opcode)?.operands();
    let mut operands = vec![None; described.len()];
    let mut at = None;
    let mut imm = None;
    for (&arg, &given) in only.args.iter().zip(&given) {
        match (arg, given) {
            (Arg::Reg(index, width), Given::Operand(operand)) => {
                *operands.get_mut(usize::from(index))? =
                    Some(Piece::Operand { index: operand, width });
            }
            (Arg::Reg(index, width), Given::Reg(reg, spelled)) => {
                if spelled != width {
                    return None;
                }
                *operands.get_mut(usize::from(index))? = Some(Piece::Reg { reg, width });
            }
            (Arg::Imm, Given::Imm(value)) => imm = Some(value),
            (Arg::Mem, Given::Mem(address)) => at = Some(address),
            _ => return None,
        }
    }
    // An operand the opcode has and its spelling did not name. [`machine`] lets one through when
    // the description says what is in it and refuses it otherwise, so anything still empty here
    // has one possible answer and these two passes write it down. The fixed ones go first because
    // the tie in the second pass copies an answer, and an answer has to be there to copy.
    for (slot, desc) in operands.iter_mut().zip(described) {
        if slot.is_some() {
            continue;
        }
        if let Constraint::Fixed(reg) = desc.constraint {
            *slot = Some(Piece::Implicit { reg });
        }
    }
    for index in 0..operands.len() {
        if operands[index].is_some() {
            continue;
        }
        let partner = tied(described, index)?;
        operands[index] = *operands.get(partner)?;
    }
    let operands: Vec<Piece> = operands.into_iter().collect::<Option<_>>()?;
    Some(Line { opcode, operands, at, imm })
}

/// The operand that shares a register with the one at that index, when the description says one
/// does.
///
/// Asked in both directions, because a tie is between two operands and either of them may be the
/// one carrying it. On this machine it is always the destination of a two-address instruction that
/// carries it and always the first source it points at, but which end the table writes it on is
/// the table's business rather than something to depend on here.
fn tied(described: &[OperandDesc], index: usize) -> Option<usize> {
    if let Constraint::Reuse(other) = described.get(index)?.constraint {
        return Some(usize::from(other));
    }
    described.iter().position(
        |desc| matches!(desc.constraint, Constraint::Reuse(back) if usize::from(back) == index),
    )
}

/// That mnemonic with the width of its arguments written on the end of it.
///
/// What an assembler does with a mnemonic a program left the suffix off, and it is the same answer
/// for the same reason: the arguments say the width, so the suffix would be saying it a second
/// time. Nothing when they do not say it. An instruction whose arguments are all of them numbers
/// and addresses has nothing to take a width from, and one whose register arguments disagree about
/// the width is two instructions at once, and either way the program has to spell it out.
fn suffixed(mnemonic: &str, given: &[Given], widths: &[Option<Width>]) -> Option<String> {
    let mut width: Option<Width> = None;
    for arg in given {
        let each = match *arg {
            Given::Reg(_, each) => each,
            Given::Operand(index) => (*widths.get(index)?)?,
            // The register an address is counted from is a whole one whatever the instruction
            // reads, and a number is as wide as it needs to be, so neither says anything here.
            Given::Imm(_) | Given::Mem(_) => continue,
        };
        if width.replace(each).is_some_and(|before| before != each) {
            return None;
        }
    }
    let suffix = match width? {
        Width::Byte => 'b',
        Width::Word => 'w',
        Width::Long => 'l',
        Width::Quad => 'q',
    };
    Some(format!("{mnemonic}{suffix}"))
}

/// The arguments of one instruction, split on the commas between them.
///
/// The commas inside an addressing mode are not between arguments, which is why this counts
/// brackets rather than splitting on every comma.
fn arguments(text: &str) -> Vec<&str> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (at, letter) in text.char_indices() {
        match letter {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(text[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    args.push(text[start..].trim());
    args
}

/// One argument as the template wrote it, before anything has been matched against an opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Given {
    Operand(usize),
    Reg(PhysReg, Width),
    Imm(i64),
    Mem(At),
}

impl Given {
    /// What kind of thing it is, which is what an opcode is looked up by.
    fn shape(&self) -> Shape {
        match self {
            Given::Operand(_) | Given::Reg(..) => Shape::Reg,
            Given::Imm(_) => Shape::Imm,
            Given::Mem(_) => Shape::Mem,
        }
    }
}

/// One argument, or nothing for one this cannot read.
fn given(text: &str) -> Option<Given> {
    if let Some(written) = text.strip_prefix('$') {
        return number(written.trim()).map(Given::Imm);
    }
    let Some(after) = sigil(text) else { return address(text, None).map(Given::Mem) };
    if after.starts_with(|letter: char| letter.is_ascii_digit()) {
        return after.parse().ok().map(Given::Operand);
    }
    // A segment register is the one name that is followed by an address rather than being an
    // argument on its own, and it is the only way anything here reaches the block a thread owns.
    if let Some((name, rest)) = after.split_once(':') {
        let segment = match name {
            "fs" => Segment::Fs,
            "gs" => Segment::Gs,
            _ => return None,
        };
        return address(rest, Some(segment)).map(Given::Mem);
    }
    let (reg, width) = gpr_named(after)?;
    Some(Given::Reg(reg, width))
}

/// What follows the sigil, for something that has one.
///
/// Two of them where a template has operands, because there `%` is what numbers an operand and a
/// register has to be written twice to get one `%` into the output. A template with no operands at
/// all has no such conflict and writes one, and both are read here, because the two cannot be
/// confused: what follows an operand's sigil is a digit and what follows a register's is a letter.
fn sigil(text: &str) -> Option<&str> {
    let after = text.strip_prefix('%')?;
    Some(after.strip_prefix('%').unwrap_or(after))
}

/// One address, given the segment already taken off the front of it.
fn address(text: &str, segment: Option<Segment>) -> Option<At> {
    let (front, inside) = match text.trim().split_once('(') {
        Some((front, rest)) => (front.trim(), Some(rest.strip_suffix(')')?.trim())),
        None => (text.trim(), None),
    };
    let disp = if front.is_empty() { 0 } else { i32::try_from(number(front)?).ok()? };
    let base = match inside {
        Some(inside) => Some(base(inside)?),
        None => None,
    };
    Some(At { segment, disp, base })
}

/// The register an address is counted from, which is a whole one whatever the instruction reads.
fn base(text: &str) -> Option<Piece> {
    let after = sigil(text)?;
    if after.starts_with(|letter: char| letter.is_ascii_digit()) {
        return after.parse().ok().map(|index| Piece::Operand { index, width: Width::Quad });
    }
    let (reg, width) = gpr_named(after)?;
    (width == Width::Quad).then_some(Piece::Reg { reg, width })
}

/// One number, in the two spellings an assembler writes them in.
fn number(text: &str) -> Option<i64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest.trim()),
        None => (false, text.strip_prefix('+').unwrap_or(text).trim()),
    };
    let value = match digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
        Some(hex) => i64::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<i64>().ok()?,
    };
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hint a spin lock writes, which is the shortest template there is: one instruction, no
    /// arguments, and an opcode whose operand list is empty.
    #[test]
    fn a_template_that_is_one_mnemonic_is_the_instruction_of_that_name() {
        let lines = read("pause", &[]).expect("pause is an instruction");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].opcode, "pause");
        assert!(lines[0].operands.is_empty());
        assert_eq!(lines[0].at, None);
    }

    /// What every program that finds its own thread identifier writes, and the reason this was
    /// built: a segment, a distance into it, and an output the allocator places.
    #[test]
    fn a_read_through_a_segment_is_the_load_the_machine_already_has() {
        let lines = read("movq %%fs:0, %0", &[]).expect("a load through a segment");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].opcode, "mov_rm_64");
        assert_eq!(lines[0].operands, vec![Piece::Operand { index: 0, width: Width::Quad }]);
        assert_eq!(lines[0].at, Some(At { segment: Some(Segment::Fs), disp: 0, base: None }));
    }

    /// The arguments come back in the opcode's order and not the order they were written in, which
    /// is the one thing a reader of AT&T text has to get right and the one thing a table lookup
    /// cannot get wrong.
    #[test]
    fn a_copy_puts_its_source_and_destination_where_the_opcode_holds_them() {
        let lines = read("movq %1, %0", &[]).expect("a copy between two operands");
        assert_eq!(lines[0].opcode, "mov_rr_64");
        assert_eq!(
            lines[0].operands,
            vec![
                Piece::Operand { index: 0, width: Width::Quad },
                Piece::Operand { index: 1, width: Width::Quad },
            ]
        );
    }

    /// A register the template named is a fixed register and not a choice, and the width its name
    /// says has to be the width the instruction uses.
    #[test]
    fn a_register_the_template_named_is_read_at_the_width_its_name_says() {
        let lines = read("movq %%rax, %0", &[]).expect("a copy out of a named register");
        let source = Piece::Reg { reg: PhysReg::new(0), width: Width::Quad };
        assert_eq!(lines[0].operands[1], source);
        assert_eq!(read("movq %%eax, %0", &[]), None, "a narrow name in a wide instruction");
    }

    /// Several instructions, written the way a program writes them, which is one string with the
    /// separators inside it.
    #[test]
    fn a_template_with_several_instructions_is_several_instructions() {
        let lines = read("pause\n\tpause ; pause", &[]).expect("three of them");
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.opcode == "pause"));
    }

    /// `rep nop` is `pause`, which is the one prefix and instruction pair this reads. libuv writes
    /// it with a semicolon between the two, so the prefix arrives on a line of its own, and the
    /// comment beside it in that source says `a.k.a. PAUSE`.
    #[test]
    fn a_repeat_prefix_on_a_nop_is_the_spin_hint() {
        for template in ["rep; nop", "rep nop", "rep\n\tnop", "repz; nop", "repe nop"] {
            let lines =
                read(template, &[]).unwrap_or_else(|| panic!("{template} is the spin hint"));
            assert_eq!(lines.len(), 1, "{template}");
            assert_eq!(lines[0].opcode, "pause", "{template}");
            assert!(lines[0].operands.is_empty(), "{template}");
        }
    }

    /// What a program asking a processor what it can do writes, which is the one instruction here
    /// whose text names none of its operands and whose description names all of them.
    #[test]
    fn an_instruction_whose_operands_are_all_implicit_is_read_from_the_description() {
        let lines = read("cpuid", &[]).expect("cpuid is an instruction");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].opcode, "cpuid");
        let regs: Vec<PhysReg> = lines[0]
            .operands
            .iter()
            .map(|piece| match *piece {
                Piece::Implicit { reg } => reg,
                other => panic!("{other:?} is a piece the text named"),
            })
            .collect();
        let expected = ["rax", "rbx", "rcx", "rdx", "rax", "rcx"];
        let expected: Vec<PhysReg> =
            expected.iter().map(|name| gpr_named(name).expect("a register").0).collect();
        assert_eq!(regs, expected, "four written and then the two read");
    }

    /// The other relaxation, which is an operand tied to one the text did name. AT&T writes a
    /// two-address instruction with two arguments and this machine describes it with three, and the
    /// third is the destination before the instruction ran. A template that wrote `addq %1, %0`
    /// means the operand is both the third and the first, which is what `"+r"` beside it says.
    #[test]
    fn an_operand_tied_to_a_named_one_is_read_as_that_one() {
        let lines = read("addq %1, %0", &[]).expect("an addition onto an operand");
        assert_eq!(lines[0].opcode, "add_rr_64");
        let destination = Piece::Operand { index: 0, width: Width::Quad };
        assert_eq!(
            lines[0].operands,
            vec![destination, destination, Piece::Operand { index: 1, width: Width::Quad }],
            "written, read, and the other source"
        );
    }

    /// The same tie on the instruction this was built for, which is a shift by the one register
    /// this machine will shift by. Its count is fixed to `rcx` and its value is tied, so both
    /// relaxations are at work on one instruction.
    #[test]
    fn a_shift_by_cl_has_one_operand_of_each_kind_filled_in() {
        let lines = read("shlq %%cl, %0", &[]).expect("a shift by cl");
        assert_eq!(lines[0].opcode, "shl_rcl_64");
        let destination = Piece::Operand { index: 0, width: Width::Quad };
        assert_eq!(lines[0].operands[0], destination);
        assert_eq!(lines[0].operands[1], destination, "the value being shifted");
        assert_eq!(
            lines[0].operands[2],
            Piece::Reg { reg: gpr_named("cl").expect("cl").0, width: Width::Byte }
        );
    }

    /// The pair zstd writes to keep a comparison branchless, which is the whole of why this reads a
    /// conditional move at all. Neither mnemonic carries its suffix and both are read at the width
    /// the operands have, the comparison writes the condition state and the move reads it, and the
    /// move's false arm is the operand it is told to overwrite.
    #[test]
    fn a_comparison_and_a_conditional_move_are_the_two_instructions_they_say_they_are() {
        let widths = [Some(Width::Long); 4];
        let lines = read("cmp %1, %2\ncmova %3, %0", &widths).expect("the branchless select");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].opcode, "cmp_rr_32");
        assert_eq!(lines[1].opcode, "cmov_a_32");
        let kept = Piece::Operand { index: 0, width: Width::Long };
        assert_eq!(
            lines[1].operands,
            vec![kept, kept, Piece::Operand { index: 3, width: Width::Long }],
            "the destination is also the arm taken when the condition does not hold"
        );
    }

    /// The suffix is worked out from the operands and from nothing else, so a template whose
    /// arguments do not agree about the width, or do not say one at all, is refused.
    #[test]
    fn a_mnemonic_with_no_suffix_is_refused_when_the_arguments_do_not_say_the_width() {
        let mixed = [Some(Width::Long), Some(Width::Quad)];
        assert_eq!(read("cmp %0, %1", &mixed), None, "two operands of different widths");
        assert_eq!(read("cmp $1, $2", &[]), None, "nothing that has a width at all");
        assert_eq!(
            read("cmp %0, %1", &[Some(Width::Long)]),
            None,
            "an operand the statement has not got"
        );
    }

    /// A prefix in front of anything else, which is refused rather than dropped. Dropping the
    /// `lock` off a read modify write is the one of these that would turn a correct program into
    /// one that is correct nearly all of the time, which is the worst answer available.
    #[test]
    fn a_prefix_this_does_not_read_is_refused_rather_than_dropped() {
        assert_eq!(read("lock; incl %0", &[]), None, "a lock prefix");
        assert_eq!(read("rep; movsb", &[]), None, "a repeat this has no instruction for");
        assert_eq!(
            read("rep; pause", &[]),
            None,
            "a prefix on an instruction that is already the pair"
        );
        assert_eq!(read("rep", &[]), None, "a prefix with nothing behind it");
        assert_eq!(read("rep; rep; nop", &[]), None, "two prefixes");
        assert_eq!(read("rep nop, %0", &[]), None, "a prefix on an instruction with an argument");
    }

    /// Every refusal in the module documentation, held here so that a later change that starts
    /// guessing at one of them has to say so.
    #[test]
    fn what_cannot_be_placed_is_refused_rather_than_guessed_at() {
        assert_eq!(read("hcf", &[]), None, "a mnemonic this machine does not have");
        assert_eq!(read("movq %0", &[]), None, "an instruction with the wrong number of arguments");
        assert_eq!(read("idivq %0", &[]), None, "an opcode the machine writes as more than one");
        assert_eq!(read("again:", &[]), None, "a label");
        assert_eq!(read(".byte 0", &[]), None, "a directive");
        assert_eq!(read("movq (%%rax,%%rbx,8), %0", &[]), None, "a scaled index");
        assert_eq!(read("movq %%cs:0, %0", &[]), None, "a segment nothing here reaches");
        assert_eq!(read("movq %%xmm0, %0", &[]), None, "a register in the other file");
    }

    /// An empty template is no instructions rather than one that could not be read, which is the
    /// case the backend has lowered since before there was a reader.
    #[test]
    fn a_template_with_nothing_in_it_is_no_instructions() {
        assert_eq!(read("", &[]), Some(Vec::new()));
        assert_eq!(read("  \n\t # nothing here \n", &[]), Some(Vec::new()));
    }

    /// The two spellings of a number and the sign in front of one, since a displacement is as
    /// often negative as not.
    #[test]
    fn a_displacement_is_read_in_both_spellings_and_both_signs() {
        let cases = [("-8(%%rbp)", -8), ("0x10(%%rbp)", 16), ("+4(%%rbp)", 4)];
        for (written, disp) in cases {
            let text = format!("movq {written}, %0");
            let lines = read(&text, &[]).unwrap_or_else(|| panic!("{text} is a load"));
            assert_eq!(lines[0].at.expect("an address").disp, disp, "{text}");
        }
    }

    /// The three spellings of a boundary, all of which mean the same thing and two of which say it
    /// in bytes while the first says it as a power. zstd writes the first one in front of the match
    /// loop of its lazy matcher, and the other two are what a program written for another assembler
    /// says.
    #[test]
    fn an_alignment_is_read_in_all_three_spellings_and_carries_its_boundary_in_bytes() {
        let cases = [(".p2align 5", 32), (".align 16", 16), (".balign 8", 8), (".p2align 0", 1)];
        for (template, bytes) in cases {
            let lines = read(template, &[]).unwrap_or_else(|| panic!("{template} is an alignment"));
            assert_eq!(lines.len(), 1, "{template}");
            assert_eq!(lines[0].opcode, ALIGN, "{template}");
            assert!(lines[0].operands.is_empty(), "{template}");
            assert_eq!(lines[0].at, None, "{template}");
            assert_eq!(lines[0].imm, Some(bytes), "{template}");
        }
        let lines = read(".p2align 4\n\tpause", &[]).expect("an alignment in front of one");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].opcode, ALIGN);
        assert_eq!(lines[1].opcode, "pause");
    }

    /// What an alignment this cannot promise looks like. A second argument is a fill byte or a
    /// number of bytes to stop after, and either one means something this does not do, so it is
    /// refused rather than dropped on the floor. A boundary that is not a power of two or is larger
    /// than a page is one nothing here can give.
    #[test]
    fn an_alignment_that_asks_for_more_than_a_boundary_is_refused() {
        assert_eq!(read(".p2align 4, 0x90", &[]), None, "a fill byte");
        assert_eq!(read(".p2align 4, 0x90, 8", &[]), None, "a most to skip");
        assert_eq!(read(".align 24", &[]), None, "a boundary that is not a power of two");
        assert_eq!(read(".balign 8192", &[]), None, "a boundary larger than a page");
        assert_eq!(read(".p2align 20", &[]), None, "a power larger than a page");
        assert_eq!(read(".p2align", &[]), None, "a boundary that was left out");
        assert_eq!(read(".p2align four", &[]), None, "a boundary that is not a number");
        assert_eq!(read(".skip 16", &[]), None, "a directive that is not an alignment");
        assert_eq!(read("rep; .p2align 4", &[]), None, "a prefix in front of one");
    }
}
