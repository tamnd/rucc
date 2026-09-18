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

use crate::regs::{PhysReg, Segment};
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
    /// A position on the x87 stack, which is a depth rather than a register.
    ///
    /// Its own kind for the reason [`Kind::Vec`] is: it is what picks a row, and a row it picked
    /// wrongly would be a different instruction. Nothing is written from it, since every row that
    /// takes one is a fixed opcode with the depth already in its second byte. What it is for is
    /// the same thing every kind here is for: the x87 mnemonics that take a stack position also
    /// have forms that take an address, and a lookup that could not tell the two apart would
    /// encode one as the other.
    Stack,
}

impl Kind {
    /// What kind of argument that is.
    #[must_use]
    pub fn of(arg: Arg) -> Self {
        match arg {
            Arg::Reg(_, _) | Arg::Named(_) | Arg::Through => Kind::Reg,
            Arg::Xmm(_) => Kind::Vec,
            Arg::Stack(_) => Kind::Stack,
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
    /// One byte of the same thing, which reaches a hundred and twenty seven bytes either way and
    /// is the only distance the one instruction that carries it has. Nothing this compiler emits
    /// uses it, because choosing the short form of a jump that also has a long one is a pass over
    /// a whole section rather than an encoding question, and a whole section is not what an
    /// encoder is given.
    Cb,
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
static RRR: [Kind; 3] = [Kind::Reg, Kind::Reg, Kind::Reg];
static MR: [Kind; 2] = [Kind::Mem, Kind::Reg];
static IM: [Kind; 2] = [Kind::Imm, Kind::Mem];
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
// The x87 stack positions, which are one argument or two and are never anything else. There is no
// row here mixing one with a register or with an address, because no instruction on this machine
// names a stack position and a register in the same breath.
static S: [Kind; 1] = [Kind::Stack];
static SS: [Kind; 2] = [Kind::Stack, Kind::Stack];

/// Every instruction [`crate::x86_64::written`] can name, and the bytes it comes out as.
///
/// In the order the opcodes that reach them are described in, and grouped the same way, so that
/// a reader with the manual open can go down all three tables together. An instruction reached
/// from more than one opcode is written once, where it is first reached, which is why the
/// conversions hold the widening a division needs and the arithmetic holds the clearing.
///
/// There are rows here the listing never names, and they are not a mistake. The assembler reads
/// files this compiler did not write, and what those files contain is what the person who wrote
/// them meant rather than what a C expression compiles to: a carry carried from one instruction to
/// the next, a rotate, a bit test, a count of a loop that leaves the flags alone. A row with no
/// opcode behind it in the listing costs a lookup nothing, and the alternative is refusing a file
/// gas assembles, so the claim the tests hold is one way round: everything the listing names is
/// here, and not everything here is named by the listing.
///
/// A row for a small immediate comes in front of the general row for the same instruction,
/// because a lookup takes the first row that fits and the narrower one is the one wanted.
static ENCODINGS: &[Encoding] = &[
    // Constants. The three narrow ones put the destination in the opcode rather than in an
    // addressing byte, which is one byte shorter and is why `B0` and `B8` are here instead of
    // `C6 /0` and `C7 /0`. The addressed forms reach memory as well and these do not, and no move
    // here writes an immediate to memory, so the shorter row is the only row each of them needs.
    // The sixty four bit move is the one that keeps the addressing byte: its short form carries
    // the whole eight bytes and the addressed one sign extends four, so seven beats ten whenever
    // the number fits, which is nearly always.
    takes("movb", &IR, Fits::Byte, Byte, &[0xB0], plus(1), ImmSize::Ib),
    takes("movw", &IR, Fits::Word, Word, &[0xB8], plus(1), ImmSize::Iw),
    takes("movl", &IR, Fits::Long, Long, &[0xB8], plus(1), ImmSize::Id),
    takes("movq", &IR, Signed32, Quad, &[0xC7], ext(1, 0), ImmSize::Id),
    bytes("movq", &IR, Quad, &[0xB8], plus(1), ImmSize::Io),
    // The name a file gives the long form when it wants that form whatever the number is. It is the
    // row above and nothing else, and it is a row of its own rather than a spelling because a
    // lookup here is by mnemonic and a program that writes this has asked for ten bytes.
    bytes("movabsq", &IR, Quad, &[0xB8], plus(1), ImmSize::Io),
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
    // The two of the eight that carry the flag from the instruction before them, which nothing this
    // compiler generates uses and every hand written addition of a number wider than a register is
    // made of. They sit in the same column as the rest, two opcodes above the addition and two
    // above the subtraction, and read exactly the same way round.
    bytes("adcb", &RR, Byte, &[0x10], pair(1, 0), NO_IMM),
    bytes("adcw", &RR, Word, &[0x11], pair(1, 0), NO_IMM),
    bytes("adcl", &RR, Long, &[0x11], pair(1, 0), NO_IMM),
    bytes("adcq", &RR, Quad, &[0x11], pair(1, 0), NO_IMM),
    bytes("sbbb", &RR, Byte, &[0x18], pair(1, 0), NO_IMM),
    bytes("sbbw", &RR, Word, &[0x19], pair(1, 0), NO_IMM),
    bytes("sbbl", &RR, Long, &[0x19], pair(1, 0), NO_IMM),
    bytes("sbbq", &RR, Quad, &[0x19], pair(1, 0), NO_IMM),
    // The multiply is the other way round from the rest of them: it is not one of the eight that
    // share an opcode column, and the register beside the addressing byte is its destination.
    bytes("imulw", &RR, Word, &[0x0F, 0xAF], pair(0, 1), NO_IMM),
    bytes("imull", &RR, Long, &[0x0F, 0xAF], pair(0, 1), NO_IMM),
    bytes("imulq", &RR, Quad, &[0x0F, 0xAF], pair(0, 1), NO_IMM),
    // The same arithmetic with the source in memory, which for the eight that share a column is
    // the opcode two above the register form: the column holds the direction, so `01` writes the
    // register into what the addressing byte names and `03` reads it out. The addressing byte
    // therefore names the memory and the register beside it is the destination, which is the
    // multiply's arrangement rather than the addition's and is why every row here reads the same
    // way round.
    bytes("addb", &MR, Byte, &[0x02], pair(0, 1), NO_IMM),
    bytes("addw", &MR, Word, &[0x03], pair(0, 1), NO_IMM),
    bytes("addl", &MR, Long, &[0x03], pair(0, 1), NO_IMM),
    bytes("addq", &MR, Quad, &[0x03], pair(0, 1), NO_IMM),
    bytes("subb", &MR, Byte, &[0x2A], pair(0, 1), NO_IMM),
    bytes("subw", &MR, Word, &[0x2B], pair(0, 1), NO_IMM),
    bytes("subl", &MR, Long, &[0x2B], pair(0, 1), NO_IMM),
    bytes("subq", &MR, Quad, &[0x2B], pair(0, 1), NO_IMM),
    bytes("andb", &MR, Byte, &[0x22], pair(0, 1), NO_IMM),
    bytes("andw", &MR, Word, &[0x23], pair(0, 1), NO_IMM),
    bytes("andl", &MR, Long, &[0x23], pair(0, 1), NO_IMM),
    bytes("andq", &MR, Quad, &[0x23], pair(0, 1), NO_IMM),
    bytes("orb", &MR, Byte, &[0x0A], pair(0, 1), NO_IMM),
    bytes("orw", &MR, Word, &[0x0B], pair(0, 1), NO_IMM),
    bytes("orl", &MR, Long, &[0x0B], pair(0, 1), NO_IMM),
    bytes("orq", &MR, Quad, &[0x0B], pair(0, 1), NO_IMM),
    bytes("xorb", &MR, Byte, &[0x32], pair(0, 1), NO_IMM),
    bytes("xorw", &MR, Word, &[0x33], pair(0, 1), NO_IMM),
    bytes("xorl", &MR, Long, &[0x33], pair(0, 1), NO_IMM),
    bytes("xorq", &MR, Quad, &[0x33], pair(0, 1), NO_IMM),
    bytes("adcb", &MR, Byte, &[0x12], pair(0, 1), NO_IMM),
    bytes("adcw", &MR, Word, &[0x13], pair(0, 1), NO_IMM),
    bytes("adcl", &MR, Long, &[0x13], pair(0, 1), NO_IMM),
    bytes("adcq", &MR, Quad, &[0x13], pair(0, 1), NO_IMM),
    bytes("sbbb", &MR, Byte, &[0x1A], pair(0, 1), NO_IMM),
    bytes("sbbw", &MR, Word, &[0x1B], pair(0, 1), NO_IMM),
    bytes("sbbl", &MR, Long, &[0x1B], pair(0, 1), NO_IMM),
    bytes("sbbq", &MR, Quad, &[0x1B], pair(0, 1), NO_IMM),
    bytes("imulw", &MR, Word, &[0x0F, 0xAF], pair(0, 1), NO_IMM),
    bytes("imull", &MR, Long, &[0x0F, 0xAF], pair(0, 1), NO_IMM),
    bytes("imulq", &MR, Quad, &[0x0F, 0xAF], pair(0, 1), NO_IMM),
    bytes("cmpb", &MR, Byte, &[0x3A], pair(0, 1), NO_IMM),
    bytes("cmpw", &MR, Word, &[0x3B], pair(0, 1), NO_IMM),
    bytes("cmpl", &MR, Long, &[0x3B], pair(0, 1), NO_IMM),
    bytes("cmpq", &MR, Quad, &[0x3B], pair(0, 1), NO_IMM),
    // The same arithmetic the other way about, a register read and an address written. This is the
    // opcode the register form already uses, since the register form is one register addressing
    // another and the only thing that changes is what the addressing byte names. So `01` is `addl`
    // between two registers above and `addl` into memory here, and the difference is entirely in
    // the byte behind it.
    //
    // Nothing this compiler emits is shaped this way. A read and modify and write of a variable in
    // memory goes through a register in the middle, because the value is wanted afterwards as often
    // as not and because a register is where the selector puts everything. A file written by hand
    // has no such habit and adds straight into the array it is walking, which is what GMP does in
    // every one of its loops.
    bytes("addb", &RM, Byte, &[0x00], pair(1, 0), NO_IMM),
    bytes("addw", &RM, Word, &[0x01], pair(1, 0), NO_IMM),
    bytes("addl", &RM, Long, &[0x01], pair(1, 0), NO_IMM),
    bytes("addq", &RM, Quad, &[0x01], pair(1, 0), NO_IMM),
    bytes("orb", &RM, Byte, &[0x08], pair(1, 0), NO_IMM),
    bytes("orw", &RM, Word, &[0x09], pair(1, 0), NO_IMM),
    bytes("orl", &RM, Long, &[0x09], pair(1, 0), NO_IMM),
    bytes("orq", &RM, Quad, &[0x09], pair(1, 0), NO_IMM),
    bytes("adcb", &RM, Byte, &[0x10], pair(1, 0), NO_IMM),
    bytes("adcw", &RM, Word, &[0x11], pair(1, 0), NO_IMM),
    bytes("adcl", &RM, Long, &[0x11], pair(1, 0), NO_IMM),
    bytes("adcq", &RM, Quad, &[0x11], pair(1, 0), NO_IMM),
    bytes("sbbb", &RM, Byte, &[0x18], pair(1, 0), NO_IMM),
    bytes("sbbw", &RM, Word, &[0x19], pair(1, 0), NO_IMM),
    bytes("sbbl", &RM, Long, &[0x19], pair(1, 0), NO_IMM),
    bytes("sbbq", &RM, Quad, &[0x19], pair(1, 0), NO_IMM),
    bytes("andb", &RM, Byte, &[0x20], pair(1, 0), NO_IMM),
    bytes("andw", &RM, Word, &[0x21], pair(1, 0), NO_IMM),
    bytes("andl", &RM, Long, &[0x21], pair(1, 0), NO_IMM),
    bytes("andq", &RM, Quad, &[0x21], pair(1, 0), NO_IMM),
    bytes("subb", &RM, Byte, &[0x28], pair(1, 0), NO_IMM),
    bytes("subw", &RM, Word, &[0x29], pair(1, 0), NO_IMM),
    bytes("subl", &RM, Long, &[0x29], pair(1, 0), NO_IMM),
    bytes("subq", &RM, Quad, &[0x29], pair(1, 0), NO_IMM),
    bytes("xorb", &RM, Byte, &[0x30], pair(1, 0), NO_IMM),
    bytes("xorw", &RM, Word, &[0x31], pair(1, 0), NO_IMM),
    bytes("xorl", &RM, Long, &[0x31], pair(1, 0), NO_IMM),
    bytes("xorq", &RM, Quad, &[0x31], pair(1, 0), NO_IMM),
    bytes("cmpb", &RM, Byte, &[0x38], pair(1, 0), NO_IMM),
    bytes("cmpw", &RM, Word, &[0x39], pair(1, 0), NO_IMM),
    bytes("cmpl", &RM, Long, &[0x39], pair(1, 0), NO_IMM),
    bytes("cmpq", &RM, Quad, &[0x39], pair(1, 0), NO_IMM),
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
    takes("adcb", &IR, Fits::Byte, Byte, &[0x80], ext(1, 2), ImmSize::Ib),
    takes("adcw", &IR, Signed8, Word, &[0x83], ext(1, 2), ImmSize::Ib),
    takes("adcw", &IR, Fits::Word, Word, &[0x81], ext(1, 2), ImmSize::Iw),
    takes("adcl", &IR, Signed8, Long, &[0x83], ext(1, 2), ImmSize::Ib),
    takes("adcl", &IR, Fits::Long, Long, &[0x81], ext(1, 2), ImmSize::Id),
    takes("adcq", &IR, Signed8, Quad, &[0x83], ext(1, 2), ImmSize::Ib),
    takes("adcq", &IR, Signed32, Quad, &[0x81], ext(1, 2), ImmSize::Id),
    takes("sbbb", &IR, Fits::Byte, Byte, &[0x80], ext(1, 3), ImmSize::Ib),
    takes("sbbw", &IR, Signed8, Word, &[0x83], ext(1, 3), ImmSize::Ib),
    takes("sbbw", &IR, Fits::Word, Word, &[0x81], ext(1, 3), ImmSize::Iw),
    takes("sbbl", &IR, Signed8, Long, &[0x83], ext(1, 3), ImmSize::Ib),
    takes("sbbl", &IR, Fits::Long, Long, &[0x81], ext(1, 3), ImmSize::Id),
    takes("sbbq", &IR, Signed8, Quad, &[0x83], ext(1, 3), ImmSize::Ib),
    takes("sbbq", &IR, Signed32, Quad, &[0x81], ext(1, 3), ImmSize::Id),
    takes("cmpb", &IR, Fits::Byte, Byte, &[0x80], ext(1, 7), ImmSize::Ib),
    takes("cmpw", &IR, Signed8, Word, &[0x83], ext(1, 7), ImmSize::Ib),
    takes("cmpw", &IR, Fits::Word, Word, &[0x81], ext(1, 7), ImmSize::Iw),
    takes("cmpl", &IR, Signed8, Long, &[0x83], ext(1, 7), ImmSize::Ib),
    takes("cmpl", &IR, Fits::Long, Long, &[0x81], ext(1, 7), ImmSize::Id),
    takes("cmpq", &IR, Signed8, Quad, &[0x83], ext(1, 7), ImmSize::Ib),
    takes("cmpq", &IR, Signed32, Quad, &[0x81], ext(1, 7), ImmSize::Id),
    // The same eight writing an immediate to an address, which is the rows above with an address
    // where the register was and is why they are grouped by which digit names them rather than by
    // which instruction they are. The one thing this compiler writes here is the inclusive or of
    // zero with a byte that a probing prologue touches a page with, see `crate::frame::Probe`, and
    // the rest are for a file that adds a constant to a counter in memory without loading it first.
    takes("addb", &IM, Fits::Byte, Byte, &[0x80], ext(1, 0), ImmSize::Ib),
    takes("addw", &IM, Signed8, Word, &[0x83], ext(1, 0), ImmSize::Ib),
    takes("addw", &IM, Fits::Word, Word, &[0x81], ext(1, 0), ImmSize::Iw),
    takes("addl", &IM, Signed8, Long, &[0x83], ext(1, 0), ImmSize::Ib),
    takes("addl", &IM, Fits::Long, Long, &[0x81], ext(1, 0), ImmSize::Id),
    takes("addq", &IM, Signed8, Quad, &[0x83], ext(1, 0), ImmSize::Ib),
    takes("addq", &IM, Signed32, Quad, &[0x81], ext(1, 0), ImmSize::Id),
    takes("orb", &IM, Fits::Byte, Byte, &[0x80], ext(1, 1), ImmSize::Ib),
    takes("orw", &IM, Signed8, Word, &[0x83], ext(1, 1), ImmSize::Ib),
    takes("orw", &IM, Fits::Word, Word, &[0x81], ext(1, 1), ImmSize::Iw),
    takes("orl", &IM, Signed8, Long, &[0x83], ext(1, 1), ImmSize::Ib),
    takes("orl", &IM, Fits::Long, Long, &[0x81], ext(1, 1), ImmSize::Id),
    takes("orq", &IM, Signed8, Quad, &[0x83], ext(1, 1), ImmSize::Ib),
    takes("orq", &IM, Signed32, Quad, &[0x81], ext(1, 1), ImmSize::Id),
    takes("adcb", &IM, Fits::Byte, Byte, &[0x80], ext(1, 2), ImmSize::Ib),
    takes("adcw", &IM, Signed8, Word, &[0x83], ext(1, 2), ImmSize::Ib),
    takes("adcw", &IM, Fits::Word, Word, &[0x81], ext(1, 2), ImmSize::Iw),
    takes("adcl", &IM, Signed8, Long, &[0x83], ext(1, 2), ImmSize::Ib),
    takes("adcl", &IM, Fits::Long, Long, &[0x81], ext(1, 2), ImmSize::Id),
    takes("adcq", &IM, Signed8, Quad, &[0x83], ext(1, 2), ImmSize::Ib),
    takes("adcq", &IM, Signed32, Quad, &[0x81], ext(1, 2), ImmSize::Id),
    takes("sbbb", &IM, Fits::Byte, Byte, &[0x80], ext(1, 3), ImmSize::Ib),
    takes("sbbw", &IM, Signed8, Word, &[0x83], ext(1, 3), ImmSize::Ib),
    takes("sbbw", &IM, Fits::Word, Word, &[0x81], ext(1, 3), ImmSize::Iw),
    takes("sbbl", &IM, Signed8, Long, &[0x83], ext(1, 3), ImmSize::Ib),
    takes("sbbl", &IM, Fits::Long, Long, &[0x81], ext(1, 3), ImmSize::Id),
    takes("sbbq", &IM, Signed8, Quad, &[0x83], ext(1, 3), ImmSize::Ib),
    takes("sbbq", &IM, Signed32, Quad, &[0x81], ext(1, 3), ImmSize::Id),
    takes("andb", &IM, Fits::Byte, Byte, &[0x80], ext(1, 4), ImmSize::Ib),
    takes("andw", &IM, Signed8, Word, &[0x83], ext(1, 4), ImmSize::Ib),
    takes("andw", &IM, Fits::Word, Word, &[0x81], ext(1, 4), ImmSize::Iw),
    takes("andl", &IM, Signed8, Long, &[0x83], ext(1, 4), ImmSize::Ib),
    takes("andl", &IM, Fits::Long, Long, &[0x81], ext(1, 4), ImmSize::Id),
    takes("andq", &IM, Signed8, Quad, &[0x83], ext(1, 4), ImmSize::Ib),
    takes("andq", &IM, Signed32, Quad, &[0x81], ext(1, 4), ImmSize::Id),
    takes("subb", &IM, Fits::Byte, Byte, &[0x80], ext(1, 5), ImmSize::Ib),
    takes("subw", &IM, Signed8, Word, &[0x83], ext(1, 5), ImmSize::Ib),
    takes("subw", &IM, Fits::Word, Word, &[0x81], ext(1, 5), ImmSize::Iw),
    takes("subl", &IM, Signed8, Long, &[0x83], ext(1, 5), ImmSize::Ib),
    takes("subl", &IM, Fits::Long, Long, &[0x81], ext(1, 5), ImmSize::Id),
    takes("subq", &IM, Signed8, Quad, &[0x83], ext(1, 5), ImmSize::Ib),
    takes("subq", &IM, Signed32, Quad, &[0x81], ext(1, 5), ImmSize::Id),
    takes("xorb", &IM, Fits::Byte, Byte, &[0x80], ext(1, 6), ImmSize::Ib),
    takes("xorw", &IM, Signed8, Word, &[0x83], ext(1, 6), ImmSize::Ib),
    takes("xorw", &IM, Fits::Word, Word, &[0x81], ext(1, 6), ImmSize::Iw),
    takes("xorl", &IM, Signed8, Long, &[0x83], ext(1, 6), ImmSize::Ib),
    takes("xorl", &IM, Fits::Long, Long, &[0x81], ext(1, 6), ImmSize::Id),
    takes("xorq", &IM, Signed8, Quad, &[0x83], ext(1, 6), ImmSize::Ib),
    takes("xorq", &IM, Signed32, Quad, &[0x81], ext(1, 6), ImmSize::Id),
    takes("cmpb", &IM, Fits::Byte, Byte, &[0x80], ext(1, 7), ImmSize::Ib),
    takes("cmpw", &IM, Signed8, Word, &[0x83], ext(1, 7), ImmSize::Ib),
    takes("cmpw", &IM, Fits::Word, Word, &[0x81], ext(1, 7), ImmSize::Iw),
    takes("cmpl", &IM, Signed8, Long, &[0x83], ext(1, 7), ImmSize::Ib),
    takes("cmpl", &IM, Fits::Long, Long, &[0x81], ext(1, 7), ImmSize::Id),
    takes("cmpq", &IM, Signed8, Quad, &[0x83], ext(1, 7), ImmSize::Ib),
    takes("cmpq", &IM, Signed32, Quad, &[0x81], ext(1, 7), ImmSize::Id),
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
    // Adding one and taking one away, which are two of the eight that share `0xFF` and are not what
    // this compiler writes for `++`: an addition of one sets the carry flag and these leave it
    // alone, so the two are different instructions and the addition is the one a C program means.
    // Hand written assembly counts loops with these precisely because they leave the flag alone,
    // which is the whole reason a carry can be carried across an iteration at all.
    bytes("incb", &R, Byte, &[0xFE], ext(0, 0), NO_IMM),
    bytes("incw", &R, Word, &[0xFF], ext(0, 0), NO_IMM),
    bytes("incl", &R, Long, &[0xFF], ext(0, 0), NO_IMM),
    bytes("incq", &R, Quad, &[0xFF], ext(0, 0), NO_IMM),
    bytes("decb", &R, Byte, &[0xFE], ext(0, 1), NO_IMM),
    bytes("decw", &R, Word, &[0xFF], ext(0, 1), NO_IMM),
    bytes("decl", &R, Long, &[0xFF], ext(0, 1), NO_IMM),
    bytes("decq", &R, Quad, &[0xFF], ext(0, 1), NO_IMM),
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
    // The multiply that writes both halves of its answer, which is the fifth of the eight and is
    // not `imul` above: this one is unsigned and puts the top half in a second register the
    // instruction does not name, which is what a program doing arithmetic wider than a register
    // wants and what nothing a C expression compiles to ever needs.
    bytes("mulb", &R, Byte, &[0xF6], ext(0, 4), NO_IMM),
    bytes("mulw", &R, Word, &[0xF7], ext(0, 4), NO_IMM),
    bytes("mull", &R, Long, &[0xF7], ext(0, 4), NO_IMM),
    bytes("mulq", &R, Quad, &[0xF7], ext(0, 4), NO_IMM),
    // The signed one beside it, which is the same instruction in the same column and is a different
    // row from the two operand `imul` above because it takes one operand and writes two registers.
    bytes("imulb", &R, Byte, &[0xF6], ext(0, 5), NO_IMM),
    bytes("imulw", &R, Word, &[0xF7], ext(0, 5), NO_IMM),
    bytes("imull", &R, Long, &[0xF7], ext(0, 5), NO_IMM),
    bytes("imulq", &R, Quad, &[0xF7], ext(0, 5), NO_IMM),
    // All eight of the group naming an address instead of a register, which is the same opcode and
    // the same digit with the addressing byte pointing somewhere else. The compiler loads into a
    // register first every time, so none of these had a row. A file written by hand multiplies
    // straight out of the array it is walking, which is what `mulq 16(%r8)` in GMP's division is.
    bytes("negb", &M, Byte, &[0xF6], ext(0, 3), NO_IMM),
    bytes("negw", &M, Word, &[0xF7], ext(0, 3), NO_IMM),
    bytes("negl", &M, Long, &[0xF7], ext(0, 3), NO_IMM),
    bytes("negq", &M, Quad, &[0xF7], ext(0, 3), NO_IMM),
    bytes("notb", &M, Byte, &[0xF6], ext(0, 2), NO_IMM),
    bytes("notw", &M, Word, &[0xF7], ext(0, 2), NO_IMM),
    bytes("notl", &M, Long, &[0xF7], ext(0, 2), NO_IMM),
    bytes("notq", &M, Quad, &[0xF7], ext(0, 2), NO_IMM),
    bytes("mulb", &M, Byte, &[0xF6], ext(0, 4), NO_IMM),
    bytes("mulw", &M, Word, &[0xF7], ext(0, 4), NO_IMM),
    bytes("mull", &M, Long, &[0xF7], ext(0, 4), NO_IMM),
    bytes("mulq", &M, Quad, &[0xF7], ext(0, 4), NO_IMM),
    bytes("imulb", &M, Byte, &[0xF6], ext(0, 5), NO_IMM),
    bytes("imulw", &M, Word, &[0xF7], ext(0, 5), NO_IMM),
    bytes("imull", &M, Long, &[0xF7], ext(0, 5), NO_IMM),
    bytes("imulq", &M, Quad, &[0xF7], ext(0, 5), NO_IMM),
    bytes("divb", &M, Byte, &[0xF6], ext(0, 6), NO_IMM),
    bytes("divw", &M, Word, &[0xF7], ext(0, 6), NO_IMM),
    bytes("divl", &M, Long, &[0xF7], ext(0, 6), NO_IMM),
    bytes("divq", &M, Quad, &[0xF7], ext(0, 6), NO_IMM),
    bytes("idivb", &M, Byte, &[0xF6], ext(0, 7), NO_IMM),
    bytes("idivw", &M, Word, &[0xF7], ext(0, 7), NO_IMM),
    bytes("idivl", &M, Long, &[0xF7], ext(0, 7), NO_IMM),
    bytes("idivq", &M, Quad, &[0xF7], ext(0, 7), NO_IMM),
    // The two that share the other opcode of the pair, counted up and counted down.
    bytes("incb", &M, Byte, &[0xFE], ext(0, 0), NO_IMM),
    bytes("incw", &M, Word, &[0xFF], ext(0, 0), NO_IMM),
    bytes("incl", &M, Long, &[0xFF], ext(0, 0), NO_IMM),
    bytes("incq", &M, Quad, &[0xFF], ext(0, 0), NO_IMM),
    bytes("decb", &M, Byte, &[0xFE], ext(0, 1), NO_IMM),
    bytes("decw", &M, Word, &[0xFF], ext(0, 1), NO_IMM),
    bytes("decl", &M, Long, &[0xFF], ext(0, 1), NO_IMM),
    bytes("decq", &M, Quad, &[0xFF], ext(0, 1), NO_IMM),
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
    // The rotates, which are the first two of the eight the shifts are the last four of. A C
    // program has no way to write one, which is why this compiler never emits one and why every
    // library that wants one writes it in assembly or in a builtin, but they are the same opcode
    // and the same two shapes as the shifts beside them.
    takes("rolb", &IR, Fits::Byte, Byte, &[0xC0], ext(1, 0), ImmSize::Ib),
    takes("rolw", &IR, Fits::Byte, Word, &[0xC1], ext(1, 0), ImmSize::Ib),
    takes("roll", &IR, Fits::Byte, Long, &[0xC1], ext(1, 0), ImmSize::Ib),
    takes("rolq", &IR, Fits::Byte, Quad, &[0xC1], ext(1, 0), ImmSize::Ib),
    takes("rorb", &IR, Fits::Byte, Byte, &[0xC0], ext(1, 1), ImmSize::Ib),
    takes("rorw", &IR, Fits::Byte, Word, &[0xC1], ext(1, 1), ImmSize::Ib),
    takes("rorl", &IR, Fits::Byte, Long, &[0xC1], ext(1, 1), ImmSize::Ib),
    takes("rorq", &IR, Fits::Byte, Quad, &[0xC1], ext(1, 1), ImmSize::Ib),
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
    bytes("rolb", &RR, Byte, &[0xD2], ext(1, 0), NO_IMM),
    bytes("rolw", &RR, Word, &[0xD3], ext(1, 0), NO_IMM),
    bytes("roll", &RR, Long, &[0xD3], ext(1, 0), NO_IMM),
    bytes("rolq", &RR, Quad, &[0xD3], ext(1, 0), NO_IMM),
    bytes("rorb", &RR, Byte, &[0xD2], ext(1, 1), NO_IMM),
    bytes("rorw", &RR, Word, &[0xD3], ext(1, 1), NO_IMM),
    bytes("rorl", &RR, Long, &[0xD3], ext(1, 1), NO_IMM),
    bytes("rorq", &RR, Quad, &[0xD3], ext(1, 1), NO_IMM),
    // The two rotates that go through the carry, which are the middle pair of the eight and are the
    // reason the group is eight and not six. They shift a bit out into the carry and the old carry
    // in at the other end, so a chain of them moves a number wider than a register one place, which
    // is what a halving of a multiple precision sum is and is the only thing anybody uses them for.
    // No C expression names one, the way no C expression names a plain rotate.
    takes("rclb", &IR, Fits::Byte, Byte, &[0xC0], ext(1, 2), ImmSize::Ib),
    takes("rclw", &IR, Fits::Byte, Word, &[0xC1], ext(1, 2), ImmSize::Ib),
    takes("rcll", &IR, Fits::Byte, Long, &[0xC1], ext(1, 2), ImmSize::Ib),
    takes("rclq", &IR, Fits::Byte, Quad, &[0xC1], ext(1, 2), ImmSize::Ib),
    takes("rcrb", &IR, Fits::Byte, Byte, &[0xC0], ext(1, 3), ImmSize::Ib),
    takes("rcrw", &IR, Fits::Byte, Word, &[0xC1], ext(1, 3), ImmSize::Ib),
    takes("rcrl", &IR, Fits::Byte, Long, &[0xC1], ext(1, 3), ImmSize::Ib),
    takes("rcrq", &IR, Fits::Byte, Quad, &[0xC1], ext(1, 3), ImmSize::Ib),
    bytes("rclb", &RR, Byte, &[0xD2], ext(1, 2), NO_IMM),
    bytes("rclw", &RR, Word, &[0xD3], ext(1, 2), NO_IMM),
    bytes("rcll", &RR, Long, &[0xD3], ext(1, 2), NO_IMM),
    bytes("rclq", &RR, Quad, &[0xD3], ext(1, 2), NO_IMM),
    bytes("rcrb", &RR, Byte, &[0xD2], ext(1, 3), NO_IMM),
    bytes("rcrw", &RR, Word, &[0xD3], ext(1, 3), NO_IMM),
    bytes("rcrl", &RR, Long, &[0xD3], ext(1, 3), NO_IMM),
    bytes("rcrq", &RR, Quad, &[0xD3], ext(1, 3), NO_IMM),
    // All eight of them shifting by one place with nothing written down, which is a third opcode
    // beside the constant and the count. It is one byte shorter than writing the one out, and it is
    // not the same instruction as writing the one out: the overflow flag it leaves is defined here
    // and undefined there, which matters to the `rcr` a halving is built from.
    //
    // This is a row and not a choice. Nothing here decides to write `D1` where a file wrote `C1`
    // with a one behind it, because that is picking a shorter encoding for the same instruction and
    // is the relaxation pass's business rather than a lookup's. What this does is take the file
    // that wrote the one operand form, which is what every hand written file writes.
    bytes("rolb", &R, Byte, &[0xD0], ext(0, 0), NO_IMM),
    bytes("rolw", &R, Word, &[0xD1], ext(0, 0), NO_IMM),
    bytes("roll", &R, Long, &[0xD1], ext(0, 0), NO_IMM),
    bytes("rolq", &R, Quad, &[0xD1], ext(0, 0), NO_IMM),
    bytes("rorb", &R, Byte, &[0xD0], ext(0, 1), NO_IMM),
    bytes("rorw", &R, Word, &[0xD1], ext(0, 1), NO_IMM),
    bytes("rorl", &R, Long, &[0xD1], ext(0, 1), NO_IMM),
    bytes("rorq", &R, Quad, &[0xD1], ext(0, 1), NO_IMM),
    bytes("rclb", &R, Byte, &[0xD0], ext(0, 2), NO_IMM),
    bytes("rclw", &R, Word, &[0xD1], ext(0, 2), NO_IMM),
    bytes("rcll", &R, Long, &[0xD1], ext(0, 2), NO_IMM),
    bytes("rclq", &R, Quad, &[0xD1], ext(0, 2), NO_IMM),
    bytes("rcrb", &R, Byte, &[0xD0], ext(0, 3), NO_IMM),
    bytes("rcrw", &R, Word, &[0xD1], ext(0, 3), NO_IMM),
    bytes("rcrl", &R, Long, &[0xD1], ext(0, 3), NO_IMM),
    bytes("rcrq", &R, Quad, &[0xD1], ext(0, 3), NO_IMM),
    bytes("shlb", &R, Byte, &[0xD0], ext(0, 4), NO_IMM),
    bytes("shlw", &R, Word, &[0xD1], ext(0, 4), NO_IMM),
    bytes("shll", &R, Long, &[0xD1], ext(0, 4), NO_IMM),
    bytes("shlq", &R, Quad, &[0xD1], ext(0, 4), NO_IMM),
    bytes("shrb", &R, Byte, &[0xD0], ext(0, 5), NO_IMM),
    bytes("shrw", &R, Word, &[0xD1], ext(0, 5), NO_IMM),
    bytes("shrl", &R, Long, &[0xD1], ext(0, 5), NO_IMM),
    bytes("shrq", &R, Quad, &[0xD1], ext(0, 5), NO_IMM),
    bytes("sarb", &R, Byte, &[0xD0], ext(0, 7), NO_IMM),
    bytes("sarw", &R, Word, &[0xD1], ext(0, 7), NO_IMM),
    bytes("sarl", &R, Long, &[0xD1], ext(0, 7), NO_IMM),
    bytes("sarq", &R, Quad, &[0xD1], ext(0, 7), NO_IMM),
    // The two shifts that name a second register to shift in from, which are the only instructions
    // on this machine with three operands that are not a multiply. They move a window across a pair
    // of registers, which is what shifting a number wider than a register is, and they are how GMP
    // writes every one of its shifting loops.
    //
    // The count comes first in both shapes, either as a number or in `cl`, and the register shifted
    // in is second and the one shifted is last. That is the same order the manual writes them in
    // backwards, and the addressing byte therefore names the last argument and holds the middle one
    // beside it, which is the opposite way round from the three operand multiply above.
    takes("shldw", &IRR, Fits::Byte, Word, &[0x0F, 0xA4], pair(2, 1), ImmSize::Ib),
    takes("shldl", &IRR, Fits::Byte, Long, &[0x0F, 0xA4], pair(2, 1), ImmSize::Ib),
    takes("shldq", &IRR, Fits::Byte, Quad, &[0x0F, 0xA4], pair(2, 1), ImmSize::Ib),
    takes("shrdw", &IRR, Fits::Byte, Word, &[0x0F, 0xAC], pair(2, 1), ImmSize::Ib),
    takes("shrdl", &IRR, Fits::Byte, Long, &[0x0F, 0xAC], pair(2, 1), ImmSize::Ib),
    takes("shrdq", &IRR, Fits::Byte, Quad, &[0x0F, 0xAC], pair(2, 1), ImmSize::Ib),
    bytes("shldw", &RRR, Word, &[0x0F, 0xA5], pair(2, 1), NO_IMM),
    bytes("shldl", &RRR, Long, &[0x0F, 0xA5], pair(2, 1), NO_IMM),
    bytes("shldq", &RRR, Quad, &[0x0F, 0xA5], pair(2, 1), NO_IMM),
    bytes("shrdw", &RRR, Word, &[0x0F, 0xAD], pair(2, 1), NO_IMM),
    bytes("shrdl", &RRR, Long, &[0x0F, 0xAD], pair(2, 1), NO_IMM),
    bytes("shrdq", &RRR, Quad, &[0x0F, 0xAD], pair(2, 1), NO_IMM),
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
    bytes("cmovew", &RR, Word, &[0x0F, 0x44], pair(0, 1), NO_IMM),
    bytes("cmovel", &RR, Long, &[0x0F, 0x44], pair(0, 1), NO_IMM),
    bytes("cmoveq", &RR, Quad, &[0x0F, 0x44], pair(0, 1), NO_IMM),
    bytes("cmovnew", &RR, Word, &[0x0F, 0x45], pair(0, 1), NO_IMM),
    bytes("cmovnel", &RR, Long, &[0x0F, 0x45], pair(0, 1), NO_IMM),
    bytes("cmovneq", &RR, Quad, &[0x0F, 0x45], pair(0, 1), NO_IMM),
    bytes("cmovlw", &RR, Word, &[0x0F, 0x4C], pair(0, 1), NO_IMM),
    bytes("cmovll", &RR, Long, &[0x0F, 0x4C], pair(0, 1), NO_IMM),
    bytes("cmovlq", &RR, Quad, &[0x0F, 0x4C], pair(0, 1), NO_IMM),
    bytes("cmovlew", &RR, Word, &[0x0F, 0x4E], pair(0, 1), NO_IMM),
    bytes("cmovlel", &RR, Long, &[0x0F, 0x4E], pair(0, 1), NO_IMM),
    bytes("cmovleq", &RR, Quad, &[0x0F, 0x4E], pair(0, 1), NO_IMM),
    bytes("cmovgw", &RR, Word, &[0x0F, 0x4F], pair(0, 1), NO_IMM),
    bytes("cmovgl", &RR, Long, &[0x0F, 0x4F], pair(0, 1), NO_IMM),
    bytes("cmovgq", &RR, Quad, &[0x0F, 0x4F], pair(0, 1), NO_IMM),
    bytes("cmovgew", &RR, Word, &[0x0F, 0x4D], pair(0, 1), NO_IMM),
    bytes("cmovgel", &RR, Long, &[0x0F, 0x4D], pair(0, 1), NO_IMM),
    bytes("cmovgeq", &RR, Quad, &[0x0F, 0x4D], pair(0, 1), NO_IMM),
    bytes("cmovbw", &RR, Word, &[0x0F, 0x42], pair(0, 1), NO_IMM),
    bytes("cmovbl", &RR, Long, &[0x0F, 0x42], pair(0, 1), NO_IMM),
    bytes("cmovbq", &RR, Quad, &[0x0F, 0x42], pair(0, 1), NO_IMM),
    bytes("cmovbew", &RR, Word, &[0x0F, 0x46], pair(0, 1), NO_IMM),
    bytes("cmovbel", &RR, Long, &[0x0F, 0x46], pair(0, 1), NO_IMM),
    bytes("cmovbeq", &RR, Quad, &[0x0F, 0x46], pair(0, 1), NO_IMM),
    bytes("cmovaw", &RR, Word, &[0x0F, 0x47], pair(0, 1), NO_IMM),
    bytes("cmoval", &RR, Long, &[0x0F, 0x47], pair(0, 1), NO_IMM),
    bytes("cmovaq", &RR, Quad, &[0x0F, 0x47], pair(0, 1), NO_IMM),
    bytes("cmovaew", &RR, Word, &[0x0F, 0x43], pair(0, 1), NO_IMM),
    bytes("cmovael", &RR, Long, &[0x0F, 0x43], pair(0, 1), NO_IMM),
    bytes("cmovaeq", &RR, Quad, &[0x0F, 0x43], pair(0, 1), NO_IMM),
    // The six conditions above that this compiler has no way to reach. A C comparison comes out as
    // one of the ten, because the sign and the overflow and the parity are not things an expression
    // asks about on their own: what `a < b` wants to know is the two of them together, which is `l`,
    // and the sign by itself is only the answer when one side is a zero the selector folded away.
    // A file written by hand asks about them directly and GMP does it in every other loop.
    bytes("cmovow", &RR, Word, &[0x0F, 0x40], pair(0, 1), NO_IMM),
    bytes("cmovol", &RR, Long, &[0x0F, 0x40], pair(0, 1), NO_IMM),
    bytes("cmovoq", &RR, Quad, &[0x0F, 0x40], pair(0, 1), NO_IMM),
    bytes("cmovnow", &RR, Word, &[0x0F, 0x41], pair(0, 1), NO_IMM),
    bytes("cmovnol", &RR, Long, &[0x0F, 0x41], pair(0, 1), NO_IMM),
    bytes("cmovnoq", &RR, Quad, &[0x0F, 0x41], pair(0, 1), NO_IMM),
    bytes("cmovsw", &RR, Word, &[0x0F, 0x48], pair(0, 1), NO_IMM),
    bytes("cmovsl", &RR, Long, &[0x0F, 0x48], pair(0, 1), NO_IMM),
    bytes("cmovsq", &RR, Quad, &[0x0F, 0x48], pair(0, 1), NO_IMM),
    bytes("cmovnsw", &RR, Word, &[0x0F, 0x49], pair(0, 1), NO_IMM),
    bytes("cmovnsl", &RR, Long, &[0x0F, 0x49], pair(0, 1), NO_IMM),
    bytes("cmovnsq", &RR, Quad, &[0x0F, 0x49], pair(0, 1), NO_IMM),
    bytes("cmovpw", &RR, Word, &[0x0F, 0x4A], pair(0, 1), NO_IMM),
    bytes("cmovpl", &RR, Long, &[0x0F, 0x4A], pair(0, 1), NO_IMM),
    bytes("cmovpq", &RR, Quad, &[0x0F, 0x4A], pair(0, 1), NO_IMM),
    bytes("cmovnpw", &RR, Word, &[0x0F, 0x4B], pair(0, 1), NO_IMM),
    bytes("cmovnpl", &RR, Long, &[0x0F, 0x4B], pair(0, 1), NO_IMM),
    bytes("cmovnpq", &RR, Quad, &[0x0F, 0x4B], pair(0, 1), NO_IMM),
    // All sixteen of them reading memory, which is the same opcode with an address where the source
    // register was and is the shape the instruction was put on the machine for. Loading a value and
    // then deciding whether to keep it is two instructions, one of which might fault on an address
    // the condition says not to read, and a conditional move from memory reads either way, so it is
    // no help with the fault and every help with the branch. This compiler loads first and moves
    // second, which is why it has never needed these. GMP writes `cmovc 16(%r8), %rbx` instead.
    bytes("cmovow", &MR, Word, &[0x0F, 0x40], pair(0, 1), NO_IMM),
    bytes("cmovol", &MR, Long, &[0x0F, 0x40], pair(0, 1), NO_IMM),
    bytes("cmovoq", &MR, Quad, &[0x0F, 0x40], pair(0, 1), NO_IMM),
    bytes("cmovnow", &MR, Word, &[0x0F, 0x41], pair(0, 1), NO_IMM),
    bytes("cmovnol", &MR, Long, &[0x0F, 0x41], pair(0, 1), NO_IMM),
    bytes("cmovnoq", &MR, Quad, &[0x0F, 0x41], pair(0, 1), NO_IMM),
    bytes("cmovbw", &MR, Word, &[0x0F, 0x42], pair(0, 1), NO_IMM),
    bytes("cmovbl", &MR, Long, &[0x0F, 0x42], pair(0, 1), NO_IMM),
    bytes("cmovbq", &MR, Quad, &[0x0F, 0x42], pair(0, 1), NO_IMM),
    bytes("cmovaew", &MR, Word, &[0x0F, 0x43], pair(0, 1), NO_IMM),
    bytes("cmovael", &MR, Long, &[0x0F, 0x43], pair(0, 1), NO_IMM),
    bytes("cmovaeq", &MR, Quad, &[0x0F, 0x43], pair(0, 1), NO_IMM),
    bytes("cmovew", &MR, Word, &[0x0F, 0x44], pair(0, 1), NO_IMM),
    bytes("cmovel", &MR, Long, &[0x0F, 0x44], pair(0, 1), NO_IMM),
    bytes("cmoveq", &MR, Quad, &[0x0F, 0x44], pair(0, 1), NO_IMM),
    bytes("cmovnew", &MR, Word, &[0x0F, 0x45], pair(0, 1), NO_IMM),
    bytes("cmovnel", &MR, Long, &[0x0F, 0x45], pair(0, 1), NO_IMM),
    bytes("cmovneq", &MR, Quad, &[0x0F, 0x45], pair(0, 1), NO_IMM),
    bytes("cmovbew", &MR, Word, &[0x0F, 0x46], pair(0, 1), NO_IMM),
    bytes("cmovbel", &MR, Long, &[0x0F, 0x46], pair(0, 1), NO_IMM),
    bytes("cmovbeq", &MR, Quad, &[0x0F, 0x46], pair(0, 1), NO_IMM),
    bytes("cmovaw", &MR, Word, &[0x0F, 0x47], pair(0, 1), NO_IMM),
    bytes("cmoval", &MR, Long, &[0x0F, 0x47], pair(0, 1), NO_IMM),
    bytes("cmovaq", &MR, Quad, &[0x0F, 0x47], pair(0, 1), NO_IMM),
    bytes("cmovsw", &MR, Word, &[0x0F, 0x48], pair(0, 1), NO_IMM),
    bytes("cmovsl", &MR, Long, &[0x0F, 0x48], pair(0, 1), NO_IMM),
    bytes("cmovsq", &MR, Quad, &[0x0F, 0x48], pair(0, 1), NO_IMM),
    bytes("cmovnsw", &MR, Word, &[0x0F, 0x49], pair(0, 1), NO_IMM),
    bytes("cmovnsl", &MR, Long, &[0x0F, 0x49], pair(0, 1), NO_IMM),
    bytes("cmovnsq", &MR, Quad, &[0x0F, 0x49], pair(0, 1), NO_IMM),
    bytes("cmovpw", &MR, Word, &[0x0F, 0x4A], pair(0, 1), NO_IMM),
    bytes("cmovpl", &MR, Long, &[0x0F, 0x4A], pair(0, 1), NO_IMM),
    bytes("cmovpq", &MR, Quad, &[0x0F, 0x4A], pair(0, 1), NO_IMM),
    bytes("cmovnpw", &MR, Word, &[0x0F, 0x4B], pair(0, 1), NO_IMM),
    bytes("cmovnpl", &MR, Long, &[0x0F, 0x4B], pair(0, 1), NO_IMM),
    bytes("cmovnpq", &MR, Quad, &[0x0F, 0x4B], pair(0, 1), NO_IMM),
    bytes("cmovlw", &MR, Word, &[0x0F, 0x4C], pair(0, 1), NO_IMM),
    bytes("cmovll", &MR, Long, &[0x0F, 0x4C], pair(0, 1), NO_IMM),
    bytes("cmovlq", &MR, Quad, &[0x0F, 0x4C], pair(0, 1), NO_IMM),
    bytes("cmovgew", &MR, Word, &[0x0F, 0x4D], pair(0, 1), NO_IMM),
    bytes("cmovgel", &MR, Long, &[0x0F, 0x4D], pair(0, 1), NO_IMM),
    bytes("cmovgeq", &MR, Quad, &[0x0F, 0x4D], pair(0, 1), NO_IMM),
    bytes("cmovlew", &MR, Word, &[0x0F, 0x4E], pair(0, 1), NO_IMM),
    bytes("cmovlel", &MR, Long, &[0x0F, 0x4E], pair(0, 1), NO_IMM),
    bytes("cmovleq", &MR, Quad, &[0x0F, 0x4E], pair(0, 1), NO_IMM),
    bytes("cmovgw", &MR, Word, &[0x0F, 0x4F], pair(0, 1), NO_IMM),
    bytes("cmovgl", &MR, Long, &[0x0F, 0x4F], pair(0, 1), NO_IMM),
    bytes("cmovgq", &MR, Quad, &[0x0F, 0x4F], pair(0, 1), NO_IMM),
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
    // The four conditions left, which complete the sixteen. The sign and the overflow are asked
    // about directly only by a file that knows what the flags hold, which a C expression never does.
    bytes("seto", &R, Byte, &[0x0F, 0x90], ext(0, 0), NO_IMM),
    bytes("setno", &R, Byte, &[0x0F, 0x91], ext(0, 0), NO_IMM),
    bytes("sets", &R, Byte, &[0x0F, 0x98], ext(0, 0), NO_IMM),
    bytes("setns", &R, Byte, &[0x0F, 0x99], ext(0, 0), NO_IMM),
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
    // The same widenings reading memory instead of a register, which is the same opcode with an
    // address where the source register was. `movzbl` has had its row since `_Bool` was read this
    // way and the other ten are here because a file somebody else wrote reaches memory as readily
    // as it reaches a register, where what this compiler emits has always loaded first.
    bytes("movzbw", &MR, Word, &[0x0F, 0xB6], pair(0, 1), NO_IMM),
    bytes("movzbq", &MR, Quad, &[0x0F, 0xB6], pair(0, 1), NO_IMM),
    bytes("movzwl", &MR, Long, &[0x0F, 0xB7], pair(0, 1), NO_IMM),
    bytes("movzwq", &MR, Quad, &[0x0F, 0xB7], pair(0, 1), NO_IMM),
    bytes("movsbw", &MR, Word, &[0x0F, 0xBE], pair(0, 1), NO_IMM),
    bytes("movsbl", &MR, Long, &[0x0F, 0xBE], pair(0, 1), NO_IMM),
    bytes("movsbq", &MR, Quad, &[0x0F, 0xBE], pair(0, 1), NO_IMM),
    bytes("movswl", &MR, Long, &[0x0F, 0xBF], pair(0, 1), NO_IMM),
    bytes("movswq", &MR, Quad, &[0x0F, 0xBF], pair(0, 1), NO_IMM),
    bytes("movslq", &MR, Quad, &[0x63], pair(0, 1), NO_IMM),
    // Asking about one bit and, for the three behind the first, changing it. The immediate form is
    // one opcode told apart by the three spare bits the way the eight above are, and the register
    // form is four opcodes of its own. There is no byte form of either, because the bit is counted
    // off a whole operand and the narrowest operand the instruction has is a word.
    //
    // The register form is written the way the arithmetic is and not the way the conversions are:
    // the bit number is the register beside the addressing byte and the thing it is counted in is
    // what the byte addresses, so the source is first in both the text and the encoding.
    takes("btw", &IR, Fits::Byte, Word, &[0x0F, 0xBA], ext(1, 4), ImmSize::Ib),
    takes("btl", &IR, Fits::Byte, Long, &[0x0F, 0xBA], ext(1, 4), ImmSize::Ib),
    takes("btq", &IR, Fits::Byte, Quad, &[0x0F, 0xBA], ext(1, 4), ImmSize::Ib),
    takes("btsw", &IR, Fits::Byte, Word, &[0x0F, 0xBA], ext(1, 5), ImmSize::Ib),
    takes("btsl", &IR, Fits::Byte, Long, &[0x0F, 0xBA], ext(1, 5), ImmSize::Ib),
    takes("btsq", &IR, Fits::Byte, Quad, &[0x0F, 0xBA], ext(1, 5), ImmSize::Ib),
    takes("btrw", &IR, Fits::Byte, Word, &[0x0F, 0xBA], ext(1, 6), ImmSize::Ib),
    takes("btrl", &IR, Fits::Byte, Long, &[0x0F, 0xBA], ext(1, 6), ImmSize::Ib),
    takes("btrq", &IR, Fits::Byte, Quad, &[0x0F, 0xBA], ext(1, 6), ImmSize::Ib),
    takes("btcw", &IR, Fits::Byte, Word, &[0x0F, 0xBA], ext(1, 7), ImmSize::Ib),
    takes("btcl", &IR, Fits::Byte, Long, &[0x0F, 0xBA], ext(1, 7), ImmSize::Ib),
    takes("btcq", &IR, Fits::Byte, Quad, &[0x0F, 0xBA], ext(1, 7), ImmSize::Ib),
    bytes("btw", &RR, Word, &[0x0F, 0xA3], pair(1, 0), NO_IMM),
    bytes("btl", &RR, Long, &[0x0F, 0xA3], pair(1, 0), NO_IMM),
    bytes("btq", &RR, Quad, &[0x0F, 0xA3], pair(1, 0), NO_IMM),
    bytes("btsw", &RR, Word, &[0x0F, 0xAB], pair(1, 0), NO_IMM),
    bytes("btsl", &RR, Long, &[0x0F, 0xAB], pair(1, 0), NO_IMM),
    bytes("btsq", &RR, Quad, &[0x0F, 0xAB], pair(1, 0), NO_IMM),
    bytes("btrw", &RR, Word, &[0x0F, 0xB3], pair(1, 0), NO_IMM),
    bytes("btrl", &RR, Long, &[0x0F, 0xB3], pair(1, 0), NO_IMM),
    bytes("btrq", &RR, Quad, &[0x0F, 0xB3], pair(1, 0), NO_IMM),
    bytes("btcw", &RR, Word, &[0x0F, 0xBB], pair(1, 0), NO_IMM),
    bytes("btcl", &RR, Long, &[0x0F, 0xBB], pair(1, 0), NO_IMM),
    bytes("btcq", &RR, Quad, &[0x0F, 0xBB], pair(1, 0), NO_IMM),
    // Finding the lowest or the highest bit that is set, which read one operand and write another,
    // so they go the way the conversions above do and not the way the bit tests beside them do.
    bytes("bsfw", &RR, Word, &[0x0F, 0xBC], pair(0, 1), NO_IMM),
    bytes("bsfl", &RR, Long, &[0x0F, 0xBC], pair(0, 1), NO_IMM),
    bytes("bsfq", &RR, Quad, &[0x0F, 0xBC], pair(0, 1), NO_IMM),
    bytes("bsrw", &RR, Word, &[0x0F, 0xBD], pair(0, 1), NO_IMM),
    bytes("bsrl", &RR, Long, &[0x0F, 0xBD], pair(0, 1), NO_IMM),
    bytes("bsrq", &RR, Quad, &[0x0F, 0xBD], pair(0, 1), NO_IMM),
    bytes("bsfw", &MR, Word, &[0x0F, 0xBC], pair(0, 1), NO_IMM),
    bytes("bsfl", &MR, Long, &[0x0F, 0xBC], pair(0, 1), NO_IMM),
    bytes("bsfq", &MR, Quad, &[0x0F, 0xBC], pair(0, 1), NO_IMM),
    bytes("bsrw", &MR, Word, &[0x0F, 0xBD], pair(0, 1), NO_IMM),
    bytes("bsrl", &MR, Long, &[0x0F, 0xBD], pair(0, 1), NO_IMM),
    bytes("bsrq", &MR, Quad, &[0x0F, 0xBD], pair(0, 1), NO_IMM),
    // A copy between registers, which is a store to a register rather than a load from one, so it
    // is written the same way round as the arithmetic above and not as the conversions.
    bytes("movb", &RR, Byte, &[0x88], pair(1, 0), NO_IMM),
    bytes("movw", &RR, Word, &[0x89], pair(1, 0), NO_IMM),
    bytes("movl", &RR, Long, &[0x89], pair(1, 0), NO_IMM),
    bytes("movq", &RR, Quad, &[0x89], pair(1, 0), NO_IMM),
    // The address computation, which is the one instruction that is given an address and does not
    // read it.
    bytes("leaq", &MR, Quad, &[0x8D], pair(0, 1), NO_IMM),
    // The same instruction keeping only the low thirty two bits of what it worked out, which this
    // compiler has no use for because a pointer here is sixty four bits wide and which hand written
    // code uses as an addition that does not touch the flags.
    bytes("leal", &MR, Long, &[0x8D], pair(0, 1), NO_IMM),
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
    // Reading a byte and widening it in the one instruction, which is the same opcode as the
    // register form above with a memory operand where its register was. The byte the opcode reads
    // is a byte whether the operand is a register or an address, so the width here is the width
    // written rather than the width read, which is what the register form says too. This is how a
    // `_Bool` is read, and it is the only widening load, because it is the only one where the
    // value in memory is narrower than anything that will look at it.
    bytes("movzbl", &MR, Long, &[0x0F, 0xB6], pair(0, 1), NO_IMM),
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
    // The rest of it. A test against zero is the only one this compiler writes and a byte is the
    // only width it writes it at, because what it is testing is a condition that already came out
    // of a comparison. A file written by hand tests a register against itself at its own width and
    // tests single bits with an immediate, which is what `test $1, %r10` is, and neither of those
    // had a row.
    //
    // The immediate here is never sign extended from a byte. The eight instructions that share
    // `0x83` all have that form and this one does not, so a small number still costs the full
    // width, which is one of the few places the table cannot be filled in by analogy.
    bytes("testw", &RR, Word, &[0x85], pair(1, 0), NO_IMM),
    bytes("testl", &RR, Long, &[0x85], pair(1, 0), NO_IMM),
    bytes("testq", &RR, Quad, &[0x85], pair(1, 0), NO_IMM),
    bytes("testb", &RM, Byte, &[0x84], pair(1, 0), NO_IMM),
    bytes("testw", &RM, Word, &[0x85], pair(1, 0), NO_IMM),
    bytes("testl", &RM, Long, &[0x85], pair(1, 0), NO_IMM),
    bytes("testq", &RM, Quad, &[0x85], pair(1, 0), NO_IMM),
    bytes("testb", &MR, Byte, &[0x84], pair(0, 1), NO_IMM),
    bytes("testw", &MR, Word, &[0x85], pair(0, 1), NO_IMM),
    bytes("testl", &MR, Long, &[0x85], pair(0, 1), NO_IMM),
    bytes("testq", &MR, Quad, &[0x85], pair(0, 1), NO_IMM),
    takes("testb", &IR, Fits::Byte, Byte, &[0xF6], ext(1, 0), ImmSize::Ib),
    takes("testw", &IR, Fits::Word, Word, &[0xF7], ext(1, 0), ImmSize::Iw),
    takes("testl", &IR, Fits::Long, Long, &[0xF7], ext(1, 0), ImmSize::Id),
    takes("testq", &IR, Signed32, Quad, &[0xF7], ext(1, 0), ImmSize::Id),
    takes("testb", &IM, Fits::Byte, Byte, &[0xF6], ext(1, 0), ImmSize::Ib),
    takes("testw", &IM, Fits::Word, Word, &[0xF7], ext(1, 0), ImmSize::Iw),
    takes("testl", &IM, Fits::Long, Long, &[0xF7], ext(1, 0), ImmSize::Id),
    takes("testq", &IM, Signed32, Quad, &[0xF7], ext(1, 0), ImmSize::Id),
    // The three instructions that do nothing but touch the carry. A C expression has no carry to
    // touch and the selector never leaves one lying about on purpose, so none of these has been
    // wanted. GMP clears it before a loop that will add into it and complements it to turn a borrow
    // into a carry, and both of those are a whole instruction because there is nothing smaller.
    bytes("clc", &NO_ARGS, Long, &[0xF8], NO_MODRM, NO_IMM),
    bytes("stc", &NO_ARGS, Long, &[0xF9], NO_MODRM, NO_IMM),
    bytes("cmc", &NO_ARGS, Long, &[0xF5], NO_MODRM, NO_IMM),
    // The string move, which reads at `rsi` and writes at `rdi` and steps both, and which is one
    // instruction and a whole copy when a repeat prefix is put in front of it. Nothing this
    // compiler emits is a string instruction, because a copy it knows the length of comes out as a
    // loop it can schedule and one it does not comes out as a call to `memcpy`. A file that wants
    // the two byte version writes it itself, which is what GMP's `copyi` does.
    bytes("movsb", &NO_ARGS, Byte, &[0xA4], NO_MODRM, NO_IMM),
    bytes("movsw", &NO_ARGS, Word, &[0xA5], NO_MODRM, NO_IMM),
    bytes("movsl", &NO_ARGS, Long, &[0xA5], NO_MODRM, NO_IMM),
    bytes("movsq", &NO_ARGS, Quad, &[0xA5], NO_MODRM, NO_IMM),
    // The ten conditional jumps, which are one opcode column of sixteen and are told apart by the
    // low four bits the way the ten `setcc` above are. The low bits are the same ones: a jump on
    // a condition and a set on it differ in the byte before, `0x8` against `0x9`, and in nothing
    // else. A near jump reaches anywhere in the section, and the short form that fits its
    // distance in one byte is not here because choosing it is not an encoding question: it needs
    // the distance, the distance needs the layout, and the layout changes when a jump gets
    // shorter. That is a pass over a whole section and `je` above has been waiting for it since
    // before there was anything to jump on.
    bytes("je", &D, Long, &[0x0F, 0x84], NO_MODRM, ImmSize::Cd),
    bytes("jne", &D, Long, &[0x0F, 0x85], NO_MODRM, ImmSize::Cd),
    bytes("jl", &D, Long, &[0x0F, 0x8C], NO_MODRM, ImmSize::Cd),
    bytes("jle", &D, Long, &[0x0F, 0x8E], NO_MODRM, ImmSize::Cd),
    bytes("jg", &D, Long, &[0x0F, 0x8F], NO_MODRM, ImmSize::Cd),
    bytes("jge", &D, Long, &[0x0F, 0x8D], NO_MODRM, ImmSize::Cd),
    bytes("jb", &D, Long, &[0x0F, 0x82], NO_MODRM, ImmSize::Cd),
    bytes("jbe", &D, Long, &[0x0F, 0x86], NO_MODRM, ImmSize::Cd),
    bytes("ja", &D, Long, &[0x0F, 0x87], NO_MODRM, ImmSize::Cd),
    bytes("jae", &D, Long, &[0x0F, 0x83], NO_MODRM, ImmSize::Cd),
    // The other six of the sixteen, which sit in the same column and differ in the same four bits.
    // A loop that counts up to zero ends with `js` and a multiple precision add that has run out of
    // limbs ends with `jnc`, and neither of those is something a C expression asks for, so the ten
    // above were enough until a file somebody else wrote came along.
    bytes("jo", &D, Long, &[0x0F, 0x80], NO_MODRM, ImmSize::Cd),
    bytes("jno", &D, Long, &[0x0F, 0x81], NO_MODRM, ImmSize::Cd),
    bytes("js", &D, Long, &[0x0F, 0x88], NO_MODRM, ImmSize::Cd),
    bytes("jns", &D, Long, &[0x0F, 0x89], NO_MODRM, ImmSize::Cd),
    bytes("jp", &D, Long, &[0x0F, 0x8A], NO_MODRM, ImmSize::Cd),
    bytes("jnp", &D, Long, &[0x0F, 0x8B], NO_MODRM, ImmSize::Cd),
    bytes("jmp", &D, Long, &[0xE9], NO_MODRM, ImmSize::Cd),
    // The jump on a register being zero, which is the one branch on this machine that has no long
    // form at all: a hundred and twenty seven bytes either way is the whole of its reach, and a
    // program that needs further writes a test and one of the jumps above. It reads `rcx` and names
    // it in the mnemonic rather than in an operand, so there is nothing here but a destination.
    bytes("jrcxz", &D, Long, &[0xE3], NO_MODRM, ImmSize::Cb),
    // The same mnemonic through a register, which is a different row for the reason the call above
    // has two: what a row is looked up by is the arguments as well as the name. It is another of
    // the eight that share `0xFF` and sits one place along from the call, and it is sixty four bits
    // without a prefix saying so for the same reason the call is.
    bytes("jmp", &R, Long, &[0xFF], ext(0, 4), NO_IMM),
    // What a prologue and an epilogue are made of. A push and a pop move eight bytes without
    // being told to, so neither carries the prefix that would say so.
    bytes("pushq", &R, Long, &[0x50], plus(0), NO_IMM),
    bytes("popq", &R, Long, &[0x58], plus(0), NO_IMM),
    // The same two moving eight bytes to and from memory rather than a register, which are not the
    // short forms above at all: the push is another of the eight that share `0xFF` and the pop has
    // an opcode nothing else uses. Eight bytes without a prefix saying so, the way the short forms
    // are, and this compiler writes neither because a spill it made has a register in hand.
    bytes("pushq", &M, Long, &[0xFF], ext(0, 6), NO_IMM),
    bytes("popq", &M, Long, &[0x8F], ext(0, 0), NO_IMM),
    bytes("ret", &NO_ARGS, Long, &[0xC3], NO_MODRM, NO_IMM),
    // The barrier. Three bytes with no operands, so the last of them is written as part of the
    // opcode rather than built: `0xF0` is the addressing byte that names no memory and no
    // register, and there is nothing here that could choose a different one.
    bytes("mfence", &NO_ARGS, Long, &[0x0F, 0xAE, 0xF0], NO_MODRM, NO_IMM),
    // The four hints, which are one opcode told apart by the three spare bits of the addressing
    // byte, the way the eight instructions sharing `0xFF` are. The size is `Long` because a row has
    // to name one and there is no prefix to write: the instruction is about a line in the cache and
    // not about however many bytes a later read will take out of it.
    bytes("prefetchnta", &M, Long, &[0x0F, 0x18], ext(0, 0), NO_IMM),
    bytes("prefetcht0", &M, Long, &[0x0F, 0x18], ext(0, 1), NO_IMM),
    bytes("prefetcht1", &M, Long, &[0x0F, 0x18], ext(0, 2), NO_IMM),
    bytes("prefetcht2", &M, Long, &[0x0F, 0x18], ext(0, 3), NO_IMM),
    // The instruction a program stops on. Two bytes and no operands, and what makes it work is
    // that the manual promises this opcode will never be given a meaning, so every processor there
    // is raises the fault for an instruction it does not know rather than doing something.
    bytes("ud2", &NO_ARGS, Long, &[0x0F, 0x0B], NO_MODRM, NO_IMM),
    // The landing pad, and four bytes for the same reason the barrier is three: no operands, so
    // the addressing byte at the end of it is part of the opcode. A machine that does not check
    // reads the whole of it as a wider `nop`, which is what makes an object built with it run
    // everywhere rather than only where the check exists.
    bytes("endbr64", &NO_ARGS, Long, &[0xF3, 0x0F, 0x1E, 0xFA], NO_MODRM, NO_IMM),
    // The byte that does nothing, which is the one byte form rather than any of the longer ones.
    // Length is what `-fpatchable-function-entry=` counts, and a patcher writing over the room it
    // asked for wants a whole number of bytes it can start at, so the reserved space is that many
    // one byte instructions and not the shortest sequence that adds up.
    bytes("nop", &NO_ARGS, Long, &[0x90], NO_MODRM, NO_IMM),
    // The spin loop hint, which is the byte above with the repeat prefix in front of it. A machine
    // that has never heard of it decodes the prefix as having nothing to repeat and runs the `nop`,
    // which is why the hint could be added to the instruction set without breaking anything that
    // was already written.
    bytes("pause", &NO_ARGS, Long, &[0xF3, 0x90], NO_MODRM, NO_IMM),
    // What the processor is asked about itself. No arguments in the encoding for the reason there
    // are none in the text: every register it touches is named by the instruction rather than by
    // anything written beside it, so there is no addressing byte and nothing to put in one.
    bytes("cpuid", &NO_ARGS, Long, &[0x0F, 0xA2], NO_MODRM, NO_IMM),
    // The lock prefix, which is a row of its own because that is what it is in the encoding: one
    // byte in front of the instruction it applies to, and not a bit of anything the instruction
    // itself writes. An assembler reads it the same way, so the text form is the word on a line of
    // its own in front of the instruction, which is what an opcode with two spellings already does
    // for a comparison and the byte it sets.
    //
    // No arguments, so no addressing byte and no REX, and the size is `Long` only because a row
    // has to name one and `Long` is the size that writes no prefix at all.
    bytes("lock", &NO_ARGS, Long, &[0xF0], NO_MODRM, NO_IMM),
    // Compare and exchange, which is the one instruction on this machine that reads a register the
    // program did not name: it compares what is at the address against `rax` and puts what it
    // found there whichever way the comparison went. The byte form is one opcode below the rest,
    // the way every other pair of a byte form and a wider one here is.
    bytes("cmpxchgb", &RM, Byte, &[0x0F, 0xB0], pair(1, 0), NO_IMM),
    bytes("cmpxchgw", &RM, Word, &[0x0F, 0xB1], pair(1, 0), NO_IMM),
    bytes("cmpxchgl", &RM, Long, &[0x0F, 0xB1], pair(1, 0), NO_IMM),
    bytes("cmpxchgq", &RM, Quad, &[0x0F, 0xB1], pair(1, 0), NO_IMM),
    // The exchange and the exchange and add, which are the two read modify writes this machine does
    // in one instruction. Both put the register beside the addressing byte and the object in the
    // addressing mode, the way a compare and exchange does, and both are a byte form one opcode
    // below a form for the three wider sizes.
    bytes("xchgb", &RM, Byte, &[0x86], pair(1, 0), NO_IMM),
    bytes("xchgw", &RM, Word, &[0x87], pair(1, 0), NO_IMM),
    bytes("xchgl", &RM, Long, &[0x87], pair(1, 0), NO_IMM),
    bytes("xchgq", &RM, Quad, &[0x87], pair(1, 0), NO_IMM),
    // The exchange between two registers, which is the same opcode with a register where the
    // address was. It is not atomic and needs no lock prefix, since nothing else can see either end
    // of it, which is why this compiler only ever reaches for the form above.
    bytes("xchgb", &RR, Byte, &[0x86], pair(1, 0), NO_IMM),
    bytes("xchgw", &RR, Word, &[0x87], pair(1, 0), NO_IMM),
    bytes("xchgl", &RR, Long, &[0x87], pair(1, 0), NO_IMM),
    bytes("xchgq", &RR, Quad, &[0x87], pair(1, 0), NO_IMM),
    bytes("xaddb", &RM, Byte, &[0x0F, 0xC0], pair(1, 0), NO_IMM),
    bytes("xaddw", &RM, Word, &[0x0F, 0xC1], pair(1, 0), NO_IMM),
    bytes("xaddl", &RM, Long, &[0x0F, 0xC1], pair(1, 0), NO_IMM),
    bytes("xaddq", &RM, Quad, &[0x0F, 0xC1], pair(1, 0), NO_IMM),
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
    // The same move between two vector registers, which takes the load opcode because the load is
    // the one whose destination is the register beside the addressing byte. This compiler writes
    // `movaps` for a copy instead, since it moves the whole register and has no prefix to carry,
    // but a file somebody else wrote is entitled to say what it means.
    bytes("movss", &VV, Single, &[0x0F, 0x10], pair(0, 1), NO_IMM),
    bytes("movsd", &VV, Double, &[0x0F, 0x10], pair(0, 1), NO_IMM),
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
    // The arithmetic, which is the first group of rows here with no addressing byte and no
    // register number anywhere in it. Two opcode bytes each and both of them constant: `0xDE` says
    // the operation works on two values on the stack and pops, the low three bits of the second
    // byte are the depth of the second value, and the rest of it says which operation. So the
    // depth is not encoded from an argument the way a register number is, it is part of the
    // opcode, and the argument list is here to pick the row rather than to be written out.
    //
    // A subtraction and a division come in two, because the machine cannot swap two depths and a
    // rule that wanted the other order has nowhere to put it. `0xE1` against `0xE9` and `0xF1`
    // against `0xF9` are the same operation with the operands the other way round.
    bytes("faddp", &SS, Long, &[0xDE, 0xC1], NO_MODRM, NO_IMM),
    bytes("fsubp", &SS, Long, &[0xDE, 0xE1], NO_MODRM, NO_IMM),
    bytes("fsubrp", &SS, Long, &[0xDE, 0xE9], NO_MODRM, NO_IMM),
    bytes("fmulp", &SS, Long, &[0xDE, 0xC9], NO_MODRM, NO_IMM),
    bytes("fdivp", &SS, Long, &[0xDE, 0xF1], NO_MODRM, NO_IMM),
    bytes("fdivrp", &SS, Long, &[0xDE, 0xF9], NO_MODRM, NO_IMM),
    // The two that work on the top alone, which name nothing at all: there is one value they could
    // be talking about and the opcode is the whole instruction.
    bytes("fchs", &NO_ARGS, Long, &[0xD9, 0xE0], NO_MODRM, NO_IMM),
    bytes("fabs", &NO_ARGS, Long, &[0xD9, 0xE1], NO_MODRM, NO_IMM),
    // The comparison, which writes the flags this machine's conditional jumps and byte sets read
    // rather than the status word the x87 has of its own. That is what the `i` in the middle is
    // and it is why there is no `fnstsw` here: the older way of reading an x87 comparison is to
    // save the status word into `ax` and pick the bits out, and the way this uses has been in the
    // machine since the Pentium Pro and is in the baseline.
    //
    // The `u` says a quiet NaN is an answer rather than an exception, which is the same choice
    // `ucomisd` is here for and no choice at all: the ordered comparison is a rule with an extra
    // condition on it, not a second instruction.
    bytes("fucomip", &SS, Long, &[0xDF, 0xE9], NO_MODRM, NO_IMM),
    // The pop that throws its value away, which is how the second operand of a comparison comes
    // off the stack. `0xDD 0xD8` is a store to the top itself, which is a store to where the value
    // already is, so the whole of what it does is the pop on the end of it.
    bytes("fstp", &S, Long, &[0xDD, 0xD8], NO_MODRM, NO_IMM),
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
    /// Which storage the address is in, when it is not the flat one. See [`Segment`].
    pub segment: Option<Segment>,
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
    /// A position on the x87 stack, which carries no number because nothing is written from it.
    ///
    /// [`Kind::Stack`] says the rest. The depth the instruction works at is in the opcode, and
    /// which depth that is comes from the text table, so what is left here is the fact that an
    /// argument was there at all, which is what the lookup needs.
    Stack,
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
            Value::Stack => Kind::Stack,
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

        // Group two of the legacy prefixes, in front of everything else because that is where a
        // segment override goes. It says which storage the address is in, which is a fact about
        // the address rather than about how wide the operands are, so it is read off the address
        // rather than off the row.
        if let Some(prefix) = self.segment() {
            out.push(prefix);
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
            ImmSize::Cb => {
                holes.dest = Some(out.len());
                out.push(0);
            }
        }
        Ok(holes)
    }

    /// The prefix that says the address is in a thread's own block, when one of the arguments is
    /// such an address. At most one argument of an instruction is an address at all.
    fn segment(&self) -> Option<u8> {
        let segment = self.values.iter().find_map(|value| match value {
            Value::Mem(addr) => addr.segment,
            _ => None,
        })?;
        Some(match segment {
            Segment::Fs => 0x64,
            Segment::Gs => 0x65,
        })
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
    use crate::x86_64::{
        INSTS, R8, R10, R12, R13, R15, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP, xmm,
    };

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
        assert_eq!(hex("movl", &[Value::Imm(0xffff_ffff), long(RAX)]), "b8 ff ff ff ff");
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
        // thirty two bit register that could hold a number too big for four bytes. It is also the
        // one place the destination is in the opcode rather than in a byte of its own, which is
        // what makes it five bytes where the sixty four bit form is seven.
        assert_eq!(hex("movl", &[Value::Imm(1), long(RAX)]), "b8 01 00 00 00");
        assert_eq!(hex("movl", &[Value::Imm(1), long(RCX)]), "b9 01 00 00 00");
        assert_eq!(hex("movw", &[Value::Imm(1), word(RAX)]), "66 b8 01 00");
        assert_eq!(hex("movb", &[Value::Imm(1), byte(RCX)]), "b1 01");
        // A byte register the opcode has no number for without a prefix still gets the prefix,
        // because the register is counted the same way whichever field it lands in.
        assert_eq!(hex("movb", &[Value::Imm(1), byte(RSI)]), "40 b6 01");
    }

    /// The eighth of the eight that share an opcode column, which is the one that keeps none of
    /// the answer and only the flags. Its `/digit` is seven, so the byte naming the register is
    /// `0xf8` plus its number rather than `0xc0` plus it, and getting that column wrong is the
    /// failure that writes a subtraction where a comparison was meant.
    #[test]
    fn a_comparison_against_an_immediate_is_the_eighth_column_of_the_shared_opcode() {
        assert_eq!(hex("cmpl", &[Value::Imm(1), long(RCX)]), "83 f9 01");
        assert_eq!(hex("cmpl", &[Value::Imm(-1), long(RCX)]), "83 f9 ff");
        assert_eq!(hex("cmpl", &[Value::Imm(1000), long(RCX)]), "81 f9 e8 03 00 00");
        assert_eq!(hex("cmpq", &[Value::Imm(8), quad(RSP)]), "48 83 fc 08");
        assert_eq!(hex("cmpq", &[Value::Imm(100_000), quad(RAX)]), "48 81 f8 a0 86 01 00");
        assert_eq!(hex("cmpw", &[Value::Imm(1), word(RAX)]), "66 83 f8 01");
        assert_eq!(hex("cmpw", &[Value::Imm(1000), word(RAX)]), "66 81 f8 e8 03");
        assert_eq!(hex("cmpb", &[Value::Imm(200), byte(RAX)]), "80 f8 c8");
        // Sixty four bits carries four bytes of immediate at the most, and a number needing more
        // is refused here the way it is refused for the arithmetic, rather than cut down.
        let mut out = Vec::new();
        let big = 0x1_2345_6789;
        let error = encode("cmpq", &[Value::Imm(big), quad(RAX)], &mut out)
            .expect_err("more than four bytes of immediate");
        assert_eq!(error, Error::Immediate { mnemonic: "cmpq".to_owned(), imm: big });
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
        let indexed =
            Addr { base: Some(RCX), index: Some(RDX), scale: 4, disp: -16, ..Addr::default() };
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
        let bad = Addr { base: Some(RCX), index: Some(RSP), scale: 1, ..Addr::default() };
        let error = encode("leaq", &[Value::Mem(bad), quad(RAX)], &mut out)
            .expect_err("the stack pointer as an index");
        assert_eq!(error, Error::Index);
    }

    #[test]
    fn an_address_in_a_thread_s_own_block_is_a_prefix_and_a_constant_and_no_register() {
        // What every protected function on this platform starts with. The prefix comes first of
        // everything, in front of the one that says the operands are sixty-four bits wide, and the
        // address itself names no register at all: `04 25` is the byte pair that means a second
        // addressing byte with no base and no index in it, and then four bytes of constant.
        let guard = Addr { segment: Some(Segment::Fs), disp: 40, ..Addr::default() };
        assert_eq!(hex("movq", &[Value::Mem(guard), quad(RAX)]), "64 48 8b 04 25 28 00 00 00");
        // The other segment, which is the same instruction with the other prefix byte.
        let other = Addr { segment: Some(Segment::Gs), disp: 40, ..Addr::default() };
        assert_eq!(hex("movq", &[Value::Mem(other), quad(RAX)]), "65 48 8b 04 25 28 00 00 00");
        // A register the upper eight, so that the prefix that says so is in the picture too: it
        // goes behind the segment and in front of nothing else, which is the order the machine
        // reads them in.
        let guard = Addr { segment: Some(Segment::Fs), disp: 40, ..Addr::default() };
        assert_eq!(hex("movq", &[Value::Mem(guard), quad(R12)]), "64 4c 8b 24 25 28 00 00 00");
    }

    /// The one instruction here that writes a number to an address, which is a probing prologue
    /// touching the page it has just reached.
    ///
    /// Four bytes, and the middle two are the same pair the stack pointer always needs: its number
    /// in an addressing byte means there is a second byte rather than a register, and the second
    /// byte then says the base is the stack pointer and there is no index. The extension in the
    /// first of them is the one that says this is an inclusive or rather than any of the other
    /// seven instructions that share the opcode, and the last byte is the zero that makes it leave
    /// the page alone.
    #[test]
    fn the_probe_a_prologue_touches_a_page_with_is_the_shortest_write_the_machine_has() {
        let top = Addr { base: Some(RSP), ..Addr::default() };
        assert_eq!(hex("orb", &[Value::Imm(0), Value::Mem(top)]), "80 0c 24 00");
        // Where the loop below a large frame looks, which is the same instruction reaching further
        // down and carrying the displacement it needs.
        let down = Addr { base: Some(RSP), disp: -4096, ..Addr::default() };
        assert_eq!(hex("orb", &[Value::Imm(0), Value::Mem(down)]), "80 8c 24 00 f0 ff ff 00");
    }

    /// The landing pad a prologue writes under `-fcf-protection=branch`.
    ///
    /// Four bytes and none of them chosen, since it has no operands: the whole of it including the
    /// addressing byte at the end is the opcode. The bytes matter because a machine with no check
    /// in it reads the same four as a wider `nop`, which is what lets one object run on a machine
    /// that enforces this and on one that has never heard of it.
    #[test]
    fn the_landing_pad_is_four_bytes_and_none_of_them_are_worked_out() {
        assert_eq!(hex("endbr64", &[]), "f3 0f 1e fa");
        assert_eq!(hex("ud2", &[]), "0f 0b");
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
        let indexed = Addr { base: Some(RCX), index: Some(RDX), scale: 4, ..Addr::default() };
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

    /// The x87 arithmetic, which is two constant bytes each and nothing worked out from anything.
    ///
    /// Worth checking one by one all the same, because the second byte is where the operation and
    /// the depth both live and a row with the wrong one in it would assemble to a different
    /// instruction rather than to nothing. The bytes are what the system assembler produces for
    /// the same lines.
    #[test]
    fn the_x87_arithmetic_is_two_constant_bytes_with_the_operation_in_the_second() {
        let pair = [Value::Stack, Value::Stack];
        let op = |mnemonic| hex(mnemonic, &pair);
        assert_eq!(op("faddp"), "de c1");
        assert_eq!(op("fmulp"), "de c9");
        // The two directions, which differ in one bit of the second byte and are the whole reason
        // a subtraction and a division are two opcodes here and an addition is one.
        assert_eq!(op("fsubp"), "de e1");
        assert_eq!(op("fsubrp"), "de e9");
        assert_eq!(op("fdivp"), "de f1");
        assert_eq!(op("fdivrp"), "de f9");
        // The two that name nothing at all, not even a depth, because there is one value they
        // could be talking about.
        assert_eq!(hex("fchs", &[]), "d9 e0");
        assert_eq!(hex("fabs", &[]), "d9 e1");
        // The comparison and the pop that gets the operand it did not take off the stack.
        assert_eq!(op("fucomip"), "df e9");
        assert_eq!(hex("fstp", &[Value::Stack]), "dd d8");
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

    /// A compare and exchange, and the prefix that makes it indivisible.
    ///
    /// The prefix is a row with no arguments because that is what it is in the encoding, one byte
    /// in front of whatever follows it, and the instruction it applies to encodes the same either
    /// way. The instruction itself names the address in the r/m field and the value it would put
    /// there in the register field, which is the direction a store has and the reverse of the one a
    /// conditional move has. Checked against what the assembler makes of the same six lines.
    #[test]
    fn a_compare_and_exchange_is_the_prefix_and_then_a_store_shaped_instruction() {
        let at = Addr { base: Some(RAX), ..Addr::default() };
        assert_eq!(hex("lock", &[]), "f0");
        assert_eq!(hex("cmpxchgb", &[byte(RCX), Value::Mem(at)]), "0f b0 08");
        assert_eq!(hex("cmpxchgw", &[word(RCX), Value::Mem(at)]), "66 0f b1 08");
        assert_eq!(hex("cmpxchgl", &[long(RCX), Value::Mem(at)]), "0f b1 08");
        assert_eq!(hex("cmpxchgq", &[quad(RCX), Value::Mem(at)]), "48 0f b1 08");
        // The byte form reaches the second half of the register file the same way every other byte
        // form does, which is worth a line because the register it names is one the allocator picks
        // and the other one is always `rax`.
        assert_eq!(hex("cmpxchgb", &[byte(RSI), Value::Mem(at)]), "40 0f b0 30");
    }

    /// The two read modify writes, which name their operands the way a compare and exchange does
    /// and are two different opcodes rather than two spellings of one. The exchange has no prefix in
    /// front of it, which is the machine and not an omission: an exchange with memory is indivisible
    /// whether the prefix is written or not. Checked against what the assembler makes of the same
    /// eight lines.
    #[test]
    fn a_read_modify_write_names_the_address_the_way_a_compare_and_exchange_does() {
        let at = Addr { base: Some(RAX), ..Addr::default() };
        assert_eq!(hex("xchgb", &[byte(RCX), Value::Mem(at)]), "86 08");
        assert_eq!(hex("xchgw", &[word(RCX), Value::Mem(at)]), "66 87 08");
        assert_eq!(hex("xchgl", &[long(RCX), Value::Mem(at)]), "87 08");
        assert_eq!(hex("xchgq", &[quad(RCX), Value::Mem(at)]), "48 87 08");
        assert_eq!(hex("xaddb", &[byte(RCX), Value::Mem(at)]), "0f c0 08");
        assert_eq!(hex("xaddw", &[word(RCX), Value::Mem(at)]), "66 0f c1 08");
        assert_eq!(hex("xaddl", &[long(RCX), Value::Mem(at)]), "0f c1 08");
        assert_eq!(hex("xaddq", &[quad(RCX), Value::Mem(at)]), "48 0f c1 08");
        // The byte form reaching the second half of the register file, for the reason above.
        assert_eq!(hex("xchgb", &[byte(RSI), Value::Mem(at)]), "40 86 30");
    }

    #[test]
    fn a_conversion_puts_its_destination_where_the_arithmetic_puts_its_source() {
        assert_eq!(hex("movzbl", &[byte(RAX), long(RCX)]), "0f b6 c8");
        assert_eq!(hex("movsbq", &[byte(RAX), quad(RCX)]), "48 0f be c8");
        assert_eq!(hex("movslq", &[long(RSI), quad(RAX)]), "48 63 c6");
        assert_eq!(hex("movzwl", &[Value::Reg(RSI, Width::Word), long(RAX)]), "0f b7 c6");
    }

    /// The rows the listing never names, whose bytes are what gas writes for the same lines.
    ///
    /// Every other group here is checked against the manual because the compiler emits it and a
    /// mistake would show up as a program that runs wrong. These are only ever reached by reading
    /// somebody else's file, so the thing worth pinning is that this compiler and gas agree about
    /// what the line means, and the way to get that is to assemble the same lines with gas and
    /// write down what came out. That is what these strings are.
    ///
    /// The accumulator and the shift by one have shorter forms that gas picks and this does not,
    /// so the cases below are written around them: `adcb $200, %cl` rather than `%al`, `rorl $5`
    /// rather than `$1`. Choosing a shorter encoding of the same instruction is a separate job
    /// from having the instruction at all, and it belongs with branch relaxation.
    #[test]
    fn the_instructions_a_hand_written_file_uses_and_a_compiler_never_emits() {
        // The two of the eight that carry the flag, in all three shapes the other six have.
        assert_eq!(hex("adcb", &[byte(RCX), byte(RAX)]), "10 c8");
        assert_eq!(hex("adcw", &[word(RCX), word(RAX)]), "66 11 c8");
        assert_eq!(hex("adcl", &[long(RCX), long(RAX)]), "11 c8");
        assert_eq!(hex("adcq", &[quad(RCX), quad(RAX)]), "48 11 c8");
        assert_eq!(hex("sbbq", &[quad(RCX), quad(RAX)]), "48 19 c8");
        let at = Addr { base: Some(RDX), ..Addr::default() };
        assert_eq!(hex("adcq", &[Value::Mem(at), quad(R8)]), "4c 13 02");
        assert_eq!(hex("sbbb", &[Value::Mem(at), byte(RAX)]), "1a 02");
        assert_eq!(hex("adcq", &[Value::Imm(1), quad(RAX)]), "48 83 d0 01");
        assert_eq!(hex("adcl", &[Value::Imm(1000), long(RCX)]), "81 d1 e8 03 00 00");
        assert_eq!(hex("sbbq", &[Value::Imm(-1), quad(R8)]), "49 83 d8 ff");
        assert_eq!(hex("adcb", &[Value::Imm(200), byte(RCX)]), "80 d1 c8");
        // Adding one and taking one away, which are not an addition of one.
        assert_eq!(hex("incl", &[long(RAX)]), "ff c0");
        assert_eq!(hex("incq", &[quad(RCX)]), "48 ff c1");
        assert_eq!(hex("incw", &[word(RAX)]), "66 ff c0");
        assert_eq!(hex("incb", &[byte(RSI)]), "40 fe c6");
        assert_eq!(hex("decq", &[quad(RCX)]), "48 ff c9");
        // The one operand multiplies, which write two registers and name one.
        assert_eq!(hex("mulq", &[quad(RDX)]), "48 f7 e2");
        assert_eq!(hex("mulb", &[byte(RCX)]), "f6 e1");
        assert_eq!(hex("imulq", &[quad(RDX)]), "48 f7 ea");
        // The rotates, in both shapes the shifts beside them have.
        assert_eq!(hex("rolq", &[Value::Imm(3), quad(RAX)]), "48 c1 c0 03");
        assert_eq!(hex("rorl", &[Value::Imm(5), long(RCX)]), "c1 c9 05");
        assert_eq!(hex("rolb", &[byte(RCX), byte(RDX)]), "d2 c2");
        assert_eq!(hex("rorq", &[byte(RCX), quad(RAX)]), "48 d3 c8");
        // The bit tests, whose immediate form is one opcode and four extensions and whose register
        // form is four opcodes, written the way the arithmetic is.
        assert_eq!(hex("btq", &[Value::Imm(0), quad(R8)]), "49 0f ba e0 00");
        assert_eq!(hex("btsq", &[Value::Imm(63), quad(RAX)]), "48 0f ba e8 3f");
        assert_eq!(hex("btrl", &[Value::Imm(2), long(RCX)]), "0f ba f1 02");
        assert_eq!(hex("btcw", &[Value::Imm(1), word(RDX)]), "66 0f ba fa 01");
        assert_eq!(hex("btq", &[quad(RDX), quad(RAX)]), "48 0f a3 d0");
        assert_eq!(hex("btsl", &[long(RCX), long(RAX)]), "0f ab c8");
        // The bit scans, written the way the conversions are.
        assert_eq!(hex("bsfq", &[quad(RAX), quad(RDX)]), "48 0f bc d0");
        assert_eq!(hex("bsrl", &[long(RAX), long(RCX)]), "0f bd c8");
        // The widenings reading memory, which is the shape only `movzbl` had.
        let from = Addr { base: Some(RSI), ..Addr::default() };
        assert_eq!(hex("movzbq", &[Value::Mem(from), quad(RAX)]), "48 0f b6 06");
        assert_eq!(hex("movzwl", &[Value::Mem(from), long(RAX)]), "0f b7 06");
        assert_eq!(hex("movsbl", &[Value::Mem(from), long(RAX)]), "0f be 06");
        assert_eq!(hex("movslq", &[Value::Mem(from), quad(RAX)]), "48 63 06");
        let along = Addr { base: Some(RSI), disp: 4, ..Addr::default() };
        assert_eq!(hex("movswq", &[Value::Mem(along), quad(RDX)]), "48 0f bf 56 04");
        // The address computation that keeps thirty two bits, which is an addition that leaves the
        // flags alone and is why a hand written file reaches for it.
        let sum = Addr { base: Some(RSI), index: Some(RDX), scale: 2, ..Addr::default() };
        assert_eq!(hex("leal", &[Value::Mem(sum), long(RAX)]), "8d 04 56");
        // The long move under the name a file gives it when it wants the long one.
        assert_eq!(hex("movabsq", &[Value::Imm(-1), quad(RAX)]), "48 b8 ff ff ff ff ff ff ff ff");
        // A push and a pop of memory, which are not the short forms above them.
        let deep = Addr { base: Some(RAX), ..Addr::default() };
        assert_eq!(hex("pushq", &[Value::Mem(deep)]), "ff 30");
        assert_eq!(hex("popq", &[Value::Mem(deep)]), "8f 00");
        // The exchange between two registers, which needs no lock.
        assert_eq!(hex("xchgl", &[long(RCX), long(RDX)]), "87 ca");
        // The scalar move between two vector registers, which this compiler writes as `movaps`.
        assert_eq!(hex("movss", &[Value::Xmm(xmm(0)), Value::Xmm(xmm(1))]), "f3 0f 10 c8");
        assert_eq!(hex("movsd", &[Value::Xmm(xmm(2)), Value::Xmm(xmm(3))]), "f2 0f 10 da");
    }

    /// The rows a loop in GMP needs, whose bytes are again what gas writes for the same lines.
    ///
    /// Same rule as the group above and for the same reason. Every string here came out of
    /// `objdump` on an object gas made, so a disagreement means this compiler reads a line
    /// differently from the assembler every one of these files was written against.
    ///
    /// The accumulator form of the test is the one thing here gas writes shorter, `a9` against
    /// `f7 c0`, and it is the same deferral the group above describes: the table cannot say a row
    /// applies only when the destination is `rax`, so having that form at all needs a way to
    /// constrain a row by register and that goes in with relaxation.
    #[test]
    fn the_instructions_a_hand_written_loop_writes_into_memory() {
        // The eight with a register read and an address written, which is the opcode the register
        // form uses with the addressing byte naming memory instead.
        let at = Addr { base: Some(RDI), ..Addr::default() };
        assert_eq!(hex("addq", &[quad(RAX), Value::Mem(at)]), "48 01 07");
        assert_eq!(hex("addb", &[byte(RAX), Value::Mem(at)]), "00 07");
        assert_eq!(hex("addw", &[word(RAX), Value::Mem(at)]), "66 01 07");
        assert_eq!(hex("orq", &[quad(R8), Value::Mem(at)]), "4c 09 07");
        assert_eq!(hex("sbbq", &[quad(RAX), Value::Mem(at)]), "48 19 07");
        assert_eq!(hex("andq", &[quad(RAX), Value::Mem(at)]), "48 21 07");
        assert_eq!(hex("subq", &[quad(RAX), Value::Mem(at)]), "48 29 07");
        assert_eq!(hex("xorq", &[quad(RAX), Value::Mem(at)]), "48 31 07");
        assert_eq!(hex("cmpq", &[quad(RAX), Value::Mem(at)]), "48 39 07");
        assert_eq!(hex("cmpq", &[Value::Mem(at), quad(RAX)]), "48 3b 07");
        let indexed = Addr { base: Some(RDI), index: Some(R8), scale: 8, disp: 8, ..at };
        assert_eq!(hex("adcq", &[quad(R15), Value::Mem(indexed)]), "4e 11 7c c7 08");
        // The same eight with a number written instead of a register, which narrows the way the
        // register with immediate rows above it do.
        assert_eq!(hex("addq", &[Value::Imm(1), Value::Mem(at)]), "48 83 07 01");
        assert_eq!(hex("addq", &[Value::Imm(1000), Value::Mem(at)]), "48 81 07 e8 03 00 00");
        assert_eq!(hex("addl", &[Value::Imm(1), Value::Mem(at)]), "83 07 01");
        assert_eq!(hex("addw", &[Value::Imm(1), Value::Mem(at)]), "66 83 07 01");
        assert_eq!(hex("addb", &[Value::Imm(1), Value::Mem(at)]), "80 07 01");
        assert_eq!(hex("subq", &[Value::Imm(8), Value::Mem(at)]), "48 83 2f 08");
        assert_eq!(hex("cmpq", &[Value::Imm(0), Value::Mem(at)]), "48 83 3f 00");
        assert_eq!(hex("andl", &[Value::Imm(255), Value::Mem(at)]), "81 27 ff 00 00 00");
        // The test at the widths the compiler never asks for and against a number, whose immediate
        // is the full width however small it is.
        assert_eq!(hex("testq", &[quad(RAX), quad(RAX)]), "48 85 c0");
        assert_eq!(hex("testl", &[long(RAX), long(RAX)]), "85 c0");
        assert_eq!(hex("testw", &[word(RAX), word(RAX)]), "66 85 c0");
        assert_eq!(hex("testq", &[quad(RAX), Value::Mem(at)]), "48 85 07");
        assert_eq!(hex("testq", &[Value::Mem(at), quad(RAX)]), "48 85 07");
        assert_eq!(hex("testq", &[Value::Imm(1), quad(R10)]), "49 f7 c2 01 00 00 00");
        assert_eq!(hex("testq", &[Value::Imm(1), Value::Mem(at)]), "48 f7 07 01 00 00 00");
        assert_eq!(hex("testb", &[Value::Imm(1), byte(RCX)]), "f6 c1 01");
        // The one operand group naming an address, which is the same digit column again.
        let sixteen = Addr { base: Some(R8), disp: 16, ..Addr::default() };
        assert_eq!(hex("mulq", &[Value::Mem(sixteen)]), "49 f7 60 10");
        assert_eq!(hex("mull", &[Value::Mem(at)]), "f7 27");
        assert_eq!(hex("imulq", &[Value::Mem(at)]), "48 f7 2f");
        assert_eq!(hex("divq", &[Value::Mem(at)]), "48 f7 37");
        assert_eq!(hex("idivq", &[Value::Mem(at)]), "48 f7 3f");
        assert_eq!(hex("negq", &[Value::Mem(at)]), "48 f7 1f");
        assert_eq!(hex("notq", &[Value::Mem(at)]), "48 f7 17");
        assert_eq!(hex("incq", &[Value::Mem(at)]), "48 ff 07");
        assert_eq!(hex("decq", &[Value::Mem(at)]), "48 ff 0f");
        assert_eq!(hex("incb", &[Value::Mem(at)]), "fe 07");
        let four = Addr { base: Some(RSI), disp: 4, ..Addr::default() };
        assert_eq!(hex("decl", &[Value::Mem(four)]), "ff 4e 04");
    }

    /// The shifts and rotates a multiple precision loop is built out of.
    #[test]
    fn the_shifts_a_number_wider_than_a_register_needs() {
        // The two that go through the carry, which is what halving a wide number is done with.
        assert_eq!(hex("rclq", &[Value::Imm(3), quad(RAX)]), "48 c1 d0 03");
        assert_eq!(hex("rcrq", &[Value::Imm(3), quad(RAX)]), "48 c1 d8 03");
        assert_eq!(hex("rclq", &[byte(RCX), quad(RAX)]), "48 d3 d0");
        assert_eq!(hex("rcrq", &[byte(RCX), quad(RAX)]), "48 d3 d8");
        // All eight by one place, which is a third opcode and not the constant one written short.
        assert_eq!(hex("rolq", &[quad(RAX)]), "48 d1 c0");
        assert_eq!(hex("rorq", &[quad(RAX)]), "48 d1 c8");
        assert_eq!(hex("rclq", &[quad(RAX)]), "48 d1 d0");
        assert_eq!(hex("rcrq", &[quad(RAX)]), "48 d1 d8");
        assert_eq!(hex("shlq", &[quad(RAX)]), "48 d1 e0");
        assert_eq!(hex("shrq", &[quad(RAX)]), "48 d1 e8");
        assert_eq!(hex("sarq", &[quad(RAX)]), "48 d1 f8");
        assert_eq!(hex("shrl", &[long(RAX)]), "d1 e8");
        assert_eq!(hex("shrb", &[byte(RAX)]), "d0 e8");
        assert_eq!(hex("shrw", &[word(RAX)]), "66 d1 e8");
        // The two that name a register to shift in from, whose addressing byte names the argument
        // at the end and holds the middle one beside it.
        let five = Value::Imm(5);
        assert_eq!(hex("shldq", &[five, quad(RSI), quad(RDI)]), "48 0f a4 f7 05");
        assert_eq!(hex("shrdq", &[five, quad(RSI), quad(RDI)]), "48 0f ac f7 05");
        assert_eq!(hex("shldl", &[five, long(RSI), long(RDI)]), "0f a4 f7 05");
        assert_eq!(hex("shldq", &[byte(RCX), quad(RSI), quad(RDI)]), "48 0f a5 f7");
        assert_eq!(hex("shrdq", &[byte(RCX), quad(RSI), quad(RDI)]), "48 0f ad f7");
        assert_eq!(hex("shldw", &[byte(RCX), word(RSI), word(RDI)]), "66 0f a5 f7");
    }

    /// The six conditions a C expression has no way to ask about, and the move that reads memory.
    #[test]
    fn the_conditions_a_compiler_never_reaches_and_the_move_that_reads_memory() {
        assert_eq!(hex("cmovsq", &[quad(RAX), quad(RBX)]), "48 0f 48 d8");
        assert_eq!(hex("cmovnsq", &[quad(RAX), quad(RBX)]), "48 0f 49 d8");
        assert_eq!(hex("cmovoq", &[quad(RAX), quad(RBX)]), "48 0f 40 d8");
        assert_eq!(hex("cmovnoq", &[quad(RAX), quad(RBX)]), "48 0f 41 d8");
        assert_eq!(hex("cmovpq", &[quad(RAX), quad(RBX)]), "48 0f 4a d8");
        assert_eq!(hex("cmovnpq", &[quad(RAX), quad(RBX)]), "48 0f 4b d8");
        assert_eq!(hex("cmovsl", &[long(RAX), long(RBX)]), "0f 48 d8");
        let at = Addr { base: Some(R8), disp: 16, ..Addr::default() };
        assert_eq!(hex("cmovbq", &[Value::Mem(at), quad(RBX)]), "49 0f 42 58 10");
        let plain = Addr { base: Some(RDI), ..Addr::default() };
        assert_eq!(hex("cmoveq", &[Value::Mem(plain), quad(RAX)]), "48 0f 44 07");
        assert_eq!(hex("cmovnsw", &[Value::Mem(plain), word(RAX)]), "66 0f 49 07");
        let four = Addr { base: Some(RSI), disp: 4, ..Addr::default() };
        assert_eq!(hex("cmovgl", &[Value::Mem(four), long(RDX)]), "0f 4f 56 04");
        assert_eq!(hex("seto", &[byte(RAX)]), "0f 90 c0");
        assert_eq!(hex("setno", &[byte(RAX)]), "0f 91 c0");
        assert_eq!(hex("sets", &[byte(RAX)]), "0f 98 c0");
        assert_eq!(hex("setns", &[byte(RBX)]), "0f 99 c3");
        // The six branches that complete the sixteen, whose four bytes of distance are a hole.
        for (mnemonic, opcode) in
            [("jo", 0x80), ("jno", 0x81), ("js", 0x88), ("jns", 0x89), ("jp", 0x8A), ("jnp", 0x8B)]
        {
            let mut out = Vec::new();
            let holes = encode(mnemonic, &[Value::Dest], &mut out).expect(mnemonic);
            assert_eq!(out, vec![0x0F, opcode, 0, 0, 0, 0], "{mnemonic}");
            assert_eq!(holes.dest, Some(2), "{mnemonic}");
        }
    }

    /// The three instructions that touch nothing but the carry, and the copy that steps two
    /// registers, which is one instruction until a repeat prefix makes it a whole loop.
    #[test]
    fn the_flag_and_the_string_instructions_a_compiler_has_no_use_for() {
        assert_eq!(hex("clc", &[]), "f8");
        assert_eq!(hex("stc", &[]), "f9");
        assert_eq!(hex("cmc", &[]), "f5");
        assert_eq!(hex("movsb", &[]), "a4");
        assert_eq!(hex("movsw", &[]), "66 a5");
        assert_eq!(hex("movsl", &[]), "a5");
        assert_eq!(hex("movsq", &[]), "48 a5");
    }

    #[test]
    fn the_one_branch_with_nowhere_but_a_byte_to_say_where_it_goes() {
        // `jrcxz` has no long form, so the byte it leaves is the whole of its reach and a caller
        // that assumed four would write over the instruction behind it. The hole says where, and
        // how wide it is is the row's business rather than the caller's.
        let mut out = Vec::new();
        let holes = encode("jrcxz", &[Value::Dest], &mut out).expect("a jump on a register");
        assert_eq!(out, vec![0xE3, 0x00], "the opcode and the room for the distance");
        assert_eq!(holes.dest, Some(1), "the distance goes in the byte behind the opcode");
        let row = encoding("jrcxz", &[Kind::Dest], 0).expect("the row it was encoded from");
        assert_eq!(row.imm, ImmSize::Cb, "the row is what says how much room there is");
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
