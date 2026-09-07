//! What each x86-64 machine instruction is in bytes.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1.
//!
//! The other half of [`crate::x86_64::written`]. That says which instructions of the machine an
//! opcode is, what each of them is called and which operand each argument is drawn from, and this
//! says what the bytes of one of them are. Section 11.1 asks for one description behind both the
//! text and the object file, and this is how the two are one: a caller walks the same list of
//! [`Written`](crate::x86_64::Written) either way and only the last step differs, so an
//! instruction cannot be in the listing and missing from the object, or be written with one
//! operand and encoded with another.
//!
//! # How a row is found
//!
//! By the mnemonic and by what kind of thing each of its arguments is, which is what an assembler
//! does. `movl` is four different instructions depending on whether it is given an immediate, a
//! register, a load or a store, and they share a mnemonic because they do the same thing rather
//! than because they are the same instruction.
//!
//! The immediate is part of the question too. Two families here have a shorter form for a small
//! number: every arithmetic instruction can sign extend one byte instead of carrying four, and
//! the sixty four bit move is ten bytes with the whole number in it and seven with four sign
//! extended bytes. Writing those as their own rows, in front of the general one, is what keeps
//! the choice out of the encoder, where it would be a special case, and in the table, where it is
//! two more lines that a person can check against a manual.
//!
//! # What is not chosen here
//!
//! The short form of a jump. Every jump and call this writes carries a four byte distance,
//! because how far it goes is not known until every block has a place and this encodes one
//! instruction at a time. Picking the two byte form where it fits is relaxation, which
//! `spec/11-asm-objects-debug.md` section 11.1 describes as a pass over the whole function rather
//! than a decision an encoder makes, and it is not written yet. The bytes are correct without it
//! and longer than an assembler's would be.
//!
//! The other accumulator forms are not here either. `addl $1000, %eax` has a five byte encoding
//! that only `eax` can use and a six byte one that any register can, and only the second is
//! written, because the first is a size win of one byte on one register and a row that applies to
//! one register is the kind of row a reader stops checking. A shift by one is the same trade the
//! other way round: the machine has a form that means one and carries no count, and we write the
//! general form with a one in it, which is a byte longer and the same instruction.

use std::fmt;

use crate::regs::PhysReg;
use crate::x86_64::text::{Arg, Width};

use Fits::{Signed8, Signed32};
use Size::{Byte, Double, DoubleQuad, Long, Quad, Single, SingleQuad, Word, WordQuad};

/// What kind of thing one argument of an instruction is.
///
/// The coarse version of [`Arg`]: which operand a register is drawn from and how much of it is
/// read decide what is written, and neither decides which instruction it is. What decides that is
/// whether the argument is a register at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A register, whichever one and however much of it.
    Reg,
    /// A vector register, whichever one.
    ///
    /// Which file a register is in is part of which instruction it is, and this is the one place
    /// that could say so: an argument here is what picks a row, and `movq %rax, %rbx` and
    /// `movq %xmm0, %rax` are the same mnemonic with two register arguments. Nothing else about
    /// the two tells them apart, and a lookup that could not tell them apart would encode a
    /// conversion between the files as a copy inside one of them.
    Vec,
    /// An address.
    Mem,
    /// A number the instruction carries.
    Imm,
    /// Somewhere else in the program, which is what a jump and a call are given.
    Dest,
}

impl Kind {
    /// What kind of argument that is.
    #[must_use]
    pub fn of(arg: Arg) -> Self {
        match arg {
            Arg::Reg(_, _) | Arg::Named(_) | Arg::Through => Kind::Reg,
            Arg::Xmm(_) => Kind::Vec,
            Arg::Mem => Kind::Mem,
            Arg::Imm => Kind::Imm,
            Arg::Symbol | Arg::Label => Kind::Dest,
        }
    }
}

/// What an instruction's prefixes say about the size of what it works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Eight bits, which is no prefix. A REX byte may still be needed, because the four registers
    /// numbered four to seven are `ah`, `ch`, `dh` and `bh` as bytes without one and `spl`,
    /// `bpl`, `sil` and `dil` with one, but that is decided by the register rather than here.
    Byte,
    /// Sixteen bits, which is the `0x66` prefix.
    Word,
    /// Thirty two bits, which is this machine's default and is no prefix either. It is also what
    /// an instruction that is sixty four bits without being told is written as, which is a push,
    /// a pop, a jump, a call and leaving, since telling those would be a byte that says nothing.
    Long,
    /// Sixty four bits, which is `REX.W`.
    Quad,
    /// One `float`, which is the `0xF3` prefix.
    ///
    /// The manual calls this a mandatory prefix rather than a size, because `0xF3` in front of an
    /// SSE opcode is part of which instruction it is rather than a claim about how wide the
    /// operands are: `0F 58` is `addps` and `F3 0F 58` is `addss`. It is here anyway, because
    /// where the byte goes is what this field decides and it goes exactly where `0x66` goes, in
    /// front of the REX byte and behind nothing.
    Single,
    /// One `double`, which is the `0xF2` prefix and is the same kind of thing.
    Double,
    /// The `0x66` prefix with `REX.W` set, which is `movq` between the two register files.
    ///
    /// The three below are each one of the prefixes above and the bit that means sixty four bits,
    /// which is a combination the machine really has and nothing here could say before. They are
    /// where the conversions between an integer and a float at sixty four bits are: `cvtsi2sdq`
    /// is the `0xF2` prefix, because that is what makes the opcode the `double` one, and `REX.W`,
    /// because that is what makes the integer it reads sixty four bits wide, and the two answer
    /// different questions about the same instruction.
    WordQuad,
    /// The `0xF3` prefix with `REX.W` set, which is the `float` conversions at sixty four bits.
    SingleQuad,
    /// The `0xF2` prefix with `REX.W` set, which is the `double` ones.
    DoubleQuad,
}

impl Size {
    /// The byte this size puts in front of the instruction, if it puts one there at all.
    ///
    /// `REX.W` is not here. It is a bit in a byte the registers also write into, so it is set
    /// where that byte is built rather than returned as a prefix of its own.
    const fn prefix(self) -> Option<u8> {
        match self {
            Word | WordQuad => Some(0x66),
            Single | SingleQuad => Some(0xF3),
            Double | DoubleQuad => Some(0xF2),
            Byte | Long | Quad => None,
        }
    }

    /// Whether the REX byte's wide bit is set, which is the other half of what a size says.
    ///
    /// Separate from [`Size::prefix`] because the two go in different bytes and because they are
    /// not the same question. A prefix in front of an SSE opcode says which instruction it is and
    /// this says how wide the general purpose register in it is, which is why four of the sizes
    /// here answer both.
    const fn wide(self) -> bool {
        matches!(self, Quad | WordQuad | SingleQuad | DoubleQuad)
    }
}

/// Where the arguments of an instruction go in the byte that addresses them.
///
/// An index rather than the argument, for the reason [`Arg`] gives: the order an instruction is
/// written in is not the order its operands are in, and an argument the machine needs may be one
/// the assembler does not write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fields {
    /// No addressing byte at all, which is an instruction whose arguments are all in the opcode
    /// or are the distance to somewhere else.
    None,
    /// The argument at that index is addressed, and the three bits beside it in the byte are more
    /// of the opcode rather than a second register. Eight instructions share `0xF7` this way.
    Ext {
        /// The argument the byte addresses, which is a register or an address.
        rm: u8,
        /// The three bits that finish the opcode.
        ext: u8,
    },
    /// The argument at `rm` is addressed and the one at `reg` is the register beside it.
    Pair {
        /// The argument the byte addresses, which is a register or an address.
        rm: u8,
        /// The argument in the register field, which is always a register.
        reg: u8,
    },
    /// The low three bits of that argument's register are added to the last byte of the opcode,
    /// which is how a push, a pop and the ten byte move name theirs.
    Plus {
        /// The argument whose register is in the opcode.
        reg: u8,
    },
}

/// The immediate an instruction carries, behind everything else it is made of.
///
/// Named the way the manual names them, because this is a table a person checks against one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImmSize {
    /// None at all.
    None,
    /// One byte.
    Ib,
    /// Two bytes.
    Iw,
    /// Four bytes.
    Id,
    /// Eight bytes, which only the ten byte move has.
    Io,
    /// Four bytes of signed distance from the end of the instruction, which is what a jump and a
    /// call carry and is filled in once the place it goes to is known.
    Cd,
}

/// Which immediates a row is for.
///
/// Two jobs. It is how one instruction has more than one encoding, since the arithmetic
/// instructions have a short form for a small number and the sixty four bit move has a long one
/// for a big one. And it is how a number too big for any form of an instruction is refused rather
/// than quietly cut down, which would be a compiler that writes a different program from the one
/// it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fits {
    /// Any at all, which is what a row with no immediate takes and what the one instruction with
    /// an eight byte immediate takes.
    Any,
    /// One that fits in a signed byte, which is the short form every arithmetic instruction here
    /// has and is why the general row is written behind it.
    Signed8,
    /// One that fits in four signed bytes, which is as far as an instruction that sign extends
    /// what it carries reaches. That is the seven byte form of the sixty four bit move, and it is
    /// also every sixty four bit arithmetic instruction with an immediate, because four bytes is
    /// the widest immediate the machine has outside that one move.
    Signed32,
    /// One that fits in a byte, counted either way, since a number over a hundred and twenty
    /// seven and the negative one it would be read as are the same eight bits.
    Byte,
    /// One that fits in two bytes, counted either way.
    Word,
    /// One that fits in four bytes, counted either way.
    Long,
}

