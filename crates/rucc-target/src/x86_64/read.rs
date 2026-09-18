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
//! register the template named, an operand of the statement written `%0` or `%q0`, a number
//! written `$5`, or an address written `8(%rbp)` with an optional `%fs:` or `%gs:` in front of it.
//! The distance into an address is a number the template wrote or, written `%c3(%rbp)`, the number
//! in one of the statement's operands, which is how a program spells a step whose size something
//! else worked out. See [`Disp`].
//!
//! A line may also be a label, which is not an instruction at all but a place, and a jump to one of
//! those. Both are read, and what they are is in the section below. A jump to a label the template
//! does not define is not, since that is a jump out of the statement and is what `asm goto` says
//! with its own list rather than in its text.
//!
//! Everything else is nothing at all rather than a guess, and the caller turns that into a refusal
//! that names the statement. Directives
//! are not read, with one exception, which is [`alignment`] and has a section of its own
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
//! # The width an operand carries on itself
//!
//! `%q0` rather than `%0`, which is the other half of the same question. The letter between the
//! sigil and the number says how much of that operand's register the instruction uses, and it is
//! the program overriding the type rather than reading it: `%b0`, `%w0`, `%k0` and `%q0` are the
//! byte, the word, the long and the quad of the same register.
//!
//! A program writes one when the header it is in is read on more than one target. libgmp writes
//! `%q0` all through `longlong.h`, which every file of that library includes, and it writes it
//! because the same header is read on the thirty two bit target where a limb is a `long` and the
//! same line still has to say sixty four bits. On this target the type already says sixty four, so
//! the modifier is the program saying it twice, and what it costs to leave unread is the whole
//! library.
//!
//! Once an argument says a width the mnemonic no longer has to, so these are read before the suffix
//! is worked out and they are what it is worked out from. Whether one of them is allowed to differ
//! from the type is not settled here. What is settled here is that the program said it, which is
//! what the flag on [`Piece::Operand`] carries out, and the caller reads that flag and the operand's
//! role together. A written operand wider than its type is the program asking for an instruction
//! that fills more of the register than the object in it does, which is what gmp writes when it
//! counts the low zero bits of a limb into an `unsigned`. A read one wider than its type is a
//! program handing an instruction the top half of a register that nothing ever defined, and that is
//! refused where it is placed.
//!
//! Four of them and not the rest. gcc has a dozen more letters in that position and they do other
//! things: print a constant without its sigil, print the suffix on its own, print an address. Those
//! are a template asking for text rather than for an instruction, and text is not what this reads.
//! `%h`, which is the high byte of one of the four registers that have one, is left out for a
//! different reason, which is that it names a register rather than a part of one and nothing in
//! this backend has a name for it.
//!
//! # The labels and the jumps between them
//!
//! A template that writes a label and jumps back to it is a loop the program wrote by hand, and
//! libgmp writes one wherever it carries a one along an integer: `MPN_INCR_U` adds into memory,
//! steps the pointer and goes round again while the addition carries. There is no way to say that
//! with a straight run of instructions, so the statement stops being one and becomes several.
//!
//! What comes back for a label is [`Step::Label`] with the name on it, and for a jump to one it is
//! [`Step::Jump`] with the opcode and the name it goes to. The name is a string and is compared
//! with other strings in the same template and with nothing else. It never reaches the object file,
//! because the caller turns each label into a block and each jump into an arm, so `%=`, which is
//! gcc's way of writing a number that differs for every copy of a statement, needs no expanding
//! here: it is part of a name whose whole job is to say which jump goes with which label.
//!
//! Ten conditions, each in every spelling an assembler takes for it, and no unconditional jump. A
//! condition ends a block with two arms and that is a shape the machine IR already has. A `jmp`
//! ends one with a single arm and leaves whatever the template wrote behind it reachable by
//! nothing, and a block nothing reaches is a question about the rest of the pipeline rather than
//! about this, so it is refused until a program asks for it. Neither is a jump on the sign, the
//! overflow or the parity, for the plainer reason that this backend has no opcode for those.
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

use std::borrow::Cow;

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
        /// Whether the template wrote that width down on the operand rather than leaving it.
        ///
        /// `%0` is spelled as the register at the width of the object in it, so an opcode that
        /// uses a different amount of the register than the object fills is an instruction whose
        /// text would not assemble, and the operand's type is the one answer there. `%q0` is the
        /// whole register whatever the object is, which is how a program asks for an instruction
        /// that fills more of the register than its own type covers: gmp counts the low zero bits
        /// of a limb into an `unsigned` and writes `%q0` so that the count arrives from a quadword
        /// instruction, and the object is the low half of what that instruction wrote.
        ///
        /// So this says which of the two the width came from, and it is not on its own permission
        /// to differ from the type: the same modifier on a read is the program handing an
        /// instruction a part of a register it never filled. The place an operand is put reads
        /// this and the operand's role together, and only a written one may differ.
        stated: bool,
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

