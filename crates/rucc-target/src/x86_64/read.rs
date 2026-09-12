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
//! that names the statement. Labels and directives are not read, because a label inside a function
//! is a place something can jump to and the block layout has already decided where the places are.
//! The scaled index of an addressing mode is not read. Neither is an instruction whose opcode has
//! an operand its spelling does not name, which [`machine`] explains. The rule throughout is that
//! an instruction this compiler cannot place is refused out loud, since the alternative is a
//! program that assembles into something other than what it says.

use crate::regs::{PhysReg, Segment};
use crate::x86_64::insts::form;
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
#[must_use]
pub fn read(template: &str) -> Option<Vec<Line>> {
    let mut lines = Vec::new();
    for text in template.split(['\n', ';']) {
        let text = uncommented(text).trim();
        if text.is_empty() {
            continue;
        }
        lines.push(instruction(text)?);
    }
    Some(lines)
}

/// One line with anything a comment starts taken off the end of it.
fn uncommented(text: &str) -> &str {
    let end = text.find('#').into_iter().chain(text.find("//")).min();
    end.map_or(text, |at| &text[..at])
}

/// One instruction, or nothing for one this cannot place.
fn instruction(text: &str) -> Option<Line> {
    let (mnemonic, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    // A label ends in a colon and a directive starts with a dot, and neither is an instruction.
    // Both are caught here rather than being looked for, because a mnemonic is letters and digits
    // and nothing else, so anything carrying punctuation is already not one.
    if mnemonic.is_empty() || !mnemonic.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }

    let given: Vec<Given> =
        arguments(rest).iter().map(|text| given(text)).collect::<Option<_>>()?;
    let shapes: Vec<Shape> = given.iter().map(Given::shape).collect();
    let opcode = machine(mnemonic, &shapes)?;
    let [only] = written(opcode)? else { return None };

    // The opcode's own order, which is not the order the arguments were written in: AT&T puts the
    // source first and an instruction may name one operand twice. Which argument goes where is
    // what the table says, and reading it is the whole of the translation.
    let mut operands = vec![None; form(opcode)?.operands().len()];
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
    let operands: Vec<Piece> = operands.into_iter().collect::<Option<_>>()?;
    Some(Line { opcode, operands, at, imm })
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
        let lines = read("pause").expect("pause is an instruction");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].opcode, "pause");
        assert!(lines[0].operands.is_empty());
        assert_eq!(lines[0].at, None);
    }

    /// What every program that finds its own thread identifier writes, and the reason this was
    /// built: a segment, a distance into it, and an output the allocator places.
    #[test]
    fn a_read_through_a_segment_is_the_load_the_machine_already_has() {
        let lines = read("movq %%fs:0, %0").expect("a load through a segment");
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
        let lines = read("movq %1, %0").expect("a copy between two operands");
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
        let lines = read("movq %%rax, %0").expect("a copy out of a named register");
        let source = Piece::Reg { reg: PhysReg::new(0), width: Width::Quad };
        assert_eq!(lines[0].operands[1], source);
        assert_eq!(read("movq %%eax, %0"), None, "a narrow name in a wide instruction");
    }

    /// Several instructions, written the way a program writes them, which is one string with the
    /// separators inside it.
    #[test]
    fn a_template_with_several_instructions_is_several_instructions() {
        let lines = read("pause\n\tpause ; pause").expect("three of them");
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.opcode == "pause"));
    }

    /// Every refusal in the module documentation, held here so that a later change that starts
    /// guessing at one of them has to say so.
    #[test]
    fn what_cannot_be_placed_is_refused_rather_than_guessed_at() {
        assert_eq!(read("hcf"), None, "a mnemonic this machine does not have");
        assert_eq!(read("movq %0"), None, "an instruction with the wrong number of arguments");
        assert_eq!(
            read("addq %1, %0"),
            None,
            "an opcode with an operand its spelling does not name"
        );
        assert_eq!(read("idivq %0"), None, "an opcode the machine writes as more than one");
        assert_eq!(read("again:"), None, "a label");
        assert_eq!(read(".byte 0"), None, "a directive");
        assert_eq!(read("movq (%%rax,%%rbx,8), %0"), None, "a scaled index");
        assert_eq!(read("movq %%cs:0, %0"), None, "a segment nothing here reaches");
        assert_eq!(read("movq %%xmm0, %0"), None, "a register in the other file");
    }

    /// An empty template is no instructions rather than one that could not be read, which is the
    /// case the backend has lowered since before there was a reader.
    #[test]
    fn a_template_with_nothing_in_it_is_no_instructions() {
        assert_eq!(read(""), Some(Vec::new()));
        assert_eq!(read("  \n\t # nothing here \n"), Some(Vec::new()));
    }

    /// The two spellings of a number and the sign in front of one, since a displacement is as
    /// often negative as not.
    #[test]
    fn a_displacement_is_read_in_both_spellings_and_both_signs() {
        let cases = [("-8(%%rbp)", -8), ("0x10(%%rbp)", 16), ("+4(%%rbp)", 4)];
        for (written, disp) in cases {
            let text = format!("movq {written}, %0");
            let lines = read(&text).unwrap_or_else(|| panic!("{text} is a load"));
            assert_eq!(lines[0].at.expect("an address").disp, disp, "{text}");
        }
    }
}