impl Fits {
    /// Whether this row is one that number may be written with.
    fn holds(self, imm: i64) -> bool {
        match self {
            Fits::Any => true,
            Signed8 => i8::try_from(imm).is_ok(),
            Signed32 => i32::try_from(imm).is_ok(),
            Fits::Byte => i8::try_from(imm).is_ok() || u8::try_from(imm).is_ok(),
            Fits::Word => i16::try_from(imm).is_ok() || u16::try_from(imm).is_ok(),
            Fits::Long => i32::try_from(imm).is_ok() || u32::try_from(imm).is_ok(),
        }
    }
}

/// One instruction of the machine, as a processor reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoding {
    /// The mnemonic, which is the one [`Written`](crate::x86_64::Written) carries.
    pub mnemonic: &'static str,
    /// What each of its arguments is, in the order they are written.
    pub args: &'static [Kind],
    /// Which immediates this row is for.
    pub fits: Fits,
    /// What the prefixes say the operands are.
    pub size: Size,
    /// The bytes of the opcode itself, in front of everything the arguments decide.
    pub opcode: &'static [u8],
    /// Where the arguments go in the byte that addresses them.
    pub fields: Fields,
    /// The immediate behind the rest of it.
    pub imm: ImmSize,
}

/// One row of the table below, for an instruction that carries no immediate or that carries one
/// of any size at all.
const fn bytes(
    mnemonic: &'static str,
    args: &'static [Kind],
    size: Size,
    opcode: &'static [u8],
    fields: Fields,
    imm: ImmSize,
) -> Encoding {
    Encoding { mnemonic, args, fits: Fits::Any, size, opcode, fields, imm }
}

/// One row for an instruction whose immediate has to be of a certain size, which is every row
/// here that carries one but the ten byte move.
const fn takes(
    mnemonic: &'static str,
    args: &'static [Kind],
    fits: Fits,
    size: Size,
    opcode: &'static [u8],
    fields: Fields,
    imm: ImmSize,
) -> Encoding {
    Encoding { mnemonic, args, fits, size, opcode, fields, imm }
}

/// An addressing byte whose spare three bits finish the opcode.
const fn ext(rm: u8, ext: u8) -> Fields {
    Fields::Ext { rm, ext }
}

/// An addressing byte that names two registers, or one register and an address.
const fn pair(rm: u8, reg: u8) -> Fields {
    Fields::Pair { rm, reg }
}

/// A register in the last byte of the opcode.
const fn plus(reg: u8) -> Fields {
    Fields::Plus { reg }
}

/// No addressing byte.
const NO_MODRM: Fields = Fields::None;
/// No immediate.
const NO_IMM: ImmSize = ImmSize::None;

// The argument lists, which are short and repeat, so they are written once and named. AT&T order
// throughout, so the source is in front of the destination.
static NO_ARGS: [Kind; 0] = [];
static R: [Kind; 1] = [Kind::Reg];
static RR: [Kind; 2] = [Kind::Reg, Kind::Reg];
static IR: [Kind; 2] = [Kind::Imm, Kind::Reg];
static IRR: [Kind; 3] = [Kind::Imm, Kind::Reg, Kind::Reg];
static MR: [Kind; 2] = [Kind::Mem, Kind::Reg];
static RM: [Kind; 2] = [Kind::Reg, Kind::Mem];
static D: [Kind; 1] = [Kind::Dest];
static M: [Kind; 1] = [Kind::Mem];
// The same shapes with a vector register in them, which is a different row rather than a different
// spelling of the same one for the reason `Kind::Vec` gives.
static VV: [Kind; 2] = [Kind::Vec, Kind::Vec];
static MV: [Kind; 2] = [Kind::Mem, Kind::Vec];
static VM: [Kind; 2] = [Kind::Vec, Kind::Mem];
static RV: [Kind; 2] = [Kind::Reg, Kind::Vec];
static VR: [Kind; 2] = [Kind::Vec, Kind::Reg];