/// How far into its storage an address counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disp {
    /// A number the template wrote, which is `8(%rax)` and is almost all of them.
    Number(i32),
    /// The number in one of the statement's operands, which is `%c3(%rax)`.
    ///
    /// The `c` is gcc's way of saying that the operand is a constant and is to be written the way a
    /// distance is written rather than the way an immediate is, which is to say without the sigil
    /// in front of it. A program writes one where the distance is a size something worked out
    /// rather than a number anybody typed. gmp walks a limb at a time and spells the step `%c3(%0)`
    /// with `sizeof(mp_limb_t)` tied to that operand, so the one line is right on the target where
    /// a limb is eight bytes and on the one where it is four.
    ///
    /// Which operand, by the number the constraint list gives it. What is in it is not this file's
    /// business, the way the register an operand is in is not: the caller has the values.
    Operand(usize),
}

/// An address an instruction in a template reads or writes.
///
/// No scaled index. See the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct At {
    /// Which storage it is counted from, when it is not the flat one.
    pub segment: Option<Segment>,
    /// How far into it.
    pub disp: Disp,
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

/// One piece of a template, which is an instruction or one of the two things that are not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A place in the template, named, which the jumps in the same template go to.
    Label(String),
    /// A jump to one of those places, taken when the condition state says what the opcode asks.
    Jump {
        /// The opcode, spelled the way the machine IR spells it, so `jcc_b` and not `x64.jcc_b`.
        opcode: &'static str,
        /// Which label it goes to, by the name the template wrote on both.
        to: String,
    },
    /// One instruction.
    Line(Line),
}

/// Every step of that template, or nothing when any of them is not one this can place.
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
pub fn read(template: &str, widths: &[Option<Width>]) -> Option<Vec<Step>> {
    let mut steps = Vec::new();
    let mut carried = false;
    for text in template.split(['\n', ';']) {
        let text = uncommented(text).trim();
        if text.is_empty() {
            continue;
        }
        // A label and a jump to one, both before the mnemonics, because a label carries punctuation
        // a mnemonic never does and a jump's argument is a name rather than anything an operand is
        // written as. A repeat prefix in front of either is half an instruction, which is what the
        // refusal on the flag says wherever it appears.
        if let Some(name) = text.strip_suffix(':') {
            if carried || !is_label(name) {
                return None;
            }
            steps.push(Step::Label(name.to_owned()));
            continue;
        }
        if let Some(step) = jumped(text) {
            if carried {
                return None;
            }
            steps.push(step);
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
            steps.push(Step::Line(line));
            continue;
        }
        steps.push(Step::Line(instruction(text, carried, widths)?));
        carried = false;
    }
    // A prefix with nothing behind it is half an instruction, and half a template is refused for
    // the reason the whole of one is.
    if carried {
        return None;
    }
    settled(&steps).then_some(steps)
}

/// Whether the labels and the jumps in that template go together.
///
/// Two ways they may not. A name written on two labels is a template with two places of the same
/// name in it, which an assembler refuses and which nothing below could tell apart. A jump to a name
/// no label in the template carries is a jump out of the statement, which is a real thing a program
/// asks for and is asked for with the target list `asm goto` has rather than with the text, so what
/// is written here is the one that is not that.
fn settled(steps: &[Step]) -> bool {
    let names: Vec<&str> = steps
        .iter()
        .filter_map(|step| match step {
            Step::Label(name) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    if names.iter().enumerate().any(|(at, name)| names[..at].contains(name)) {
        return false;
    }
    steps.iter().all(|step| match step {
        Step::Jump { to, .. } => names.contains(&to.as_str()),
        _ => true,
    })
}

/// The jump that line is, or nothing for a line that is not one.
///
/// The argument has to be a name, which is what keeps `jmp *%rax` and anything else that goes to an
/// address out of this: a jump this reads goes to a place in the same template and nowhere else.
fn jumped(text: &str) -> Option<Step> {
    let (mnemonic, rest) = text.split_once(char::is_whitespace)?;
    let opcode = condition(mnemonic)?;
    let to = rest.trim();
    is_label(to).then(|| Step::Jump { opcode, to: to.to_owned() })
}

/// The opcode a conditional jump's mnemonic names, in every spelling an assembler takes for it.
///
/// Ten conditions and twenty six spellings, because a machine that answers a comparison with a
/// handful of bits lets a program name the same bits from either side: `jb` and `jnae` are the one
/// instruction and a program writes whichever reads better where it stands. `jc` and `jnc` are the
/// two libgmp writes, and they are the same pair again named after the bit rather than after the
/// comparison, which is what a template carrying a one along an integer is really asking about.
fn condition(mnemonic: &str) -> Option<&'static str> {
    Some(match mnemonic {
        "je" | "jz" => "jcc_e",
        "jne" | "jnz" => "jcc_ne",
        "jl" | "jnge" => "jcc_l",
        "jle" | "jng" => "jcc_le",
        "jg" | "jnle" => "jcc_g",
        "jge" | "jnl" => "jcc_ge",
        "jb" | "jc" | "jnae" => "jcc_b",
        "jbe" | "jna" => "jcc_be",
        "ja" | "jnbe" => "jcc_a",
        "jae" | "jnc" | "jnb" => "jcc_ae",
        _ => return None,
    })
}

/// Whether that is a name a label in a template may carry.
///
/// Letters and digits, the three other characters an assembler takes in a name, and `%` and `=`,
/// which are there for `%=` and for nothing else. A name beginning with a digit is left out because
/// those are an assembler's local labels, where `1b` and `1f` mean the nearest one backwards and the
/// nearest one forwards, and that is a different thing from a name with two ends of its own.
fn is_label(text: &str) -> bool {
    let named = |letter: char| letter.is_ascii_alphanumeric() || "_.$%=".contains(letter);
    !text.is_empty()
        && !text.starts_with(|letter: char| letter.is_ascii_digit())
        && text.chars().all(named)
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

/// The instruction a repeat prefix and that mnemonic are together, or nothing for a pair this does
/// not read.
///
/// Three of them, and all three are one instruction a program spelled as a prefix and another
/// because its assembler was older than the mnemonic for the one it meant. That is not a trick, it
/// is what the prefix byte does: a processor without the feature ignores it and runs the
/// instruction behind it, so the old spelling was the way to ask for the new instruction and get
/// something reasonable on a machine that did not have it.
///
/// `rep nop` is `pause`, the hint a spin lock wants, and libuv writes it with `a.k.a. PAUSE` in the
/// comment beside it. `rep bsf` is `tzcnt` and `rep bsr` is `lzcnt`, which libgmp writes in
/// `longlong.h` with a comment saying exactly that, and which are not the searches they are spelled
/// as: a search answers where the bit is and a count answers how many places came before it. So
/// what comes back is the count, and a prefix left unread here would be a program counting bits and
/// getting indices.
///
/// The suffix comes through, since it is the width and the prefix says nothing about the width. The
/// sixteen bit one is refused, because there is no encoding for it in this assembler and the reason
/// is in `crate::x86_64::encode`.
fn repeated(mnemonic: &str, rest: &str) -> Option<String> {
    if mnemonic == "nop" && rest.trim().is_empty() {
        return Some("pause".to_owned());
    }
    for (search, count) in [("bsf", "tzcnt"), ("bsr", "lzcnt")] {
        let Some(suffix) = mnemonic.strip_prefix(search) else { continue };
        if suffix.is_empty() || suffix == "l" || suffix == "q" {
            return Some(format!("{count}{suffix}"));
        }
    }
    None
}

/// One line with anything a comment starts taken off the end of it.
fn uncommented(text: &str) -> &str {
    let end = text.find('#').into_iter().chain(text.find("//")).min();
    end.map_or(text, |at| &text[..at])
}

/// One instruction, or nothing for one this cannot place.
///
/// The flag says a repeat prefix was written in front of it, either on a line of its own or as
/// the first word of this one, and what it means is [`repeated`]. A prefix that combination does
/// not name is refused, `lock` included, because a prefix that changes what an instruction does is
/// not something to guess at: dropping the `lock` off a read modify write would turn a program that
/// is correct into one that is nearly always correct.
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
    let mnemonic: Cow<'_, str> =
        if prefixed { Cow::Owned(repeated(mnemonic, rest)?) } else { Cow::Borrowed(mnemonic) };
    let mnemonic = mnemonic.as_ref();
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
            (Arg::Reg(index, width), Given::Operand(operand, stated)) => {
                // A width the template wrote on the operand has to be the width the opcode uses,
                // the way a register the template named has to be. `movq %1, %k0` is a quadword
                // move into half a register, which is not an instruction and is not half of one.
                if stated.is_some_and(|stated| stated != width) {
                    return None;
                }
                *operands.get_mut(usize::from(index))? =
                    Some(Piece::Operand { index: operand, width, stated: stated.is_some() });
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
///
/// An operand that carried a modifier says its own width and the caller's list is not consulted for
/// it. That is the modifier's whole purpose, and it means a template can name an operand whose type
/// has no width here at all as long as it says which part of the register it wants.
fn suffixed(mnemonic: &str, given: &[Given], widths: &[Option<Width>]) -> Option<String> {
    let mut width: Option<Width> = None;
    for arg in given {
        let each = match *arg {
            Given::Reg(_, each) => each,
            // The template's own answer first, since a program that wrote one wrote it to say
            // something other than what the type says.
            Given::Operand(index, stated) => match stated {
                Some(stated) => stated,
                None => (*widths.get(index)?)?,
            },
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
    /// `%0`, with the width the template wrote on it for one that carried a modifier.
    Operand(usize, Option<Width>),
    Reg(PhysReg, Width),
    Imm(i64),
    Mem(At),
}

impl Given {
    /// What kind of thing it is, which is what an opcode is looked up by.
    fn shape(&self) -> Shape {
        match self {
            Given::Operand(..) | Given::Reg(..) => Shape::Reg,
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
        return after.parse().ok().map(|index| Given::Operand(index, None));
    }
    // An address whose distance is in an operand, which is `%c3(%0)` and reaches here rather than
    // the line above because it begins with a sigil and what follows the sigil is a letter, the way
    // a register's name does. The bracket is what tells the two apart and is what is asked for
    // here, since a `%c3` with nothing after it is a constant a template wants printed rather than
    // an instruction, and printing is not what this reads.
    if after.starts_with('c') && text.contains('(') {
        return address(text, None).map(Given::Mem);
    }
    // Before the register names, because `%b0` and `%bl` both begin with the same letter and only
    // one of them is a register. What tells them apart is what comes after the letter, so the
    // modifier is tried first and falls through to the names when the rest of it is not a number.
    if let Some(given) = modified(after) {
        return Some(given);
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

/// An operand with a width written on it, or nothing for anything that is not one.
///
/// Given what follows the sigil, so `q0` rather than `%q0`. See the module documentation for which
/// four letters are read and why the rest are not.
fn modified(after: &str) -> Option<Given> {
    let (letter, digits) = after.split_at_checked(1)?;
    let width = match letter {
        "b" => Width::Byte,
        "w" => Width::Word,
        "k" => Width::Long,
        "q" => Width::Quad,
        _ => return None,
    };
    Some(Given::Operand(digits.parse().ok()?, Some(width)))
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
    let disp = if front.is_empty() { Disp::Number(0) } else { displacement(front)? };
    let base = match inside {
        Some(inside) => Some(base(inside)?),
        None => None,
    };
    Some(At { segment, disp, base })
}

/// How far in, which is a number or the number in an operand. See [`Disp`].
fn displacement(text: &str) -> Option<Disp> {
    if let Some(after) = text.strip_prefix("%c") {
        return after.parse().ok().map(Disp::Operand);
    }
    i32::try_from(number(text)?).ok().map(Disp::Number)
}

/// The register an address is counted from, which is a whole one whatever the instruction reads.
fn base(text: &str) -> Option<Piece> {
    let after = sigil(text)?;
    if after.starts_with(|letter: char| letter.is_ascii_digit()) {
        // Stated, in the sense the field means: the width is the addressing mode's and not the
        // operand's, so a pointer's own type is not the thing that has to agree with it.
        return after.parse().ok().map(|index| Piece::Operand {
            index,
            width: Width::Quad,
            stated: true,
        });
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

    /// Every instruction in a template that has nothing in it but instructions.
    ///
    /// Which is almost every template written below, and is what [`read`] itself gave back before a
    /// template could hold a label too. A template with one in it comes back as nothing here, so a
    /// test about labels calls [`read`] and reads the steps.
    fn plain(template: &str, widths: &[Option<Width>]) -> Option<Vec<Line>> {
        read(template, widths)?
            .into_iter()
            .map(|step| match step {
                Step::Line(line) => Some(line),
                Step::Label(_) | Step::Jump { .. } => None,
            })
            .collect()
    }

    /// The hint a spin lock writes, which is the shortest template there is: one instruction, no
    /// arguments, and an opcode whose operand list is empty.
    #[test]
    fn a_template_that_is_one_mnemonic_is_the_instruction_of_that_name() {
        let lines = plain("pause", &[]).expect("pause is an instruction");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].opcode, "pause");
        assert!(lines[0].operands.is_empty());
        assert_eq!(lines[0].at, None);
    }

    /// What every program that finds its own thread identifier writes, and the reason this was
    /// built: a segment, a distance into it, and an output the allocator places.
    #[test]
    fn a_read_through_a_segment_is_the_load_the_machine_already_has() {
        let lines = plain("movq %%fs:0, %0", &[]).expect("a load through a segment");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].opcode, "mov_rm_64");
        let out = Piece::Operand { index: 0, width: Width::Quad, stated: false };
        assert_eq!(lines[0].operands, vec![out]);
        let at = At { segment: Some(Segment::Fs), disp: Disp::Number(0), base: None };
        assert_eq!(lines[0].at, Some(at));
    }

    /// The arguments come back in the opcode's order and not the order they were written in, which
    /// is the one thing a reader of AT&T text has to get right and the one thing a table lookup
    /// cannot get wrong.
    #[test]
    fn a_copy_puts_its_source_and_destination_where_the_opcode_holds_them() {
        let lines = plain("movq %1, %0", &[]).expect("a copy between two operands");
        assert_eq!(lines[0].opcode, "mov_rr_64");
        assert_eq!(
            lines[0].operands,
            vec![
                Piece::Operand { index: 0, width: Width::Quad, stated: false },
                Piece::Operand { index: 1, width: Width::Quad, stated: false },
            ]
        );
    }

    /// A register the template named is a fixed register and not a choice, and the width its name
    /// says has to be the width the instruction uses.
    #[test]
    fn a_register_the_template_named_is_read_at_the_width_its_name_says() {
        let lines = plain("movq %%rax, %0", &[]).expect("a copy out of a named register");
        let source = Piece::Reg { reg: PhysReg::new(0), width: Width::Quad };
        assert_eq!(lines[0].operands[1], source);
        assert_eq!(plain("movq %%eax, %0", &[]), None, "a narrow name in a wide instruction");
    }

    /// Several instructions, written the way a program writes them, which is one string with the
    /// separators inside it.
    #[test]
    fn a_template_with_several_instructions_is_several_instructions() {
        let lines = plain("pause\n\tpause ; pause", &[]).expect("three of them");
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
                plain(template, &[]).unwrap_or_else(|| panic!("{template} is the spin hint"));
            assert_eq!(lines.len(), 1, "{template}");
            assert_eq!(lines[0].opcode, "pause", "{template}");
            assert!(lines[0].operands.is_empty(), "{template}");
        }
    }

    /// What a program asking a processor what it can do writes, which is the one instruction here
    /// whose text names none of its operands and whose description names all of them.
    #[test]
    fn an_instruction_whose_operands_are_all_implicit_is_read_from_the_description() {
        let lines = plain("cpuid", &[]).expect("cpuid is an instruction");
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
        let lines = plain("addq %1, %0", &[]).expect("an addition onto an operand");
        assert_eq!(lines[0].opcode, "add_rr_64");
        let destination = Piece::Operand { index: 0, width: Width::Quad, stated: false };
        let source = Piece::Operand { index: 1, width: Width::Quad, stated: false };
        assert_eq!(
            lines[0].operands,
            vec![destination, destination, source],
            "written, read, and the other source"
        );
    }

    /// The same tie on the instruction this was built for, which is a shift by the one register
    /// this machine will shift by. Its count is fixed to `rcx` and its value is tied, so both
    /// relaxations are at work on one instruction.
    #[test]
    fn a_shift_by_cl_has_one_operand_of_each_kind_filled_in() {
        let lines = plain("shlq %%cl, %0", &[]).expect("a shift by cl");
        assert_eq!(lines[0].opcode, "shl_rcl_64");
        let destination = Piece::Operand { index: 0, width: Width::Quad, stated: false };
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
        let lines = plain("cmp %1, %2\ncmova %3, %0", &widths).expect("the branchless select");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].opcode, "cmp_rr_32");
        assert_eq!(lines[1].opcode, "cmov_a_32");
        let kept = Piece::Operand { index: 0, width: Width::Long, stated: false };
        let arm = Piece::Operand { index: 3, width: Width::Long, stated: false };
        assert_eq!(
            lines[1].operands,
            vec![kept, kept, arm],
            "the destination is also the arm taken when the condition does not hold"
        );
    }

    /// The four widths a template can write on an operand, each of them read as that much of the
    /// register. libgmp writes the last of them throughout `longlong.h`, which is the header every
    /// file of that library includes.
    #[test]
    fn an_operand_carrying_a_width_is_read_at_the_width_it_carries() {
        let cases = [
            ("addb %1, %b0", "add_rr_8", Width::Byte),
            ("addw %1, %w0", "add_rr_16", Width::Word),
            ("addl %1, %k0", "add_rr_32", Width::Long),
            ("addq %1, %q0", "add_rr_64", Width::Quad),
        ];
        for (template, opcode, width) in cases {
            let widths = [Some(width); 2];
            let lines =
                plain(template, &widths).unwrap_or_else(|| panic!("{template} is an addition"));
            assert_eq!(lines[0].opcode, opcode, "{template}");
            let written = Piece::Operand { index: 0, width, stated: true };
            assert_eq!(lines[0].operands[0], written, "{template}");
        }
    }

    /// The modifier is where the width comes from when there is one, so a template that wrote one
    /// has said what an assembler needs and the mnemonic may carry no suffix at all. The types the
    /// caller passed in are not consulted for an operand that carries its own width, which is what
    /// lets a template name an operand whose type is no width this machine has. Whether the two
    /// agree is asked where the operand is placed rather than here, since that is where the type
    /// is.
    #[test]
    fn a_width_written_on_an_operand_is_what_the_suffix_is_worked_out_from() {
        let lines = plain("add %q1, %q0", &[Some(Width::Long); 2]).expect("an addition");
        assert_eq!(lines[0].opcode, "add_rr_64", "the modifier and not the type");
        assert_eq!(
            plain("add %1, %0", &[Some(Width::Long); 2]).expect("an addition")[0].opcode,
            "add_rr_32",
            "the type, for the same template without one"
        );
        let lines = plain("addq %1, %q0", &[None, None]).expect("an addition");
        assert_eq!(lines[0].opcode, "add_rr_64", "an operand whose type has no width here");
        assert_eq!(
            plain("add %1, %q0", &[Some(Width::Long); 2]),
            None,
            "one operand saying a width and the other saying a different one"
        );
    }

    /// The count of the low zero bits of a limb as gmp writes it, which is the template this flag
    /// was added for. The count goes into an `unsigned` and the instruction is a quadword one, so
    /// the two widths differ and the one the operand comes back at is the template's. Whether that
    /// is allowed is the caller's question and the answer depends on the operand's role, which is
    /// not something this file has, so what this carries is which of the two the width came from.
    #[test]
    fn an_operand_whose_width_came_from_the_template_says_where_it_came_from() {
        let widths = [Some(Width::Long), Some(Width::Quad)];
        let lines = plain("rep;bsf\t%1, %q0", &widths).expect("the count gmp writes");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].opcode, "tzcnt_64", "the modifier and not the type");
        let written = Piece::Operand { index: 0, width: Width::Quad, stated: true };
        assert_eq!(lines[0].operands[0], written, "the count, written by the whole instruction");
        let read_from = Piece::Operand { index: 1, width: Width::Quad, stated: false };
        assert_eq!(lines[0].operands[1], read_from, "the limb, whose own type said sixty four");
    }

    /// A width that disagrees with the instruction is refused the way a register name that
    /// disagrees is, and a register name is still read as a register even though three of the four
    /// letters begin one.
    #[test]
    fn a_width_that_disagrees_with_the_instruction_is_refused() {
        assert_eq!(plain("addq %1, %k0", &[Some(Width::Quad); 2]), None, "half a destination");
        assert_eq!(plain("addl %1, %q0", &[Some(Width::Long); 2]), None, "the other way round");
        assert_eq!(
            plain("addq %1, %h0", &[Some(Width::Quad); 2]),
            None,
            "a letter this leaves out"
        );
        assert_eq!(
            plain("addq %1, %q", &[Some(Width::Quad); 2]),
            None,
            "a modifier with no operand"
        );
        let lines = plain("addb %%bl, %0", &[Some(Width::Byte)]).expect("an addition out of bl");
        assert_eq!(
            lines[0].operands[2],
            Piece::Reg { reg: gpr_named("bl").expect("bl").0, width: Width::Byte },
            "a register whose name starts with a letter a modifier also uses"
        );
    }

    /// The suffix is worked out from the operands and from nothing else, so a template whose
    /// arguments do not agree about the width, or do not say one at all, is refused.
    #[test]
    fn a_mnemonic_with_no_suffix_is_refused_when_the_arguments_do_not_say_the_width() {
        let mixed = [Some(Width::Long), Some(Width::Quad)];
        assert_eq!(plain("cmp %0, %1", &mixed), None, "two operands of different widths");
        assert_eq!(plain("cmp $1, $2", &[]), None, "nothing that has a width at all");
        assert_eq!(
            plain("cmp %0, %1", &[Some(Width::Long)]),
            None,
            "an operand the statement has not got"
        );
    }

    /// The two searches and the two counts, which are four instructions and not two spellings of
    /// two. libgmp writes all four in `longlong.h`, picking between them on what the processor the
    /// build was configured for can do, and its comment on the prefixed pair says what they are:
    /// "This is lzcnt, spelled for older assemblers."
    #[test]
    fn a_repeat_prefix_on_a_bit_search_is_the_count_it_is_the_old_spelling_of() {
        let cases = [
            ("bsf\t%1, %q0", "bsf_64"),
            ("bsr\t%1,%0", "bsr_64"),
            ("rep;bsf\t%1, %q0", "tzcnt_64"),
            ("rep;bsr\t%1, %q0", "lzcnt_64"),
            ("rep bsfl %1, %0", "tzcnt_32"),
            ("rep\n\tbsr %k1, %k0", "lzcnt_32"),
        ];
        for (template, opcode) in cases {
            let widths = [Some(Width::Quad); 2];
            let lines =
                plain(template, &widths).unwrap_or_else(|| panic!("{template} is a search"));
            assert_eq!(lines.len(), 1, "{template}");
            assert_eq!(lines[0].opcode, opcode, "{template}");
        }
        assert_eq!(
            plain("rep; bsfw %1, %0", &[Some(Width::Word); 2]),
            None,
            "the sixteen bit count, which this assembler has no encoding for"
        );
    }

    /// A prefix in front of anything else, which is refused rather than dropped. Dropping the
    /// `lock` off a read modify write is the one of these that would turn a correct program into
    /// one that is correct nearly all of the time, which is the worst answer available.
    #[test]
    fn a_prefix_this_does_not_read_is_refused_rather_than_dropped() {
        assert_eq!(plain("lock; incl %0", &[]), None, "a lock prefix");
        assert_eq!(plain("rep; movsb", &[]), None, "a repeat this has no instruction for");
        assert_eq!(
            plain("rep; pause", &[]),
            None,
            "a prefix on an instruction that is already the pair"
        );
        assert_eq!(plain("rep", &[]), None, "a prefix with nothing behind it");
        assert_eq!(plain("rep; rep; nop", &[]), None, "two prefixes");
        assert_eq!(plain("rep nop, %0", &[]), None, "a prefix on an instruction with an argument");
    }

    /// The multiply and the divide that work on a pair of registers, which are the two mnemonics a
    /// template reaches that no rule in this compiler selects. Both read back as the one instruction
    /// they are rather than as the two instruction sequence the same stem is spelled as at a width
    /// the program cannot ask for on its own.
    #[test]
    fn a_multiply_or_a_divide_on_a_pair_of_registers_reads_back_as_the_one_instruction_it_is() {
        let cases = [
            ("mulq %3", "mul_wide_64", 4),
            ("imull %3", "imul_wide_32", 4),
            ("divq %4", "div_wide_64", 5),
            ("idivw %4", "idiv_wide_16", 5),
        ];
        for (template, opcode, operands) in cases {
            let widths = [Some(Width::Quad); 5];
            let lines = plain(template, &widths).unwrap_or_else(|| panic!("{template} is a pair"));
            assert_eq!(lines.len(), 1, "{template} is one instruction");
            assert_eq!(lines[0].opcode, opcode, "{template}");
            assert_eq!(lines[0].operands.len(), operands, "{template}");
        }
    }

    /// Every refusal in the module documentation, held here so that a later change that starts
    /// guessing at one of them has to say so. The divide left in the list is the byte one, since
    /// that is the width whose dividend and both its answers live in one register and so is the one
    /// the pair forms above do not have.
    #[test]
    fn what_cannot_be_placed_is_refused_rather_than_guessed_at() {
        assert_eq!(plain("hcf", &[]), None, "a mnemonic this machine does not have");
        assert_eq!(
            plain("movq %0", &[]),
            None,
            "an instruction with the wrong number of arguments"
        );
        assert_eq!(plain("idivb %0", &[]), None, "an opcode the machine writes as more than one");
        assert_eq!(read("1:", &[]), None, "a local label, which the direction on a jump names");
        assert_eq!(read("jmp again", &[]), None, "an unconditional jump");
        assert_eq!(read("js again", &[]), None, "a condition this backend has no opcode for");
        assert_eq!(read("jc away", &[]), None, "a jump to a label the template does not define");
        assert_eq!(read("again:\njc again\nagain:", &[]), None, "one name on two labels");
        assert_eq!(plain(".byte 0", &[]), None, "a directive");
        assert_eq!(plain("movq (%%rax,%%rbx,8), %0", &[]), None, "a scaled index");
        assert_eq!(plain("movq %%cs:0, %0", &[]), None, "a segment nothing here reaches");
        assert_eq!(plain("movq %%xmm0, %0", &[]), None, "a register in the other file");
    }

    /// `MPN_INCR_U` out of libgmp's `gmp-impl.h`, which is the loop that carries a one along an
    /// integer and is the template this was built to read. A label at the top, an add into the
    /// memory the pointer names, a step of one limb whose size is in an operand, and a jump back
    /// while the addition carries.
    #[test]
    fn a_label_and_a_jump_back_to_it_are_the_loop_a_template_wrote() {
        let widths = [Some(Width::Quad), Some(Width::Quad), Some(Width::Quad)];
        let template = ".Lasm_%=_top:\n\taddq\t$1, (%0)\n\tlea\t%c2(%0), %0\n\tjc\t.Lasm_%=_top";
        let steps = read(template, &widths).expect("the loop gmp writes");
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0], Step::Label(".Lasm_%=_top".to_owned()), "the place it goes back to");
        let Step::Line(ref add) = steps[1] else { panic!("the add into memory: {steps:?}") };
        assert_eq!(add.opcode, "add_mi_64");
        let Step::Line(ref step) = steps[2] else { panic!("the step along: {steps:?}") };
        assert_eq!(step.at.expect("an address").disp, Disp::Operand(2), "the size of a limb");
        let back = Step::Jump { opcode: "jcc_b", to: ".Lasm_%=_top".to_owned() };
        assert_eq!(steps[3], back, "the carry, named after the bit rather than the comparison");
    }

    /// Both spellings of every condition this reads, since a program writes whichever reads better
    /// where it stands and the two are the one instruction.
    #[test]
    fn a_condition_is_read_in_every_spelling_an_assembler_takes_for_it() {
        let cases = [
            ("je", "jz", "jcc_e"),
            ("jne", "jnz", "jcc_ne"),
            ("jl", "jnge", "jcc_l"),
            ("jle", "jng", "jcc_le"),
            ("jg", "jnle", "jcc_g"),
            ("jge", "jnl", "jcc_ge"),
            ("jb", "jc", "jcc_b"),
            ("jbe", "jna", "jcc_be"),
            ("ja", "jnbe", "jcc_a"),
            ("jae", "jnc", "jcc_ae"),
        ];
        for (one, other, opcode) in cases {
            for written in [one, other] {
                let text = format!("again:\n{written} again");
                let steps = read(&text, &[]).unwrap_or_else(|| panic!("{text} is a jump"));
                let want = Step::Jump { opcode, to: "again".to_owned() };
                assert_eq!(steps[1], want, "{text}");
            }
        }
    }

    /// An empty template is no instructions rather than one that could not be read, which is the
    /// case the backend has lowered since before there was a reader.
    #[test]
    fn a_template_with_nothing_in_it_is_no_instructions() {
        assert_eq!(plain("", &[]), Some(Vec::new()));
        assert_eq!(plain("  \n\t # nothing here \n", &[]), Some(Vec::new()));
    }

    /// The two spellings of a number and the sign in front of one, since a displacement is as
    /// often negative as not, and the fourth spelling, which is not a number at all.
    ///
    /// `%c1` is the constant in an operand written the way a distance is written rather than the
    /// way an immediate is. What is in that operand is not known here, so what comes back is which
    /// operand it is and the caller reads it. gmp writes one wherever it steps a limb at a time.
    #[test]
    fn a_displacement_is_read_in_both_spellings_and_both_signs() {
        let cases = [
            ("-8(%%rbp)", Disp::Number(-8)),
            ("0x10(%%rbp)", Disp::Number(16)),
            ("+4(%%rbp)", Disp::Number(4)),
            ("(%%rbp)", Disp::Number(0)),
            ("%c1(%%rbp)", Disp::Operand(1)),
        ];
        for (written, disp) in cases {
            let text = format!("movq {written}, %0");
            let lines = plain(&text, &[]).unwrap_or_else(|| panic!("{text} is a load"));
            assert_eq!(lines[0].at.expect("an address").disp, disp, "{text}");
        }
        assert_eq!(plain("movq %c(%%rbp), %0", &[]), None, "a modifier with no operand");
    }

    /// The three spellings of a boundary, all of which mean the same thing and two of which say it
    /// in bytes while the first says it as a power. zstd writes the first one in front of the match
    /// loop of its lazy matcher, and the other two are what a program written for another assembler
    /// says.
    #[test]
    fn an_alignment_is_read_in_all_three_spellings_and_carries_its_boundary_in_bytes() {
        let cases = [(".p2align 5", 32), (".align 16", 16), (".balign 8", 8), (".p2align 0", 1)];
        for (template, bytes) in cases {
            let lines =
                plain(template, &[]).unwrap_or_else(|| panic!("{template} is an alignment"));
            assert_eq!(lines.len(), 1, "{template}");
            assert_eq!(lines[0].opcode, ALIGN, "{template}");
            assert!(lines[0].operands.is_empty(), "{template}");
            assert_eq!(lines[0].at, None, "{template}");
            assert_eq!(lines[0].imm, Some(bytes), "{template}");
        }
        let lines = plain(".p2align 4\n\tpause", &[]).expect("an alignment in front of one");
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
        assert_eq!(plain(".p2align 4, 0x90", &[]), None, "a fill byte");
        assert_eq!(plain(".p2align 4, 0x90, 8", &[]), None, "a most to skip");
        assert_eq!(plain(".align 24", &[]), None, "a boundary that is not a power of two");
        assert_eq!(plain(".balign 8192", &[]), None, "a boundary larger than a page");
        assert_eq!(plain(".p2align 20", &[]), None, "a power larger than a page");
        assert_eq!(plain(".p2align", &[]), None, "a boundary that was left out");
        assert_eq!(plain(".p2align four", &[]), None, "a boundary that is not a number");
        assert_eq!(plain(".skip 16", &[]), None, "a directive that is not an alignment");
        assert_eq!(plain("rep; .p2align 4", &[]), None, "a prefix in front of one");
    }
}