/// Every instruction [`crate::x86_64::written`] can name, and the bytes it comes out as.
///
/// In the order the opcodes that reach them are described in, and grouped the same way, so that
/// a reader with the manual open can go down all three tables together. An instruction reached
/// from more than one opcode is written once, where it is first reached, which is why the
/// conversions hold the widening a division needs and the arithmetic holds the clearing.
///
/// A row for a small immediate comes in front of the general row for the same instruction,
/// because a lookup takes the first row that fits and the narrower one is the one wanted.
static ENCODINGS: &[Encoding] = &[
    // Constants. The sixty four bit form is ten bytes with the number in it, and seven when four
    // sign extended bytes reach it, which is nearly always.
    takes("movb", &IR, Fits::Byte, Byte, &[0xC6], ext(1, 0), ImmSize::Ib),
    takes("movw", &IR, Fits::Word, Word, &[0xC7], ext(1, 0), ImmSize::Iw),
    takes("movl", &IR, Fits::Long, Long, &[0xC7], ext(1, 0), ImmSize::Id),
    takes("movq", &IR, Signed32, Quad, &[0xC7], ext(1, 0), ImmSize::Id),
    bytes("movq", &IR, Quad, &[0xB8], plus(1), ImmSize::Io),
    // Arithmetic, register with register. The source is written first and is the register beside
    // the addressing byte, and the destination is the one the byte addresses.
    bytes("addb", &RR, Byte, &[0x00], pair(1, 0), NO_IMM),
    bytes("addw", &RR, Word, &[0x01], pair(1, 0), NO_IMM),
    bytes("addl", &RR, Long, &[0x01], pair(1, 0), NO_IMM),
    bytes("addq", &RR, Quad, &[0x01], pair(1, 0), NO_IMM),
    bytes("subb", &RR, Byte, &[0x28], pair(1, 0), NO_IMM),
    bytes("subw", &RR, Word, &[0x29], pair(1, 0), NO_IMM),
    bytes("subl", &RR, Long, &[0x29], pair(1, 0), NO_IMM),
    bytes("subq", &RR, Quad, &[0x29], pair(1, 0), NO_IMM),
    bytes("andb", &RR, Byte, &[0x20], pair(1, 0), NO_IMM),
    bytes("andw", &RR, Word, &[0x21], pair(1, 0), NO_IMM),
    bytes("andl", &RR, Long, &[0x21], pair(1, 0), NO_IMM),
    bytes("andq", &RR, Quad, &[0x21], pair(1, 0), NO_IMM),
    bytes("orb", &RR, Byte, &[0x08], pair(1, 0), NO_IMM),
    bytes("orw", &RR, Word, &[0x09], pair(1, 0), NO_IMM),
    bytes("orl", &RR, Long, &[0x09], pair(1, 0), NO_IMM),
    bytes("orq", &RR, Quad, &[0x09], pair(1, 0), NO_IMM),
    bytes("xorb", &RR, Byte, &[0x30], pair(1, 0), NO_IMM),
    bytes("xorw", &RR, Word, &[0x31], pair(1, 0), NO_IMM),
    bytes("xorl", &RR, Long, &[0x31], pair(1, 0), NO_IMM),
    bytes("xorq", &RR, Quad, &[0x31], pair(1, 0), NO_IMM),
    // The multiply is the other way round from the rest of them: it is not one of the eight that
    // share an opcode column, and the register beside the addressing byte is its destination.
    bytes("imulw", &RR, Word, &[0x0F, 0xAF], pair(0, 1), NO_IMM),
    bytes("imull", &RR, Long, &[0x0F, 0xAF], pair(0, 1), NO_IMM),
    bytes("imulq", &RR, Quad, &[0x0F, 0xAF], pair(0, 1), NO_IMM),
    // Arithmetic, register with immediate. The eight of these share three opcodes and are told
    // apart by the three bits beside the register, which is the column the manual calls `/digit`.
    // Nothing narrower than a word can sign extend a byte, since a byte is already one.
    takes("addb", &IR, Fits::Byte, Byte, &[0x80], ext(1, 0), ImmSize::Ib),
    takes("addw", &IR, Signed8, Word, &[0x83], ext(1, 0), ImmSize::Ib),
    takes("addw", &IR, Fits::Word, Word, &[0x81], ext(1, 0), ImmSize::Iw),
    takes("addl", &IR, Signed8, Long, &[0x83], ext(1, 0), ImmSize::Ib),
    takes("addl", &IR, Fits::Long, Long, &[0x81], ext(1, 0), ImmSize::Id),
    takes("addq", &IR, Signed8, Quad, &[0x83], ext(1, 0), ImmSize::Ib),
    takes("addq", &IR, Signed32, Quad, &[0x81], ext(1, 0), ImmSize::Id),
    takes("subb", &IR, Fits::Byte, Byte, &[0x80], ext(1, 5), ImmSize::Ib),
    takes("subw", &IR, Signed8, Word, &[0x83], ext(1, 5), ImmSize::Ib),
    takes("subw", &IR, Fits::Word, Word, &[0x81], ext(1, 5), ImmSize::Iw),
    takes("subl", &IR, Signed8, Long, &[0x83], ext(1, 5), ImmSize::Ib),
    takes("subl", &IR, Fits::Long, Long, &[0x81], ext(1, 5), ImmSize::Id),
    takes("subq", &IR, Signed8, Quad, &[0x83], ext(1, 5), ImmSize::Ib),
    takes("subq", &IR, Signed32, Quad, &[0x81], ext(1, 5), ImmSize::Id),
    takes("andb", &IR, Fits::Byte, Byte, &[0x80], ext(1, 4), ImmSize::Ib),
    takes("andw", &IR, Signed8, Word, &[0x83], ext(1, 4), ImmSize::Ib),
    takes("andw", &IR, Fits::Word, Word, &[0x81], ext(1, 4), ImmSize::Iw),
    takes("andl", &IR, Signed8, Long, &[0x83], ext(1, 4), ImmSize::Ib),
    takes("andl", &IR, Fits::Long, Long, &[0x81], ext(1, 4), ImmSize::Id),
    takes("andq", &IR, Signed8, Quad, &[0x83], ext(1, 4), ImmSize::Ib),
    takes("andq", &IR, Signed32, Quad, &[0x81], ext(1, 4), ImmSize::Id),
    takes("orb", &IR, Fits::Byte, Byte, &[0x80], ext(1, 1), ImmSize::Ib),
    takes("orw", &IR, Signed8, Word, &[0x83], ext(1, 1), ImmSize::Ib),
    takes("orw", &IR, Fits::Word, Word, &[0x81], ext(1, 1), ImmSize::Iw),
    takes("orl", &IR, Signed8, Long, &[0x83], ext(1, 1), ImmSize::Ib),
    takes("orl", &IR, Fits::Long, Long, &[0x81], ext(1, 1), ImmSize::Id),
    takes("orq", &IR, Signed8, Quad, &[0x83], ext(1, 1), ImmSize::Ib),
    takes("orq", &IR, Signed32, Quad, &[0x81], ext(1, 1), ImmSize::Id),
    takes("xorb", &IR, Fits::Byte, Byte, &[0x80], ext(1, 6), ImmSize::Ib),
    takes("xorw", &IR, Signed8, Word, &[0x83], ext(1, 6), ImmSize::Ib),
    takes("xorw", &IR, Fits::Word, Word, &[0x81], ext(1, 6), ImmSize::Iw),
    takes("xorl", &IR, Signed8, Long, &[0x83], ext(1, 6), ImmSize::Ib),
    takes("xorl", &IR, Fits::Long, Long, &[0x81], ext(1, 6), ImmSize::Id),
    takes("xorq", &IR, Signed8, Quad, &[0x83], ext(1, 6), ImmSize::Ib),
    takes("xorq", &IR, Signed32, Quad, &[0x81], ext(1, 6), ImmSize::Id),
    // The three-operand multiply, whose source and destination are both written because they are
    // not the same register and whose immediate narrows the same way the eight above do.
    takes("imulw", &IRR, Signed8, Word, &[0x6B], pair(1, 2), ImmSize::Ib),
    takes("imulw", &IRR, Fits::Word, Word, &[0x69], pair(1, 2), ImmSize::Iw),
    takes("imull", &IRR, Signed8, Long, &[0x6B], pair(1, 2), ImmSize::Ib),
    takes("imull", &IRR, Fits::Long, Long, &[0x69], pair(1, 2), ImmSize::Id),
    takes("imulq", &IRR, Signed8, Quad, &[0x6B], pair(1, 2), ImmSize::Ib),
    takes("imulq", &IRR, Signed32, Quad, &[0x69], pair(1, 2), ImmSize::Id),
    // Negation and complement, which are two more of the eight that share `0xF7`.
    bytes("negb", &R, Byte, &[0xF6], ext(0, 3), NO_IMM),
    bytes("negw", &R, Word, &[0xF7], ext(0, 3), NO_IMM),
    bytes("negl", &R, Long, &[0xF7], ext(0, 3), NO_IMM),
    bytes("negq", &R, Quad, &[0xF7], ext(0, 3), NO_IMM),
    bytes("notb", &R, Byte, &[0xF6], ext(0, 2), NO_IMM),
    bytes("notw", &R, Word, &[0xF7], ext(0, 2), NO_IMM),
    bytes("notl", &R, Long, &[0xF7], ext(0, 2), NO_IMM),
    bytes("notq", &R, Quad, &[0xF7], ext(0, 2), NO_IMM),
    // The four widenings a division needs, each of which is one byte and a prefix. They read one
    // fixed register and write another and name neither, which is why they have no arguments.
    bytes("cbtw", &NO_ARGS, Word, &[0x98], NO_MODRM, NO_IMM),
    bytes("cwtd", &NO_ARGS, Word, &[0x99], NO_MODRM, NO_IMM),
    bytes("cltd", &NO_ARGS, Long, &[0x99], NO_MODRM, NO_IMM),
    bytes("cqto", &NO_ARGS, Quad, &[0x99], NO_MODRM, NO_IMM),
    // The divisions themselves, which are the last two of the eight.
    bytes("idivb", &R, Byte, &[0xF6], ext(0, 7), NO_IMM),
    bytes("idivw", &R, Word, &[0xF7], ext(0, 7), NO_IMM),
    bytes("idivl", &R, Long, &[0xF7], ext(0, 7), NO_IMM),
    bytes("idivq", &R, Quad, &[0xF7], ext(0, 7), NO_IMM),
    bytes("divb", &R, Byte, &[0xF6], ext(0, 6), NO_IMM),
    bytes("divw", &R, Word, &[0xF7], ext(0, 6), NO_IMM),
    bytes("divl", &R, Long, &[0xF7], ext(0, 6), NO_IMM),
    bytes("divq", &R, Quad, &[0xF7], ext(0, 6), NO_IMM),
    // Shifts by a constant, which carry one byte of count however wide the thing shifted is,
    // because nothing shifts a register by more than sixty three places.
    takes("shlb", &IR, Fits::Byte, Byte, &[0xC0], ext(1, 4), ImmSize::Ib),
    takes("shlw", &IR, Fits::Byte, Word, &[0xC1], ext(1, 4), ImmSize::Ib),
    takes("shll", &IR, Fits::Byte, Long, &[0xC1], ext(1, 4), ImmSize::Ib),
    takes("shlq", &IR, Fits::Byte, Quad, &[0xC1], ext(1, 4), ImmSize::Ib),
    takes("shrb", &IR, Fits::Byte, Byte, &[0xC0], ext(1, 5), ImmSize::Ib),
    takes("shrw", &IR, Fits::Byte, Word, &[0xC1], ext(1, 5), ImmSize::Ib),
    takes("shrl", &IR, Fits::Byte, Long, &[0xC1], ext(1, 5), ImmSize::Ib),
    takes("shrq", &IR, Fits::Byte, Quad, &[0xC1], ext(1, 5), ImmSize::Ib),
    takes("sarb", &IR, Fits::Byte, Byte, &[0xC0], ext(1, 7), ImmSize::Ib),
    takes("sarw", &IR, Fits::Byte, Word, &[0xC1], ext(1, 7), ImmSize::Ib),
    takes("sarl", &IR, Fits::Byte, Long, &[0xC1], ext(1, 7), ImmSize::Ib),
    takes("sarq", &IR, Fits::Byte, Quad, &[0xC1], ext(1, 7), ImmSize::Ib),
    // Shifts by a count, which is written and not encoded: the machine reads it from `cl` and
    // there is nowhere in the instruction to say so. The count is the argument at zero, and every
    // row here addresses the argument at one.
    bytes("shlb", &RR, Byte, &[0xD2], ext(1, 4), NO_IMM),
    bytes("shlw", &RR, Word, &[0xD3], ext(1, 4), NO_IMM),
    bytes("shll", &RR, Long, &[0xD3], ext(1, 4), NO_IMM),
    bytes("shlq", &RR, Quad, &[0xD3], ext(1, 4), NO_IMM),
    bytes("shrb", &RR, Byte, &[0xD2], ext(1, 5), NO_IMM),
    bytes("shrw", &RR, Word, &[0xD3], ext(1, 5), NO_IMM),
    bytes("shrl", &RR, Long, &[0xD3], ext(1, 5), NO_IMM),
    bytes("shrq", &RR, Quad, &[0xD3], ext(1, 5), NO_IMM),
    bytes("sarb", &RR, Byte, &[0xD2], ext(1, 7), NO_IMM),
    bytes("sarw", &RR, Word, &[0xD3], ext(1, 7), NO_IMM),
    bytes("sarl", &RR, Long, &[0xD3], ext(1, 7), NO_IMM),
    bytes("sarq", &RR, Quad, &[0xD3], ext(1, 7), NO_IMM),
    // The comparison, which is the eighth of the ones that share an opcode column and is written
    // the same way round as the subtraction it is.
    bytes("cmpb", &RR, Byte, &[0x38], pair(1, 0), NO_IMM),
    bytes("cmpw", &RR, Word, &[0x39], pair(1, 0), NO_IMM),
    bytes("cmpl", &RR, Long, &[0x39], pair(1, 0), NO_IMM),
    bytes("cmpq", &RR, Quad, &[0x39], pair(1, 0), NO_IMM),
    // The byte each condition sets, which is one opcode with the condition in its low four bits.
    // The conditional move, one opcode with the condition in its low four bits, the same way the
    // sets above are. Only the three widths the machine has: there is no eight bit conditional
    // move and the eight bit form of `select` is written with the thirty two bit one.
    //
    // The operands are the other way round from every move above it. `0F 45` reads a register or
    // memory and writes a register, so the register field is the destination, where in `88` and
    // `89` it is the source. The mnemonic order is the same in both and only the byte after the
    // opcode differs, which is exactly the kind of thing a table gets wrong silently.
    bytes("cmovnew", &RR, Word, &[0x0F, 0x45], pair(0, 1), NO_IMM),
    bytes("cmovnel", &RR, Long, &[0x0F, 0x45], pair(0, 1), NO_IMM),
    bytes("cmovneq", &RR, Quad, &[0x0F, 0x45], pair(0, 1), NO_IMM),
    bytes("sete", &R, Byte, &[0x0F, 0x94], ext(0, 0), NO_IMM),
    bytes("setne", &R, Byte, &[0x0F, 0x95], ext(0, 0), NO_IMM),
    bytes("setl", &R, Byte, &[0x0F, 0x9C], ext(0, 0), NO_IMM),
    bytes("setle", &R, Byte, &[0x0F, 0x9E], ext(0, 0), NO_IMM),
    bytes("setg", &R, Byte, &[0x0F, 0x9F], ext(0, 0), NO_IMM),
    bytes("setge", &R, Byte, &[0x0F, 0x9D], ext(0, 0), NO_IMM),
    bytes("setb", &R, Byte, &[0x0F, 0x92], ext(0, 0), NO_IMM),
    bytes("setbe", &R, Byte, &[0x0F, 0x96], ext(0, 0), NO_IMM),
    bytes("seta", &R, Byte, &[0x0F, 0x97], ext(0, 0), NO_IMM),
    bytes("setae", &R, Byte, &[0x0F, 0x93], ext(0, 0), NO_IMM),
    // The two conditions on the parity flag, which are here because a float comparison is the one
    // thing on this machine that sets it for a reason anybody wants. It says the two operands were
    // not ordered, which is to say one of them was a NaN.
    bytes("setp", &R, Byte, &[0x0F, 0x9A], ext(0, 0), NO_IMM),
    bytes("setnp", &R, Byte, &[0x0F, 0x9B], ext(0, 0), NO_IMM),
    // The conversions between widths, which read a register and write a wider one, so the
    // destination is the register beside the addressing byte rather than the one it addresses.
    // How wide the source is decides the opcode and how wide the destination is decides the
    // prefix, which is why five opcodes make eleven instructions.
    bytes("movzbw", &RR, Word, &[0x0F, 0xB6], pair(0, 1), NO_IMM),
    bytes("movzbl", &RR, Long, &[0x0F, 0xB6], pair(0, 1), NO_IMM),
    bytes("movzbq", &RR, Quad, &[0x0F, 0xB6], pair(0, 1), NO_IMM),
    bytes("movzwl", &RR, Long, &[0x0F, 0xB7], pair(0, 1), NO_IMM),
    bytes("movzwq", &RR, Quad, &[0x0F, 0xB7], pair(0, 1), NO_IMM),
    bytes("movsbw", &RR, Word, &[0x0F, 0xBE], pair(0, 1), NO_IMM),
    bytes("movsbl", &RR, Long, &[0x0F, 0xBE], pair(0, 1), NO_IMM),
    bytes("movsbq", &RR, Quad, &[0x0F, 0xBE], pair(0, 1), NO_IMM),
    bytes("movswl", &RR, Long, &[0x0F, 0xBF], pair(0, 1), NO_IMM),
    bytes("movswq", &RR, Quad, &[0x0F, 0xBF], pair(0, 1), NO_IMM),
    bytes("movslq", &RR, Quad, &[0x63], pair(0, 1), NO_IMM),
    // A copy between registers, which is a store to a register rather than a load from one, so it
    // is written the same way round as the arithmetic above and not as the conversions.
    bytes("movb", &RR, Byte, &[0x88], pair(1, 0), NO_IMM),
    bytes("movw", &RR, Word, &[0x89], pair(1, 0), NO_IMM),
    bytes("movl", &RR, Long, &[0x89], pair(1, 0), NO_IMM),
    bytes("movq", &RR, Quad, &[0x89], pair(1, 0), NO_IMM),
    // The address computation, which is the one instruction that is given an address and does not
    // read it.
    bytes("leaq", &MR, Quad, &[0x8D], pair(0, 1), NO_IMM),
    // Reading and writing memory, which are one opcode apart and are the same instruction with
    // the two ends swapped.
    bytes("movb", &MR, Byte, &[0x8A], pair(0, 1), NO_IMM),
    bytes("movw", &MR, Word, &[0x8B], pair(0, 1), NO_IMM),
    bytes("movl", &MR, Long, &[0x8B], pair(0, 1), NO_IMM),
    bytes("movq", &MR, Quad, &[0x8B], pair(0, 1), NO_IMM),
    bytes("movb", &RM, Byte, &[0x88], pair(1, 0), NO_IMM),
    bytes("movw", &RM, Word, &[0x89], pair(1, 0), NO_IMM),
    bytes("movl", &RM, Long, &[0x89], pair(1, 0), NO_IMM),
    bytes("movq", &RM, Quad, &[0x89], pair(1, 0), NO_IMM),
    // A call, whose distance to the function it goes to is not known here.
    bytes("call", &D, Long, &[0xE8], NO_MODRM, ImmSize::Cd),
    // The same mnemonic through an address, which is a different row rather than a different
    // mnemonic because a lookup here is by what the arguments are and not only by what the
    // instruction is called. It is one of the eight that share `0xFF` and is told from the rest by
    // the three bits beside the register. Sixty four bits without a prefix saying so, the way a
    // jump and a push are, since there is no form of it that calls a thirty two bit address.
    bytes("call", &R, Long, &[0xFF], ext(0, 2), NO_IMM),
    // What a condition and the block layout come to. The test is a comparison against zero that
    // names the same register twice, so both of its arguments are the one operand.
    bytes("testb", &RR, Byte, &[0x84], pair(1, 0), NO_IMM),
    bytes("je", &D, Long, &[0x0F, 0x84], NO_MODRM, ImmSize::Cd),
    bytes("jne", &D, Long, &[0x0F, 0x85], NO_MODRM, ImmSize::Cd),
    bytes("jmp", &D, Long, &[0xE9], NO_MODRM, ImmSize::Cd),
    // What a prologue and an epilogue are made of. A push and a pop move eight bytes without
    // being told to, so neither carries the prefix that would say so.
    bytes("pushq", &R, Long, &[0x50], plus(0), NO_IMM),
    bytes("popq", &R, Long, &[0x58], plus(0), NO_IMM),
    bytes("ret", &NO_ARGS, Long, &[0xC3], NO_MODRM, NO_IMM),
    // The barrier. Three bytes with no operands, so the last of them is written as part of the
    // opcode rather than built: `0xF0` is the addressing byte that names no memory and no
    // register, and there is nothing here that could choose a different one.
    bytes("mfence", &NO_ARGS, Long, &[0x0F, 0xAE, 0xF0], NO_MODRM, NO_IMM),
    // The vector moves, which are the same three shapes as the general purpose ones and are one
    // opcode apart the same way.
    bytes("movaps", &VV, Long, &[0x0F, 0x28], pair(0, 1), NO_IMM),
    bytes("movaps", &MV, Long, &[0x0F, 0x28], pair(0, 1), NO_IMM),
    bytes("movaps", &VM, Long, &[0x0F, 0x29], pair(1, 0), NO_IMM),
    // A scalar move, which is the same pair of opcodes one lower and behind the prefix that says
    // which format it is. The load is `0x10` and the store is `0x11`, the way `movaps` is `0x28`
    // and `0x29`, and the destination is the register beside the addressing byte in both.
    bytes("movss", &MV, Single, &[0x0F, 0x10], pair(0, 1), NO_IMM),
    bytes("movsd", &MV, Double, &[0x0F, 0x10], pair(0, 1), NO_IMM),
    bytes("movss", &VM, Single, &[0x0F, 0x11], pair(1, 0), NO_IMM),
    bytes("movsd", &VM, Double, &[0x0F, 0x11], pair(1, 0), NO_IMM),
    // Scalar arithmetic. The four opcodes are consecutive, which is worth reading as a group: add
    // is `0x58`, multiply `0x59`, subtract `0x5C` and divide `0x5E`, and the `float` and the
    // `double` of each are the same byte behind a different prefix. The destination is the
    // register beside the addressing byte here, the opposite way round from the integer
    // arithmetic, because these instructions read their addressed operand and write the other.
    bytes("addss", &VV, Single, &[0x0F, 0x58], pair(0, 1), NO_IMM),
    bytes("addsd", &VV, Double, &[0x0F, 0x58], pair(0, 1), NO_IMM),
    bytes("mulss", &VV, Single, &[0x0F, 0x59], pair(0, 1), NO_IMM),
    bytes("mulsd", &VV, Double, &[0x0F, 0x59], pair(0, 1), NO_IMM),
    bytes("subss", &VV, Single, &[0x0F, 0x5C], pair(0, 1), NO_IMM),
    bytes("subsd", &VV, Double, &[0x0F, 0x5C], pair(0, 1), NO_IMM),
    bytes("divss", &VV, Single, &[0x0F, 0x5E], pair(0, 1), NO_IMM),
    bytes("divsd", &VV, Double, &[0x0F, 0x5E], pair(0, 1), NO_IMM),
    // One format to the other, which is one opcode with the prefix saying which way round it
    // goes: behind `0xF3` it reads a `float` and writes a `double` and behind `0xF2` it does the
    // opposite, because the prefix says what the instruction reads.
    bytes("cvtss2sd", &VV, Single, &[0x0F, 0x5A], pair(0, 1), NO_IMM),
    bytes("cvtsd2ss", &VV, Double, &[0x0F, 0x5A], pair(0, 1), NO_IMM),
    // A float to an integer, cutting towards zero, which is the rounding C asks for and is why
    // the mnemonic has two `t`s in it: `cvtss2si` is the one that rounds and no C conversion
    // wants it. The prefix says which format is read and `REX.W` says how wide the integer
    // written is, which is the pair of questions the four rows are the four answers to. The
    // suffix on the mnemonic is what tells two of these rows apart, since a row is found by what
    // its arguments are and both widths of the answer are a general purpose register.
    bytes("cvttss2sil", &VR, Single, &[0x0F, 0x2C], pair(0, 1), NO_IMM),
    bytes("cvttss2siq", &VR, SingleQuad, &[0x0F, 0x2C], pair(0, 1), NO_IMM),
    bytes("cvttsd2sil", &VR, Double, &[0x0F, 0x2C], pair(0, 1), NO_IMM),
    bytes("cvttsd2siq", &VR, DoubleQuad, &[0x0F, 0x2C], pair(0, 1), NO_IMM),
    // An integer to a float, which is the same two questions the other way round and one opcode
    // lower. The mnemonic carries the width of the integer here because the register it reads is
    // the one the assembler cannot see in a memory form, and this table writes the suffix on
    // every one of them so that the four read as four rather than as two written twice.
    bytes("cvtsi2ssl", &RV, Single, &[0x0F, 0x2A], pair(0, 1), NO_IMM),
    bytes("cvtsi2ssq", &RV, SingleQuad, &[0x0F, 0x2A], pair(0, 1), NO_IMM),
    bytes("cvtsi2sdl", &RV, Double, &[0x0F, 0x2A], pair(0, 1), NO_IMM),
    bytes("cvtsi2sdq", &RV, DoubleQuad, &[0x0F, 0x2A], pair(0, 1), NO_IMM),
    // The same bits moved from one file to the other, which is what a reinterpretation is. Two
    // opcodes, `0x6E` towards the vector register and `0x7E` away from it, and the mnemonic is
    // the width rather than the direction because the direction is which argument is which.
    bytes("movd", &RV, Word, &[0x0F, 0x6E], pair(0, 1), NO_IMM),
    bytes("movq", &RV, WordQuad, &[0x0F, 0x6E], pair(0, 1), NO_IMM),
    bytes("movd", &VR, Word, &[0x0F, 0x7E], pair(1, 0), NO_IMM),
    bytes("movq", &VR, WordQuad, &[0x0F, 0x7E], pair(1, 0), NO_IMM),
    // Comparing two floats and setting the flags, which is one opcode with the prefix saying which
    // format is read: no prefix for a `float` and `0x66` for a `double`, which is the pairing the
    // moves at the top of this group have and not the one the arithmetic has. The register beside
    // the addressing byte is the left hand side, so the comparison reads the same way round as
    // `cvtss2sd` and the opposite way round from `cmpl`.
    bytes("ucomiss", &VV, Long, &[0x0F, 0x2E], pair(0, 1), NO_IMM),
    bytes("ucomisd", &VV, Word, &[0x0F, 0x2E], pair(0, 1), NO_IMM),
    // The two x87 instructions, which are the same opcode with a different extension in the
    // addressing byte: `0xDB` with five is the load and with seven is the store. One argument
    // each, because the other end of the move is the top of the x87 stack and there is nothing in
    // the instruction that says so. No size at all, in the sense every other row here means it:
    // the operand is ten bytes and nothing about `0xDB` is variable, so there is no prefix and no
    // `REX.W`, and `Long` is written because that is what this table calls a row with neither.
    bytes("fldt", &M, Long, &[0xDB], ext(0, 5), NO_IMM),
    bytes("fstpt", &M, Long, &[0xDB], ext(0, 7), NO_IMM),
    // The conversions, which are the same instruction reading and writing another format. The
    // width of the operand is in the opcode byte, which is the arrangement this corner of the
    // machine has instead of a prefix: `0xD9` is four bytes, `0xDD` is eight bytes of float,
    // `0xDB` is four bytes of integer and `0xDF` is eight, and the extension in the addressing
    // byte says load or store within each. That is why these rows look unlike every other row
    // here, where a width is a prefix or a REX bit and the opcode is the operation.
    bytes("flds", &M, Long, &[0xD9], ext(0, 0), NO_IMM),
    bytes("fldl", &M, Long, &[0xDD], ext(0, 0), NO_IMM),
    bytes("fildl", &M, Long, &[0xDB], ext(0, 0), NO_IMM),
    bytes("fildll", &M, Long, &[0xDF], ext(0, 5), NO_IMM),
    bytes("fstps", &M, Long, &[0xD9], ext(0, 3), NO_IMM),
    bytes("fstpl", &M, Long, &[0xDD], ext(0, 3), NO_IMM),
    bytes("fistpl", &M, Long, &[0xDB], ext(0, 3), NO_IMM),
    bytes("fistpll", &M, Long, &[0xDF], ext(0, 7), NO_IMM),
    // The control word, which is two bytes and is the operand of two more extensions of `0xD9`.
    bytes("fnstcw", &M, Long, &[0xD9], ext(0, 7), NO_IMM),
    bytes("fldcw", &M, Long, &[0xD9], ext(0, 5), NO_IMM),
];

/// The encoding of the instruction of that mnemonic, given those arguments and that immediate.
///
/// `None` for a mnemonic this target does not encode, for one it does encode with arguments that
/// are not the ones it takes, and for an immediate no form of it can carry.
///
/// The immediate is asked for because it is part of which instruction this is: two families here
/// have a shorter encoding for a small number, and every one of them has a largest number it can
/// hold. Pass zero for an instruction that carries none, which is what the first row of every
/// such mnemonic accepts anyway.
#[must_use]
pub fn encoding(mnemonic: &str, args: &[Kind], imm: i64) -> Option<&'static Encoding> {
    rows(mnemonic, args).find(|row| row.fits.holds(imm))
}

/// Every row of that mnemonic with those arguments, in the order they are written.
fn rows<'a>(mnemonic: &'a str, args: &'a [Kind]) -> impl Iterator<Item = &'static Encoding> + 'a {
    ENCODINGS.iter().filter(move |row| row.mnemonic == mnemonic && row.args == args)
}

/// An address, with everything about it already decided.
///
/// The machine IR's addressing mode names its registers by where they are in the operand vector
/// and this names them outright, because by here the allocator has run and there is an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Addr {
    /// The register the address starts from, if there is one.
    pub base: Option<PhysReg>,
    /// The register added to it, if there is one. It may not be the stack pointer, which is the
    /// number the encoding uses to say there is no index at all.
    pub index: Option<PhysReg>,
    /// What the index is multiplied by, which is one, two, four or eight. Ignored when there is
    /// no index.
    pub scale: u8,
    /// The constant added to the rest of it.
    pub disp: i32,
    /// Whether the address is counted from the end of the instruction rather than from a
    /// register, which is how a global is reached in position independent code and is the only
    /// way this compiler reaches one. It names no register, so it has neither base nor index.
    pub rip: bool,
}

/// What one argument of an instruction turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    /// A register, and how much of it the instruction reads or writes.
    ///
    /// The width is here because it is what decides whether a REX byte is needed at all: the four
    /// registers numbered four to seven are `ah`, `ch`, `dh` and `bh` as bytes without one and
    /// `spl`, `bpl`, `sil` and `dil` with one, so a byte instruction naming one of the second set
    /// carries a REX byte that says nothing else.
    Reg(PhysReg, Width),
    /// A vector register, all of which every instruction here that names one reads or writes.
    ///
    /// [`Value::Reg`] in the other file, and separate for the reason [`Kind::Vec`] gives. There is
    /// no width, because there is nothing narrower than the whole of one to name: an instruction
    /// that works on the low four bytes of a vector register is a different opcode rather than the
    /// same opcode at another width, which is what `movss` and `movsd` are.
    Xmm(PhysReg),
    /// The byte above the low byte of one of the first four registers, which on this machine is
    /// only ever `ah`.
    ///
    /// It is numbered like `spl` and told apart from it by the instruction having no REX byte,
    /// which is why an instruction with one of these may name no register that needs one.
    High(PhysReg),
    /// An address.
    Mem(Addr),
    /// The number an immediate carries.
    Imm(i64),
    /// Somewhere else in the program, whose distance from here is not known yet.
    Dest,
}

impl Value {
    /// What kind of argument this is, which is half of what picks an encoding.
    #[must_use]
    pub fn kind(self) -> Kind {
        match self {
            Value::Reg(_, _) | Value::High(_) => Kind::Reg,
            Value::Xmm(_) => Kind::Vec,
            Value::Mem(_) => Kind::Mem,
            Value::Imm(_) => Kind::Imm,
            Value::Dest => Kind::Dest,
        }
    }
}

/// Where in an instruction something the encoder could not know goes.
///
/// Both are offsets into the buffer the instruction was written to rather than into the
/// instruction, since what the caller has to do with either is patch the buffer or record a
/// relocation against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Holes {
    /// Where the four bytes a jump or a call leaves for the distance to its target begin.
    pub dest: Option<usize>,
    /// Where the four bytes an address counted from the end of the instruction leaves for its
    /// displacement begin.
    pub rip: Option<usize>,
}

/// Why an instruction could not be encoded.
///
/// Every one of these is a bug in the compiler rather than anything a program could ask for, so
/// they carry enough to say which instruction it was and nothing more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Nothing here encodes that mnemonic with those arguments.
    Unwritten {
        /// The mnemonic that was asked for.
        mnemonic: String,
        /// What its arguments were.
        args: Vec<Kind>,
    },
    /// An immediate no form of that instruction can carry, which the machine cannot write and
    /// which would be a different number if it were cut down to fit.
    Immediate {
        /// The mnemonic that was asked for.
        mnemonic: String,
        /// The number that would not fit.
        imm: i64,
    },
    /// An instruction naming `ah` and also a register that cannot be named without a REX byte,
    /// which is a pair the encoding has no way to write.
    Crowded {
        /// The mnemonic that was asked for.
        mnemonic: String,
    },
    /// A scale that is not one of the four the machine has.
    Scale {
        /// What was asked for.
        scale: u8,
    },
    /// The stack pointer as an index, which is the one register that cannot be one, because its
    /// number is what the encoding uses to say there is no index.
    Index,
    /// An argument that is not the kind the row said it was, which cannot happen through
    /// [`encode`] and can through a row that disagrees with itself.
    Argument {
        /// The mnemonic that was asked for.
        mnemonic: String,
        /// Which argument it was.
        at: u8,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unwritten { mnemonic, args } => {
                write!(f, "no encoding for {mnemonic} with {} arguments {args:?}", args.len())
            }
            Error::Immediate { mnemonic, imm } => {
                write!(f, "no form of {mnemonic} can carry the immediate {imm}")
            }
            Error::Crowded { mnemonic } => {
                write!(f, "{mnemonic} names ah and a register that needs a rex byte")
            }
            Error::Scale { scale } => write!(f, "{scale} is not a scale this machine has"),
            Error::Index => write!(f, "the stack pointer cannot be an index"),
            Error::Argument { mnemonic, at } => {
                write!(f, "argument {at} of {mnemonic} is not what its encoding expects")
            }
        }
    }
}

impl std::error::Error for Error {}

/// The bit of a REX byte that says the operands are sixty four bits.
const REX_W: u8 = 0b1000;
/// The bit that carries the top of the register beside the addressing byte.
const REX_R: u8 = 0b0100;
/// The bit that carries the top of an index register.
const REX_X: u8 = 0b0010;
/// The bit that carries the top of the register the addressing byte addresses, which is also the
/// top of a base register and of a register in the opcode.
const REX_B: u8 = 0b0001;

/// Writes one instruction of the machine onto the end of `out`.
///
/// The values are the arguments in the order they are written, which is the order
/// [`Written::args`](crate::x86_64::Written::args) holds them in, so a caller resolves each of
/// those and hands the results here.
///
/// # Errors
///
/// [`Error::Unwritten`] for an instruction this does not encode, and the rest for an instruction
/// it does encode that was handed something the machine cannot express. All of them are bugs
/// rather than anything a program could ask for. See [`Error`].
pub fn encode(mnemonic: &str, values: &[Value], out: &mut Vec<u8>) -> Result<Holes, Error> {
    let args: Vec<Kind> = values.iter().map(|value| value.kind()).collect();
    let imm = values
        .iter()
        .find_map(|value| match value {
            Value::Imm(number) => Some(*number),
            _ => None,
        })
        .unwrap_or(0);
    let Some(row) = encoding(mnemonic, &args, imm) else {
        // Which of the two it is says something different to whoever reads it. An instruction
        // with no row at all is a hole in this description, and one whose rows are all too narrow
        // is a lowering that produced a constant the instruction it chose cannot hold.
        return Err(if rows(mnemonic, &args).next().is_some() {
            Error::Immediate { mnemonic: mnemonic.to_owned(), imm }
        } else {
            Error::Unwritten { mnemonic: mnemonic.to_owned(), args }
        });
    };
    Writer { row, values, rex: 0, forced: false, banned: false }.write(out, imm)
}

/// One instruction being written out.
struct Writer<'a> {
    row: &'a Encoding,
    values: &'a [Value],
    /// The low four bits of the REX byte, which are the tops of the register numbers.
    rex: u8,
    /// Whether a REX byte has to be written even when it would say nothing, which is what naming
    /// one of the four registers that are only bytes with one asks for.
    forced: bool,
    /// Whether one may not be written at all, which is what naming `ah` asks for.
    banned: bool,
}

impl Writer<'_> {
    /// The whole instruction: what the arguments come to, then the bytes in the order they go in.
    ///
    /// The addressing byte and everything behind it are worked out before anything is written,
    /// because they are what says whether there is a REX byte and the REX byte goes in front.
    fn write(mut self, out: &mut Vec<u8>, imm: i64) -> Result<Holes, Error> {
        let mut tail = Vec::new();
        let mut holes = Holes::default();
        let mut plus = 0;
        match self.row.fields {
            Fields::None => {}
            Fields::Ext { rm, ext } => self.address(rm, ext, &mut tail, &mut holes)?,
            Fields::Pair { rm, reg } => {
                let reg = self.number(reg, REX_R)?;
                self.address(rm, reg, &mut tail, &mut holes)?;
            }
            Fields::Plus { reg } => plus = self.number(reg, REX_B)?,
        }
        if self.banned && (self.forced || self.rex != 0) {
            return Err(Error::Crowded { mnemonic: self.row.mnemonic.to_owned() });
        }

        if let Some(prefix) = self.row.size.prefix() {
            out.push(prefix);
        }
        let rex = if self.row.size.wide() { self.rex | REX_W } else { self.rex };
        if rex != 0 || (self.forced && !self.banned) {
            out.push(0x40 | rex);
        }
        let (last, front) = self.row.opcode.split_last().expect("an opcode is at least one byte");
        out.extend_from_slice(front);
        out.push(last + plus);
        // The offsets were taken against an empty buffer, so they move by however much is in
        // front of the addressing byte by the time it is really written.
        let at = out.len();
        for hole in [&mut holes.dest, &mut holes.rip].into_iter().flatten() {
            *hole += at;
        }
        out.extend_from_slice(&tail);

        match self.row.imm {
            ImmSize::None => {}
            ImmSize::Ib => out.push(imm as u8),
            ImmSize::Iw => out.extend_from_slice(&(imm as u16).to_le_bytes()),
            ImmSize::Id => out.extend_from_slice(&(imm as u32).to_le_bytes()),
            ImmSize::Io => out.extend_from_slice(&imm.to_le_bytes()),
            ImmSize::Cd => {
                holes.dest = Some(out.len());
                out.extend_from_slice(&0i32.to_le_bytes());
            }
        }
        Ok(holes)
    }

    /// The number of the register at that index, with its top bit put in the REX byte.
    fn number(&mut self, at: u8, bit: u8) -> Result<u8, Error> {
        match self.values.get(usize::from(at)) {
            Some(&Value::Reg(reg, width)) => {
                let number = reg.number();
                if number >= 8 {
                    self.rex |= bit;
                }
                // The one thing a width decides about the bytes. Every other difference between
                // an eight, a sixteen, a thirty two and a sixty four bit instruction is in the
                // opcode or in the prefixes, and those are on the row.
                if width == Width::Byte && (4..8).contains(&number) {
                    self.forced = true;
                }
                Ok(number & 7)
            }
            // A vector register is numbered the way a general purpose one is and there is no
            // width to look at, since the whole of it is what the instruction works on.
            Some(&Value::Xmm(reg)) => {
                let number = reg.number();
                if number >= 8 {
                    self.rex |= bit;
                }
                Ok(number & 7)
            }
            // `ah` is `al` plus four, and so are the other three, which is also why only the
            // first four registers have one.
            Some(&Value::High(reg)) if reg.number() < 4 => {
                self.banned = true;
                Ok(reg.number() + 4)
            }
            _ => Err(Error::Argument { mnemonic: self.row.mnemonic.to_owned(), at }),
        }
    }

    /// The addressing byte and whatever follows it, which is a register or a whole address.
    ///
    /// `reg` is the three bits beside the addressed one, which is either a register that has
    /// already been worked out or the rest of the opcode.
    fn address(
        &mut self,
        at: u8,
        reg: u8,
        out: &mut Vec<u8>,
        holes: &mut Holes,
    ) -> Result<(), Error> {
        match self.values.get(usize::from(at)) {
            Some(Value::Mem(addr)) => self.mem(*addr, reg, out, holes),
            Some(_) => {
                let rm = self.number(at, REX_B)?;
                out.push(0b1100_0000 | (reg << 3) | rm);
                Ok(())
            }
            None => Err(Error::Argument { mnemonic: self.row.mnemonic.to_owned(), at }),
        }
    }

    /// One address, as the addressing byte and the two things that can follow it.
    fn mem(
        &mut self,
        addr: Addr,
        reg: u8,
        out: &mut Vec<u8>,
        holes: &mut Holes,
    ) -> Result<(), Error> {
        // Counted from the end of the instruction, which is the one mode with no register in it
        // and is said by naming the base the encoding would otherwise use for no base at all.
        if addr.rip {
            out.push((reg << 3) | 0b101);
            holes.rip = Some(out.len());
            out.extend_from_slice(&addr.disp.to_le_bytes());
            return Ok(());
        }

        let index = match addr.index {
            Some(index) if index.number() == 4 => return Err(Error::Index),
            Some(index) => {
                if index.number() >= 8 {
                    self.rex |= REX_X;
                }
                Some(index.number() & 7)
            }
            None => None,
        };
        let scale = match addr.scale {
            _ if index.is_none() => 0,
            1 => 0,
            2 => 1,
            4 => 2,
            8 => 3,
            scale => return Err(Error::Scale { scale }),
        };
        let base = addr.base.map(|base| {
            if base.number() >= 8 {
                self.rex |= REX_B;
            }
            base.number() & 7
        });

        // The stack pointer's number in the addressed field means there is a second byte instead
        // of a register, so an address whose base really is the stack pointer needs that byte
        // even when it has no index. The frame pointer's number with no displacement means the
        // address is counted from the end of the instruction, so an address based on it always
        // carries a displacement, and a byte of zero is the cheapest one.
        let second = index.is_some() || base == Some(4) || base.is_none();
        let mode = match base {
            None => 0,
            Some(base) => {
                if addr.disp == 0 && base != 5 {
                    0
                } else if i8::try_from(addr.disp).is_ok() {
                    1
                } else {
                    2
                }
            }
        };
        out.push((mode << 6) | (reg << 3) | if second { 0b100 } else { base.unwrap_or(0) });
        if second {
            // Four in the index field is no index, and five in the base field with a mode of zero
            // is no base, which is how an address that is nothing but a number is written.
            out.push((scale << 6) | (index.unwrap_or(4) << 3) | base.unwrap_or(5));
        }
        match mode {
            0 if base.is_none() => out.extend_from_slice(&addr.disp.to_le_bytes()),
            0 => {}
            1 => out.push(addr.disp as u8),
            _ => out.extend_from_slice(&addr.disp.to_le_bytes()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x86_64::text::written;
    use crate::x86_64::{INSTS, R8, R12, R13, RAX, RBP, RCX, RDX, RSI, RSP};

    /// The bytes of that instruction, as a string a person can compare with a disassembler's.
    fn hex(mnemonic: &str, values: &[Value]) -> String {
        let mut out = Vec::new();
        encode(mnemonic, values, &mut out).expect("an instruction this target encodes");
        out.iter().map(|byte| format!("{byte:02x}")).collect::<Vec<_>>().join(" ")
    }

    /// A whole register, which is what most arguments are.
    fn quad(reg: PhysReg) -> Value {
        Value::Reg(reg, Width::Quad)
    }

    /// Thirty two bits of one.
    fn long(reg: PhysReg) -> Value {
        Value::Reg(reg, Width::Long)
    }

    /// Sixteen bits of one.
    fn word(reg: PhysReg) -> Value {
        Value::Reg(reg, Width::Word)
    }

    /// Eight bits of one.
    fn byte(reg: PhysReg) -> Value {
        Value::Reg(reg, Width::Byte)
    }

    #[test]
    fn every_instruction_the_listing_writes_is_one_this_encodes() {
        // The claim `spec/11-asm-objects-debug.md` section 11.1 makes about the two paths sharing
        // a description. An instruction the text path writes and this cannot encode would be an
        // opcode that compiles under `-S` and fails to produce an object file.
        for &(opcode, _) in INSTS {
            let insts = written(opcode).expect("every described opcode is a written opcode");
            for inst in insts {
                let args: Vec<Kind> = inst.args.iter().map(|&arg| Kind::of(arg)).collect();
                assert!(
                    encoding(inst.mnemonic, &args, 0).is_some(),
                    "{opcode} writes {} with {args:?} and nothing encodes it",
                    inst.mnemonic
                );
            }
        }
    }

    /// Numbers on both sides of every boundary any row here has.
    const PROBES: [i64; 11] =
        [0, 1, -1, 127, 128, -128, -129, 0xffff, 0x1_0000, 0x7fff_ffff, 0x1_0000_0000];

    #[test]
    fn the_rows_of_one_instruction_go_from_the_smallest_immediate_to_the_largest() {
        // A lookup takes the first row that fits, so the order is the whole of what makes the
        // short forms reachable. A general row in front of a short one would leave the short one
        // dead, which nothing that encodes a single instruction would ever notice, and a row that
        // held nothing the one in front of it did not would be dead outright.
        for (at, row) in ENCODINGS.iter().enumerate() {
            for other in &ENCODINGS[at + 1..] {
                if other.mnemonic != row.mnemonic || other.args != row.args {
                    continue;
                }
                for imm in PROBES {
                    assert!(
                        !row.fits.holds(imm) || other.fits.holds(imm),
                        "{} takes {imm} in front of a row that does not",
                        row.mnemonic
                    );
                }
                assert!(
                    PROBES.iter().any(|&imm| other.fits.holds(imm) && !row.fits.holds(imm)),
                    "{} has a row behind another that holds no more than it",
                    row.mnemonic
                );
            }
        }
    }

    #[test]
    fn an_immediate_no_form_of_an_instruction_can_hold_is_refused_rather_than_cut_down() {
        // The bug this is here for writes a program that adds a different number from the one it
        // was given, which nothing downstream could notice and no test of the text path could
        // either, since the text path writes the number out in full.
        let mut out = Vec::new();
        let big = 0x1_2345_6789;
        let error = encode("addq", &[Value::Imm(big), quad(RAX)], &mut out)
            .expect_err("more than four bytes of immediate");
        assert_eq!(error, Error::Immediate { mnemonic: "addq".to_owned(), imm: big });
        assert_eq!(out, Vec::<u8>::new());
        // The sixty four bit move is the one instruction that can hold it.
        assert!(encode("movq", &[Value::Imm(big), quad(RAX)], &mut out).is_ok());
        // And an immediate that fits either way round is one the machine can hold, since what it
        // carries is that many bits and not that many values.
        assert_eq!(hex("movl", &[Value::Imm(0xffff_ffff), long(RAX)]), "c7 c0 ff ff ff ff");
        assert_eq!(hex("addb", &[Value::Imm(200), byte(RAX)]), "80 c0 c8");
        assert_eq!(hex("shlq", &[Value::Imm(63), quad(RAX)]), "48 c1 e0 3f");
    }

    #[test]
    fn an_instruction_with_two_registers_is_the_opcode_and_one_byte_that_names_both() {
        // The direction that is easy to get backwards. AT&T writes the source first and the byte
        // that names the two puts the destination in the half the manual calls `r/m`.
        assert_eq!(hex("addl", &[long(RCX), long(RAX)]), "01 c8");
        assert_eq!(hex("addl", &[long(RAX), long(RCX)]), "01 c1");
        // Sixty four bits is the same instruction with a byte in front saying so.
        assert_eq!(hex("addq", &[quad(RCX), quad(RAX)]), "48 01 c8");
        // Sixteen is the same instruction with a different byte in front.
        let word = [Value::Reg(RCX, Width::Word), Value::Reg(RAX, Width::Word)];
        assert_eq!(hex("addw", &word), "66 01 c8");
        // And a multiply is the other way round, because it is not one of the eight that share
        // an opcode column.
        assert_eq!(hex("imull", &[long(RCX), long(RAX)]), "0f af c1");
    }

    #[test]
    fn a_register_the_second_half_of_the_machine_added_is_named_in_the_byte_in_front() {
        assert_eq!(hex("addl", &[long(R8), long(RAX)]), "44 01 c0");
        assert_eq!(hex("addl", &[long(RAX), long(R8)]), "41 01 c0");
        assert_eq!(hex("addq", &[quad(R8), quad(R8)]), "4d 01 c0");
        assert_eq!(hex("pushq", &[quad(R12)]), "41 54");
        assert_eq!(hex("popq", &[quad(RAX)]), "58");
    }

    /// The conditional move, whose two operands are the other way round from the text.
    ///
    /// AT&T writes the source first and the destination second, and the byte after the opcode
    /// names the destination in the register field and the source in the other one, which is the
    /// reverse of the arithmetic. Checked against what the assembler produces for the same three
    /// lines, which is the only way to be sure a direction is right.
    #[test]
    fn a_conditional_move_names_its_destination_in_the_register_field() {
        assert_eq!(hex("cmovnel", &[long(RSI), long(RAX)]), "0f 45 c6");
        assert_eq!(hex("cmovneq", &[quad(RSI), quad(RAX)]), "48 0f 45 c6");
        assert_eq!(hex("cmovnew", &[word(RSI), word(RAX)]), "66 0f 45 c6");
        // And the half of the register file that needs a byte in front to be named at all.
        assert_eq!(hex("cmovnel", &[long(R8), long(RAX)]), "41 0f 45 c0");
        assert_eq!(hex("cmovnel", &[long(RAX), long(R8)]), "44 0f 45 c0");
    }

    #[test]
    fn a_byte_register_the_machine_could_not_reach_before_forces_a_byte_that_says_nothing_else() {
        // Without the `40` these are `%dh` and `%bh`, which is the encoding bug that produces a
        // program reading a register nothing was ever put in.
        assert_eq!(hex("movb", &[byte(RSI), byte(RAX)]), "40 88 f0");
        assert_eq!(hex("sete", &[byte(RSI)]), "40 0f 94 c6");
        assert_eq!(hex("sete", &[byte(RAX)]), "0f 94 c0");
        // And the one instruction that names the high byte, which may have no such byte at all.
        assert_eq!(hex("movb", &[Value::High(RAX), byte(RDX)]), "88 e2");
        let mut out = Vec::new();
        let error = encode("movb", &[Value::High(RAX), byte(RSI)], &mut out)
            .expect_err("ah and sil in one instruction");
        assert_eq!(error, Error::Crowded { mnemonic: "movb".to_owned() });
    }

    #[test]
    fn an_immediate_is_written_in_as_few_bytes_as_it_fits_in() {
        assert_eq!(hex("addl", &[Value::Imm(1), long(RCX)]), "83 c1 01");
        assert_eq!(hex("addl", &[Value::Imm(-1), long(RCX)]), "83 c1 ff");
        assert_eq!(hex("addl", &[Value::Imm(1000), long(RCX)]), "81 c1 e8 03 00 00");
        assert_eq!(hex("addq", &[Value::Imm(8), quad(RSP)]), "48 83 c4 08");
        // The move is the one instruction with an eight byte immediate, and it is ten bytes long
        // when it needs one and seven when it does not.
        assert_eq!(hex("movq", &[Value::Imm(1), quad(RAX)]), "48 c7 c0 01 00 00 00");
        assert_eq!(
            hex("movq", &[Value::Imm(0x1_2345_6789), quad(RAX)]),
            "48 b8 89 67 45 23 01 00 00 00"
        );
        // A thirty two bit move of a constant is never the ten byte form, because there is no
        // thirty two bit register that could hold a number too big for four bytes.
        assert_eq!(hex("movl", &[Value::Imm(1), long(RAX)]), "c7 c0 01 00 00 00");
    }

    #[test]
    fn an_address_is_the_registers_it_names_and_whatever_is_added_to_them() {
        // A base on its own, which is the shortest.
        let base = Addr { base: Some(RCX), ..Addr::default() };
        assert_eq!(hex("movq", &[Value::Mem(base), quad(RAX)]), "48 8b 01");
        // A base and a displacement, in one byte where it fits and four where it does not.
        let near = Addr { base: Some(RCX), disp: -16, ..Addr::default() };
        assert_eq!(hex("movq", &[Value::Mem(near), quad(RAX)]), "48 8b 41 f0");
        let far = Addr { base: Some(RCX), disp: 1000, ..Addr::default() };
        assert_eq!(hex("movq", &[Value::Mem(far), quad(RAX)]), "48 8b 81 e8 03 00 00");
        // A base, an index and a scale, which needs the second byte.
        let indexed = Addr { base: Some(RCX), index: Some(RDX), scale: 4, disp: -16, rip: false };
        assert_eq!(hex("leaq", &[Value::Mem(indexed), quad(RAX)]), "48 8d 44 91 f0");
        // A store is the same address with the two ends the other way round.
        assert_eq!(hex("movl", &[long(RAX), Value::Mem(near)]), "89 41 f0");
    }

    #[test]
    fn the_two_registers_an_address_cannot_be_written_with_plainly_are_written_around() {
        // The stack pointer's number means there is a second byte rather than a register, so an
        // address really based on it needs that byte even with nothing to put in it.
        let stack = Addr { base: Some(RSP), disp: 8, ..Addr::default() };
        assert_eq!(hex("movq", &[Value::Mem(stack), quad(RAX)]), "48 8b 44 24 08");
        // And the frame pointer's number with no displacement means the address is counted from
        // the end of the instruction, so one based on it always carries one.
        let frame = Addr { base: Some(RBP), ..Addr::default() };
        assert_eq!(hex("movq", &[Value::Mem(frame), quad(RAX)]), "48 8b 45 00");
        // The same two facts hold of the registers whose low three bits are theirs.
        let twelve = Addr { base: Some(R12), disp: 8, ..Addr::default() };
        assert_eq!(hex("movq", &[Value::Mem(twelve), quad(RAX)]), "49 8b 44 24 08");
        let thirteen = Addr { base: Some(R13), ..Addr::default() };
        assert_eq!(hex("movq", &[Value::Mem(thirteen), quad(RAX)]), "49 8b 45 00");
        // The stack pointer is the one register that cannot be an index at all.
        let mut out = Vec::new();
        let bad = Addr { base: Some(RCX), index: Some(RSP), scale: 1, disp: 0, rip: false };
        let error = encode("leaq", &[Value::Mem(bad), quad(RAX)], &mut out)
            .expect_err("the stack pointer as an index");
        assert_eq!(error, Error::Index);
    }

    /// The two x87 instructions, whose bytes are checked against what the assembler writes for the
    /// same lines rather than against the manual, the way every other group here is.
    ///
    /// They are the only instructions on this machine with one argument that is an address and no
    /// register argument at all, so the addressing byte carries the extension where every other
    /// memory instruction carries a register, and the two of them differ in nothing else.
    #[test]
    fn the_x87_load_and_store_are_one_opcode_with_two_extensions() {
        let base = Addr { base: Some(RCX), ..Addr::default() };
        assert_eq!(hex("fldt", &[Value::Mem(base)]), "db 29");
        assert_eq!(hex("fstpt", &[Value::Mem(base)]), "db 39");
        // A displacement, which is where a `long double` in a frame really is.
        let near = Addr { base: Some(RCX), disp: -16, ..Addr::default() };
        assert_eq!(hex("fldt", &[Value::Mem(near)]), "db 69 f0");
        let stack = Addr { base: Some(RSP), disp: 8, ..Addr::default() };
        assert_eq!(hex("fstpt", &[Value::Mem(stack)]), "db 7c 24 08");
        // The two registers an address is written around, and the one that needs a REX byte, which
        // is the only byte an x87 instruction has that says anything about a register at all.
        let frame = Addr { base: Some(RBP), ..Addr::default() };
        assert_eq!(hex("fldt", &[Value::Mem(frame)]), "db 6d 00");
        let thirteen = Addr { base: Some(R13), disp: -16, ..Addr::default() };
        assert_eq!(hex("fstpt", &[Value::Mem(thirteen)]), "41 db 7d f0");
        let indexed = Addr { base: Some(RCX), index: Some(RDX), scale: 4, disp: 0, rip: false };
        assert_eq!(hex("fldt", &[Value::Mem(indexed)]), "db 2c 91");
    }

    /// The conversions, whose bytes are the part of this corner of the machine worth checking
    /// against the assembler rather than reading off a page.
    ///
    /// The width of the operand is in the opcode byte rather than in a prefix, which is the
    /// opposite of every other group here, and load and store are two extensions of the same
    /// byte. So a mistake in one of these is a mistake that encodes to a real instruction doing
    /// something else at another width, which is exactly the mistake nothing downstream catches.
    #[test]
    fn the_x87_conversions_put_the_width_in_the_opcode_and_the_direction_in_the_extension() {
        let base = Addr { base: Some(RCX), ..Addr::default() };
        let at = |mnemonic| hex(mnemonic, &[Value::Mem(base)]);
        // Up: four bytes of float, eight bytes of float, four of integer, eight of integer.
        assert_eq!(at("flds"), "d9 01");
        assert_eq!(at("fldl"), "dd 01");
        assert_eq!(at("fildl"), "db 01");
        assert_eq!(at("fildll"), "df 29");
        // Down: the same four opcodes with the extension that stores and pops.
        assert_eq!(at("fstps"), "d9 19");
        assert_eq!(at("fstpl"), "dd 19");
        assert_eq!(at("fistpl"), "db 19");
        assert_eq!(at("fistpll"), "df 39");
        // The control word, which is two more extensions of the byte the four byte float uses.
        assert_eq!(at("fnstcw"), "d9 39");
        assert_eq!(at("fldcw"), "d9 29");
        // And an address that is not the shortest one, since a frame is where all of these really
        // point and nothing in a frame is at the address a register holds.
        let frame = Addr { base: Some(RBP), disp: -16, ..Addr::default() };
        assert_eq!(hex("fildl", &[Value::Mem(frame)]), "db 45 f0");
        let stack = Addr { base: Some(RSP), disp: 8, ..Addr::default() };
        assert_eq!(hex("fistpll", &[Value::Mem(stack)]), "df 7c 24 08");
    }

    #[test]
    fn an_address_counted_from_the_end_of_the_instruction_leaves_its_displacement_open() {
        let global = Addr { rip: true, ..Addr::default() };
        let mut out = vec![0xcc];
        let holes = encode("movq", &[Value::Mem(global), quad(RAX)], &mut out).expect("a global");
        assert_eq!(out, [0xcc, 0x48, 0x8b, 0x05, 0, 0, 0, 0]);
        // Where the four bytes are, counted in the buffer rather than in the instruction, because
        // what the caller does with it is record a relocation against the buffer.
        assert_eq!(holes.rip, Some(4));
        assert_eq!(holes.dest, None);
    }

    #[test]
    fn a_jump_leaves_the_distance_to_where_it_goes_open() {
        let mut out = Vec::new();
        let holes = encode("jmp", &[Value::Dest], &mut out).expect("a jump");
        assert_eq!(out, [0xe9, 0, 0, 0, 0]);
        assert_eq!(holes.dest, Some(1));
        out.clear();
        let holes = encode("je", &[Value::Dest], &mut out).expect("a conditional jump");
        assert_eq!(out, [0x0f, 0x84, 0, 0, 0, 0]);
        assert_eq!(holes.dest, Some(2));
    }

    #[test]
    fn an_instruction_with_no_arguments_is_the_opcode_and_whatever_says_how_wide_it_is() {
        assert_eq!(hex("ret", &[]), "c3");
        assert_eq!(hex("cltd", &[]), "99");
        assert_eq!(hex("cqto", &[]), "48 99");
        assert_eq!(hex("cwtd", &[]), "66 99");
        assert_eq!(hex("cbtw", &[]), "66 98");
    }

    #[test]
    fn a_shift_by_a_count_does_not_encode_the_count_because_the_machine_knows_where_it_is() {
        // Two arguments written and one encoded, which is the case that says why an argument
        // names an operand rather than being one.
        assert_eq!(hex("shlq", &[byte(RCX), quad(RAX)]), "48 d3 e0");
        assert_eq!(hex("sarl", &[byte(RCX), long(RCX)]), "d3 f9");
        assert_eq!(hex("shll", &[Value::Imm(3), long(RAX)]), "c1 e0 03");
    }

    #[test]
    fn a_division_is_the_widening_and_then_the_instruction_that_names_only_its_divisor() {
        assert_eq!(hex("idivl", &[long(RCX)]), "f7 f9");
        assert_eq!(hex("idivq", &[quad(RSI)]), "48 f7 fe");
        assert_eq!(hex("divl", &[long(RCX)]), "f7 f1");
        assert_eq!(hex("negl", &[long(RAX)]), "f7 d8");
        assert_eq!(hex("notq", &[quad(RAX)]), "48 f7 d0");
    }

    #[test]
    fn a_conversion_puts_its_destination_where_the_arithmetic_puts_its_source() {
        assert_eq!(hex("movzbl", &[byte(RAX), long(RCX)]), "0f b6 c8");
        assert_eq!(hex("movsbq", &[byte(RAX), quad(RCX)]), "48 0f be c8");
        assert_eq!(hex("movslq", &[long(RSI), quad(RAX)]), "48 63 c6");
        assert_eq!(hex("movzwl", &[Value::Reg(RSI, Width::Word), long(RAX)]), "0f b7 c6");
    }

    #[test]
    fn a_mnemonic_with_arguments_it_does_not_take_is_refused_rather_than_encoded() {
        let mut out = Vec::new();
        let error = encode("ret", &[quad(RAX)], &mut out).expect_err("a return of a register");
        assert_eq!(error, Error::Unwritten { mnemonic: "ret".to_owned(), args: vec![Kind::Reg] });
        assert_eq!(out, Vec::<u8>::new(), "nothing is written for an instruction that is refused");
        let error = encode("frobnicate", &[], &mut out).expect_err("no such instruction");
        assert!(matches!(error, Error::Unwritten { .. }), "{error}");
        assert_eq!(encoding("addl", &[Kind::Reg], 0), None);
    }
}
