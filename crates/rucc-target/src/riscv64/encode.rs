//! Encoding one RISC-V instruction as the words the machine runs.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1, and the RISC-V unprivileged specification
//! for RV64G, which is the base integer set with the M, A, F and D extensions.
//!
//! What this takes is what GNU as takes: a mnemonic and its operands in the order the text writes
//! them, including the pseudo instructions the listing uses, such as `li`, `mv`, `call` and `beqz`.
//! A pseudo is the words GNU as writes for it, which is more than one for a constant that does not
//! fit twelve bits, for the address of a symbol and for a call. Nothing here writes the compressed
//! forms. An instruction is always four bytes, which is what keeps a distance inside an opcode the
//! same number of words in the object as in the listing.
//!
//! A symbol is left as zeros and a [`Fixup`] that says which bits it goes in, at the word of the
//! instruction it belongs to. None of them is ever marked for linker relaxation, since a listing
//! that the linker may shorten is one whose distances the compiler no longer knows.

use std::fmt;

/// Which part of a symbol's address an operand asks for, spelled in the text as `%lo()` and the
/// like around the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// The address itself, or the distance to it, which is what a bare name means.
    Plain,
    /// The upper twenty bits of the address, rounded so the low twelve can be added as a signed
    /// number, `%hi()`.
    Hi,
    /// The low twelve bits of the address, `%lo()`.
    Lo,
    /// The upper twenty bits of the offset from the thread pointer, `%tprel_hi()`.
    TprelHi,
    /// The low twelve bits of it, `%tprel_lo()`.
    TprelLo,
    /// The mark on the `add` of the thread pointer, `%tprel_add()`, which only tells the linker
    /// which instruction it is.
    TprelAdd,
}

/// What is added to the base register of an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offset {
    /// A number, which the machine carries in twelve signed bits.
    Imm(i64),
    /// Part of a symbol's address, which is left for a relocation to fill in.
    Symbol(Operator),
}

/// An address, which on this machine is a register and something added to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Addr {
    /// The register the address starts from.
    pub base: u8,
    /// What is added to it.
    pub offset: Offset,
}

/// How a floating point instruction rounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Round {
    /// To nearest, ties to even, `rne`.
    Rne,
    /// Toward zero, `rtz`, which is how C converts a float to an integer.
    Rtz,
    /// Down, `rdn`.
    Rdn,
    /// Up, `rup`.
    Rup,
    /// To nearest, ties away from zero, `rmm`.
    Rmm,
    /// Whatever the rounding mode register says, `dyn`.
    Dyn,
}

impl Round {
    /// The three bits the encoding gives it.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Round::Rne => 0,
            Round::Rtz => 1,
            Round::Rdn => 2,
            Round::Rup => 3,
            Round::Rmm => 4,
            Round::Dyn => 7,
        }
    }

    /// The name the text gives it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Round::Rne => "rne",
            Round::Rtz => "rtz",
            Round::Rdn => "rdn",
            Round::Rup => "rup",
            Round::Rmm => "rmm",
            Round::Dyn => "dyn",
        }
    }
}

/// What one operand of an instruction is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    /// An integer register, where zero is the one that reads as zero.
    Gpr(u8),
    /// A floating point register.
    Fpr(u8),
    /// A number.
    Imm(i64),
    /// An address in memory.
    Mem(Addr),
    /// A symbol, or part of its address, which is left for a relocation.
    Symbol(Operator),
    /// How a floating point instruction rounds, when the text says.
    Round(Round),
    /// Which accesses one side of a `fence` orders, as the four bits the encoding gives them:
    /// eight for device input, four for device output, two for reads and one for writes.
    Order(u8),
}

/// What a linker fills in, named after the `R_RISCV_` relocations without that in front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixup {
    /// The distance a conditional branch goes, in twelve bits of two byte steps.
    Branch,
    /// The distance `jal` goes, in twenty bits of two byte steps.
    Jal,
    /// The distance to what `call` or `tail` reaches, across the `auipc` and the `jalr` after it.
    CallPlt,
    /// The upper twenty bits of the distance to a symbol, in an `auipc`.
    PcrelHi20,
    /// The low twelve bits of that distance, in the `addi` or load after the `auipc`.
    PcrelLo12I,
    /// The same in a store.
    PcrelLo12S,
    /// The upper twenty bits of the distance to the symbol's global offset table entry.
    GotHi20,
    /// The upper twenty bits of the distance to the global offset table entry that holds a
    /// thread-local variable's offset from the thread pointer, which is `la.tls.ie`.
    TlsGotHi20,
    /// The upper twenty bits of the address, in a `lui`.
    Hi20,
    /// The low twelve bits of the address, in an `addi` or a load.
    Lo12I,
    /// The same in a store.
    Lo12S,
    /// The upper twenty bits of the offset from the thread pointer.
    TprelHi20,
    /// The low twelve bits of it, in an `addi` or a load.
    TprelLo12I,
    /// The same in a store.
    TprelLo12S,
    /// The `add` of the thread pointer, which carries no bits of anything.
    TprelAdd,
}

impl Fixup {
    /// The ELF relocation number the psABI gives it.
    #[must_use]
    pub const fn elf(self) -> u32 {
        match self {
            Fixup::Branch => 16,
            Fixup::Jal => 17,
            Fixup::CallPlt => 19,
            Fixup::GotHi20 => 20,
            Fixup::TlsGotHi20 => 21,
            Fixup::PcrelHi20 => 23,
            Fixup::PcrelLo12I => 24,
            Fixup::PcrelLo12S => 25,
            Fixup::Hi20 => 26,
            Fixup::Lo12I => 27,
            Fixup::Lo12S => 28,
            Fixup::TprelHi20 => 29,
            Fixup::TprelLo12I => 30,
            Fixup::TprelLo12S => 31,
            Fixup::TprelAdd => 32,
        }
    }

    /// The relocation's name, as readelf prints it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Fixup::Branch => "R_RISCV_BRANCH",
            Fixup::Jal => "R_RISCV_JAL",
            Fixup::CallPlt => "R_RISCV_CALL_PLT",
            Fixup::GotHi20 => "R_RISCV_GOT_HI20",
            Fixup::TlsGotHi20 => "R_RISCV_TLS_GOT_HI20",
            Fixup::PcrelHi20 => "R_RISCV_PCREL_HI20",
            Fixup::PcrelLo12I => "R_RISCV_PCREL_LO12_I",
            Fixup::PcrelLo12S => "R_RISCV_PCREL_LO12_S",
            Fixup::Hi20 => "R_RISCV_HI20",
            Fixup::Lo12I => "R_RISCV_LO12_I",
            Fixup::Lo12S => "R_RISCV_LO12_S",
            Fixup::TprelHi20 => "R_RISCV_TPREL_HI20",
            Fixup::TprelLo12I => "R_RISCV_TPREL_LO12_I",
            Fixup::TprelLo12S => "R_RISCV_TPREL_LO12_S",
            Fixup::TprelAdd => "R_RISCV_TPREL_ADD",
        }
    }

    /// Whether it reaches a thread-local variable, which makes the name it reaches one.
    #[must_use]
    pub const fn thread(self) -> bool {
        matches!(
            self,
            Fixup::TlsGotHi20
                | Fixup::TprelHi20
                | Fixup::TprelLo12I
                | Fixup::TprelLo12S
                | Fixup::TprelAdd
        )
    }

    /// Whether it is a distance from the instruction it is in to somewhere in the same section,
    /// which an assembler can work out on its own.
    ///
    /// Only the branches and `jal`. The `auipc` pairs are distances too, but gas leaves them to
    /// the linker however near the name is, and so does this.
    #[must_use]
    pub const fn relative(self) -> bool {
        matches!(self, Fixup::Branch | Fixup::Jal)
    }

    /// The word with the distance filled in, or `None` when it is odd, does not fit, or the
    /// fixup is not one [`Fixup::relative`] says is worked out here.
    #[must_use]
    pub const fn apply(self, word: u32, value: i64) -> Option<u32> {
        if value % 2 != 0 {
            return None;
        }
        // The opcode bits are the only ones the two forms put anything in when every register is
        // zero, so masking them off leaves just where the distance goes.
        match self {
            Fixup::Branch if fits(value, 13) => Some(word | (b(0, 0, 0, value) & !0x7f)),
            Fixup::Jal if fits(value, 21) => Some(word | (j(0, value) & !0x7f)),
            _ => None,
        }
    }
}

/// One instruction as words, and what is left to fill in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    /// The words, with zeros where a symbol's address or a label's distance goes.
    pub words: Vec<u32>,
    /// How to fill those in, each with the index of the word it is at. A `%pcrel_lo` belongs to
    /// the `auipc` in front of it, which is the word before.
    pub fixups: Vec<(usize, Fixup)>,
}

/// Why an instruction could not be encoded.
///
/// Each is a bug in whatever produced the instruction rather than anything a C program could ask
/// for, so they say which instruction it was and not much more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Nothing here encodes that mnemonic with those operands.
    Unwritten {
        /// The mnemonic that was asked for.
        mnemonic: String,
        /// How many operands it was given.
        operands: usize,
    },
    /// A number that the instruction cannot carry.
    Immediate {
        /// The mnemonic that was asked for.
        mnemonic: String,
        /// The number.
        imm: i64,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unwritten { mnemonic, operands } => {
                write!(f, "no encoding for {mnemonic} with {operands} operands of those kinds")
            }
            Error::Immediate { mnemonic, imm } => {
                write!(f, "{mnemonic} cannot carry the immediate {imm}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Whether a number fits in that many bits as a signed number.
#[must_use]
pub const fn fits(imm: i64, bits: u32) -> bool {
    let half = 1i64 << (bits - 1);
    -half <= imm && imm < half
}

/// The low twelve bits of a number, read back as a signed number, which is what an `addi` after
/// a `lui` or an `auipc` adds.
const fn low12(imm: i64) -> i64 {
    ((imm & 0xfff) ^ 0x800) - 0x800
}

/// The opcode, the `funct3` and the `funct7` of the register forms.
fn alu(mnemonic: &str) -> Option<(u32, u32, u32)> {
    Some(match mnemonic {
        "add" => (0x33, 0, 0),
        "sub" => (0x33, 0, 0x20),
        "sll" => (0x33, 1, 0),
        "slt" => (0x33, 2, 0),
        "sltu" => (0x33, 3, 0),
        "xor" => (0x33, 4, 0),
        "srl" => (0x33, 5, 0),
        "sra" => (0x33, 5, 0x20),
        "or" => (0x33, 6, 0),
        "and" => (0x33, 7, 0),
        "mul" => (0x33, 0, 1),
        "mulh" => (0x33, 1, 1),
        "mulhsu" => (0x33, 2, 1),
        "mulhu" => (0x33, 3, 1),
        "div" => (0x33, 4, 1),
        "divu" => (0x33, 5, 1),
        "rem" => (0x33, 6, 1),
        "remu" => (0x33, 7, 1),
        "addw" => (0x3b, 0, 0),
        "subw" => (0x3b, 0, 0x20),
        "sllw" => (0x3b, 1, 0),
        "srlw" => (0x3b, 5, 0),
        "sraw" => (0x3b, 5, 0x20),
        "mulw" => (0x3b, 0, 1),
        "divw" => (0x3b, 4, 1),
        "divuw" => (0x3b, 5, 1),
        "remw" => (0x3b, 6, 1),
        "remuw" => (0x3b, 7, 1),
        _ => return None,
    })
}

/// The opcode and the `funct3` of the forms with a twelve bit constant.
fn alu_imm(mnemonic: &str) -> Option<(u32, u32)> {
    Some(match mnemonic {
        "addi" => (0x13, 0),
        "slti" => (0x13, 2),
        "sltiu" => (0x13, 3),
        "xori" => (0x13, 4),
        "ori" => (0x13, 6),
        "andi" => (0x13, 7),
        "addiw" => (0x1b, 0),
        _ => return None,
    })
}

/// The opcode, the `funct3`, the bits above the count, and how many bits the count has, of the
/// shifts by a constant.
fn shift_imm(mnemonic: &str) -> Option<(u32, u32, u32, u32)> {
    Some(match mnemonic {
        "slli" => (0x13, 1, 0, 6),
        "srli" => (0x13, 5, 0, 6),
        "srai" => (0x13, 5, 0x10 << 6, 6),
        "slliw" => (0x1b, 1, 0, 5),
        "srliw" => (0x1b, 5, 0, 5),
        "sraiw" => (0x1b, 5, 0x20 << 5, 5),
        _ => return None,
    })
}

/// The opcode and the `funct3` of the loads, which take an integer or a floating point register.
fn load(mnemonic: &str) -> Option<(u32, u32, bool)> {
    Some(match mnemonic {
        "lb" => (0x03, 0, false),
        "lh" => (0x03, 1, false),
        "lw" => (0x03, 2, false),
        "ld" => (0x03, 3, false),
        "lbu" => (0x03, 4, false),
        "lhu" => (0x03, 5, false),
        "lwu" => (0x03, 6, false),
        "flw" => (0x07, 2, true),
        "fld" => (0x07, 3, true),
        _ => return None,
    })
}

/// The same for the stores.
fn store(mnemonic: &str) -> Option<(u32, u32, bool)> {
    Some(match mnemonic {
        "sb" => (0x23, 0, false),
        "sh" => (0x23, 1, false),
        "sw" => (0x23, 2, false),
        "sd" => (0x23, 3, false),
        "fsw" => (0x27, 2, true),
        "fsd" => (0x27, 3, true),
        _ => return None,
    })
}

/// The `funct3` of the conditional branches.
fn branch(mnemonic: &str) -> Option<u32> {
    Some(match mnemonic {
        "beq" => 0,
        "bne" => 1,
        "blt" => 4,
        "bge" => 5,
        "bltu" => 6,
        "bgeu" => 7,
        _ => return None,
    })
}

/// The `funct5`, the `funct3` and the ordering bits of the A extension's instructions, which are
/// `lr`, `sc` and the `amo` operations, at `.w` or `.d`, with `.aq`, `.rl` or `.aqrl` after it.
fn atomic(mnemonic: &str) -> Option<(u32, u32, u32)> {
    let mut parts = mnemonic.split('.');
    let f5 = match parts.next()? {
        "lr" => 0x02,
        "sc" => 0x03,
        "amoswap" => 0x01,
        "amoadd" => 0x00,
        "amoxor" => 0x04,
        "amoand" => 0x0c,
        "amoor" => 0x08,
        "amomin" => 0x10,
        "amomax" => 0x14,
        "amominu" => 0x18,
        "amomaxu" => 0x1c,
        _ => return None,
    };
    let f3 = match parts.next()? {
        "w" => 2,
        "d" => 3,
        _ => return None,
    };
    let order = match parts.next() {
        None => 0,
        Some("aq") => 2,
        Some("rl") => 1,
        Some("aqrl") => 3,
        Some(_) => return None,
    };
    parts.next().is_none().then_some((f5, f3, order))
}

/// The `funct3` of the CSR instructions that take a register, `csrrw`, `csrrs` and `csrrc`, and
/// of the pseudo instructions that write those with `zero` as the destination. The form that
/// takes a five bit constant instead is four more.
fn csr(mnemonic: &str) -> Option<u32> {
    Some(match mnemonic {
        "csrrw" | "csrw" | "csrrwi" | "csrwi" => 1,
        "csrrs" | "csrs" | "csrrsi" | "csrsi" => 2,
        "csrrc" | "csrc" | "csrrci" | "csrci" => 3,
        _ => return None,
    })
}

const fn r(op: u32, f3: u32, f7: u32, rd: u8, rs1: u8, rs2: u8) -> u32 {
    (f7 << 25) | ((rs2 as u32) << 20) | ((rs1 as u32) << 15) | (f3 << 12) | ((rd as u32) << 7) | op
}

const fn i(op: u32, f3: u32, rd: u8, rs1: u8, imm: i64) -> u32 {
    (((imm as u32) & 0xfff) << 20) | ((rs1 as u32) << 15) | (f3 << 12) | ((rd as u32) << 7) | op
}

const fn s(op: u32, f3: u32, rs1: u8, rs2: u8, imm: i64) -> u32 {
    let imm = imm as u32;
    (((imm >> 5) & 0x7f) << 25)
        | ((rs2 as u32) << 20)
        | ((rs1 as u32) << 15)
        | (f3 << 12)
        | ((imm & 0x1f) << 7)
        | op
}

const fn b(f3: u32, rs1: u8, rs2: u8, imm: i64) -> u32 {
    let imm = imm as u32;
    (((imm >> 12) & 1) << 31)
        | (((imm >> 5) & 0x3f) << 25)
        | ((rs2 as u32) << 20)
        | ((rs1 as u32) << 15)
        | (f3 << 12)
        | (((imm >> 1) & 0xf) << 8)
        | (((imm >> 11) & 1) << 7)
        | 0x63
}

const fn u(op: u32, rd: u8, imm20: i64) -> u32 {
    (((imm20 as u32) & 0xfffff) << 12) | ((rd as u32) << 7) | op
}

const fn j(rd: u8, imm: i64) -> u32 {
    let imm = imm as u32;
    (((imm >> 20) & 1) << 31)
        | (((imm >> 1) & 0x3ff) << 21)
        | (((imm >> 11) & 1) << 20)
        | (((imm >> 12) & 0xff) << 12)
        | ((rd as u32) << 7)
        | 0x6f
}

/// The return address register, the temporary `tail` goes through, and the zero register.
const RA: u8 = 1;
const T1: u8 = 6;
const ZERO: u8 = 0;

/// The scratch register a far constant goes through, `t5`.
pub const FAR: u8 = 30;

/// The words `li` writes for a constant, which are the ones GNU as writes for it in the cases a
/// program usually has: one `addi` for twelve bits, a `lui` and an `addiw` for thirty two, and
/// for anything wider the upper part built the same way, shifted up and the low twelve bits added.
/// An upper part of twelve bits is an `addiw`, which is what gas builds it with.
fn li(rd: u8, imm: i64, words: &mut Vec<u32>) {
    build(rd, imm, false, words);
}

/// The words for `li`, where `upper` says this is the upper part of a wider constant.
fn build(rd: u8, imm: i64, upper: bool, words: &mut Vec<u32>) {
    if let Ok(small) = i32::try_from(imm) {
        let lo = low12(imm);
        let hi = (i64::from(small) - lo) >> 12;
        if hi == 0 {
            words.push(i(if upper { 0x1b } else { 0x13 }, 0, rd, ZERO, lo));
        } else {
            words.push(u(0x37, rd, hi));
            if lo != 0 {
                words.push(i(0x1b, 0, rd, rd, lo));
            }
        }
        return;
    }
    // The arithmetic is modulo two to the sixty four, the same as the machine's, so a constant
    // near either end that wraps on the way still comes out as itself.
    let lo = low12(imm);
    let hi = imm.wrapping_sub(lo) >> 12;
    let shift = 12 + hi.trailing_zeros();
    build(rd, hi >> (shift - 12), true, words);
    words.push(i(0x13, 1, rd, rd, i64::from(shift)));
    if lo != 0 {
        words.push(i(0x13, 0, rd, rd, lo));
    }
}

/// How many words `li` writes for a constant.
#[must_use]
pub fn li_words(imm: i64) -> usize {
    let mut words = Vec::new();
    li(0, imm, &mut words);
    words.len()
}

/// The format bits of a floating point mnemonic's suffix, zero for single and one for double.
fn format(suffix: &str) -> Option<u32> {
    match suffix {
        "s" => Some(0),
        "d" => Some(1),
        _ => None,
    }
}

/// The two bits a conversion gives an integer type, `w`, `wu`, `l` and `lu`.
fn integer(suffix: &str) -> Option<u32> {
    match suffix {
        "w" => Some(0),
        "wu" => Some(1),
        "l" => Some(2),
        "lu" => Some(3),
        _ => None,
    }
}

/// Writes one instruction.
///
/// # Errors
///
/// When nothing here encodes that mnemonic with those operands and when a number does not fit
/// the instruction. See [`Error`].
pub fn encode(mnemonic: &str, values: &[Value]) -> Result<Encoded, Error> {
    let mut out = Encoded { words: Vec::new(), fixups: Vec::new() };
    let unwritten = || Error::Unwritten { mnemonic: mnemonic.to_owned(), operands: values.len() };
    let immediate = |imm| Error::Immediate { mnemonic: mnemonic.to_owned(), imm };
    let twelve = |imm: i64| if fits(imm, 12) { Ok(imm) } else { Err(immediate(imm)) };
    use Value::{Fpr, Gpr, Imm, Mem, Symbol};

    if let Some((op, f3, f7)) = alu(mnemonic) {
        let [Gpr(rd), Gpr(rs1), Gpr(rs2)] = *values else { return Err(unwritten()) };
        out.words.push(r(op, f3, f7, rd, rs1, rs2));
        return Ok(out);
    }
    if let Some((op, f3)) = alu_imm(mnemonic) {
        let [Gpr(rd), Gpr(rs1), Imm(imm)] = *values else { return Err(unwritten()) };
        out.words.push(i(op, f3, rd, rs1, twelve(imm)?));
        return Ok(out);
    }
    if let Some((op, f3, high, bits)) = shift_imm(mnemonic) {
        let [Gpr(rd), Gpr(rs1), Imm(imm)] = *values else { return Err(unwritten()) };
        if !(0..1 << bits).contains(&imm) {
            return Err(immediate(imm));
        }
        out.words.push(i(op, f3, rd, rs1, i64::from(high) | imm));
        return Ok(out);
    }
    if let Some((op, f3, fp)) = load(mnemonic) {
        let (rd, addr) = match *values {
            [Gpr(rd), Mem(addr)] if !fp => (rd, addr),
            [Fpr(rd), Mem(addr)] if fp => (rd, addr),
            _ => return Err(unwritten()),
        };
        let imm = match addr.offset {
            Offset::Imm(imm) => twelve(imm)?,
            Offset::Symbol(Operator::Lo) => {
                out.fixups.push((0, Fixup::Lo12I));
                0
            }
            Offset::Symbol(Operator::TprelLo) => {
                out.fixups.push((0, Fixup::TprelLo12I));
                0
            }
            Offset::Symbol(_) => return Err(unwritten()),
        };
        out.words.push(i(op, f3, rd, addr.base, imm));
        return Ok(out);
    }
    if let Some((op, f3, fp)) = store(mnemonic) {
        let (rs2, addr) = match *values {
            [Gpr(rs2), Mem(addr)] if !fp => (rs2, addr),
            [Fpr(rs2), Mem(addr)] if fp => (rs2, addr),
            _ => return Err(unwritten()),
        };
        let imm = match addr.offset {
            Offset::Imm(imm) => twelve(imm)?,
            Offset::Symbol(Operator::Lo) => {
                out.fixups.push((0, Fixup::Lo12S));
                0
            }
            Offset::Symbol(Operator::TprelLo) => {
                out.fixups.push((0, Fixup::TprelLo12S));
                0
            }
            Offset::Symbol(_) => return Err(unwritten()),
        };
        out.words.push(s(op, f3, addr.base, rs2, imm));
        return Ok(out);
    }
    if let Some(f3) = branch(mnemonic) {
        let [Gpr(rs1), Gpr(rs2), ref to] = *values else { return Err(unwritten()) };
        let imm = distance(to, 13, &mut out, Fixup::Branch).ok_or_else(unwritten)??;
        out.words.push(b(f3, rs1, rs2, imm));
        return Ok(out);
    }
    if mnemonic.starts_with('f') {
        if let Some(()) = float(mnemonic, values, &mut out).transpose()? {
            return Ok(out);
        }
    }

    if let Some((f5, f3, order)) = atomic(mnemonic) {
        let (rd, rs2, base) = match *values {
            [Gpr(rd), Mem(Addr { base, offset: Offset::Imm(0) })] if f5 == 0x02 => (rd, ZERO, base),
            [Gpr(rd), Gpr(rs2), Mem(Addr { base, offset: Offset::Imm(0) })] if f5 != 0x02 => {
                (rd, rs2, base)
            }
            _ => return Err(unwritten()),
        };
        out.words.push(r(0x2f, f3, f5 << 2 | order, rd, base, rs2));
        return Ok(out);
    }
    if let Some(f3) = csr(mnemonic) {
        // The register form, or the constant form when the source is a number, which is how GNU
        // as reads `csrs sstatus, 2` as `csrsi`. The pseudo instructions have no destination,
        // and `csrr` is `csrrs` from `zero`.
        let (rd, number, source) = match *values {
            [Gpr(rd), Imm(number), source] if mnemonic.starts_with("csrr") => (rd, number, source),
            [Imm(number), source] if !mnemonic.starts_with("csrr") => (ZERO, number, source),
            _ => return Err(unwritten()),
        };
        if !(0..1 << 12).contains(&number) {
            return Err(immediate(number));
        }
        let (f3, rs1) = match source {
            Gpr(rs1) if !mnemonic.ends_with('i') => (f3, rs1),
            Imm(imm) if (0..32).contains(&imm) => (f3 | 4, imm as u8),
            Imm(imm) => return Err(immediate(imm)),
            _ => return Err(unwritten()),
        };
        out.words.push(i(0x73, f3, rd, rs1, number));
        return Ok(out);
    }

    match (mnemonic, values) {
        ("csrr", [Gpr(rd), Imm(number)]) if (0..1 << 12).contains(number) => {
            out.words.push(i(0x73, 2, *rd, ZERO, *number));
        }
        // The counters, which are `csrr` of `cycle`, `time` and `instret`.
        ("rdcycle", [Gpr(rd)]) => out.words.push(i(0x73, 2, *rd, ZERO, 0xc00)),
        ("rdtime", [Gpr(rd)]) => out.words.push(i(0x73, 2, *rd, ZERO, 0xc01)),
        ("rdinstret", [Gpr(rd)]) => out.words.push(i(0x73, 2, *rd, ZERO, 0xc02)),
        ("fence.i", []) => out.words.push(0x0000_100f),
        ("fence.tso", []) => out.words.push(0x8330_000f),
        ("pause", []) => out.words.push(0x0100_000f),
        ("wfi", []) => out.words.push(0x1050_0073),
        ("sret", []) => out.words.push(0x1020_0073),
        ("mret", []) => out.words.push(0x3020_0073),
        ("sfence.vma", []) => out.words.push(0x1200_0073),
        ("sfence.vma", [Gpr(rs1)]) => out.words.push(r(0x73, 0, 0x09, ZERO, *rs1, ZERO)),
        ("sfence.vma", [Gpr(rs1), Gpr(rs2)]) => out.words.push(r(0x73, 0, 0x09, ZERO, *rs1, *rs2)),
        // The branches GNU as has as the others with their operands swapped or against `zero`.
        ("bgt" | "ble" | "bgtu" | "bleu", [Gpr(rs1), Gpr(rs2), to]) => {
            let f3 = match mnemonic {
                "bgt" => 4,
                "ble" => 5,
                "bgtu" => 6,
                _ => 7,
            };
            let imm = distance(to, 13, &mut out, Fixup::Branch).ok_or_else(unwritten)??;
            out.words.push(b(f3, *rs2, *rs1, imm));
        }
        ("blez" | "bgez" | "bltz" | "bgtz", [Gpr(rs), to]) => {
            let (f3, rs1, rs2) = match mnemonic {
                "blez" => (5, ZERO, *rs),
                "bgez" => (5, *rs, ZERO),
                "bltz" => (4, *rs, ZERO),
                _ => (4, ZERO, *rs),
            };
            let imm = distance(to, 13, &mut out, Fixup::Branch).ok_or_else(unwritten)??;
            out.words.push(b(f3, rs1, rs2, imm));
        }
        ("zext.b", [Gpr(rd), Gpr(rs)]) => out.words.push(i(0x13, 7, *rd, *rs, 0xff)),
        ("jalr", [Gpr(rd), Gpr(rs), Imm(imm)]) => {
            out.words.push(i(0x67, 0, *rd, *rs, twelve(*imm)?));
        }
        ("lui", [Gpr(rd), Imm(imm)]) if (0..1 << 20).contains(imm) => {
            out.words.push(u(0x37, *rd, *imm));
        }
        ("lui", [Gpr(rd), Symbol(op @ (Operator::Hi | Operator::TprelHi))]) => {
            let fixup = if *op == Operator::Hi { Fixup::Hi20 } else { Fixup::TprelHi20 };
            out.fixups.push((0, fixup));
            out.words.push(u(0x37, *rd, 0));
        }
        ("auipc", [Gpr(rd), Imm(imm)]) if (0..1 << 20).contains(imm) => {
            out.words.push(u(0x17, *rd, *imm));
        }
        ("jal", [Gpr(rd), to]) => {
            let imm = distance(to, 21, &mut out, Fixup::Jal).ok_or_else(unwritten)??;
            out.words.push(j(*rd, imm));
        }
        ("jalr", [Gpr(rd), Mem(Addr { base, offset: Offset::Imm(imm) })]) => {
            out.words.push(i(0x67, 0, *rd, *base, twelve(*imm)?));
        }
        ("ecall", []) => out.words.push(0x0000_0073),
        ("ebreak", []) => out.words.push(0x0010_0073),
        ("fence", [Value::Order(pred), Value::Order(succ)]) if *pred < 16 && *succ < 16 => {
            out.words.push((u32::from(*pred) << 24) | (u32::from(*succ) << 20) | 0x0f);
        }
        // The pseudo instructions, as the base instructions GNU as writes for them.
        ("nop", []) => out.words.push(i(0x13, 0, ZERO, ZERO, 0)),
        ("li", [Gpr(rd), Imm(imm)]) => li(*rd, *imm, &mut out.words),
        ("mv", [Gpr(rd), Gpr(rs)]) => out.words.push(i(0x13, 0, *rd, *rs, 0)),
        ("not", [Gpr(rd), Gpr(rs)]) => out.words.push(i(0x13, 4, *rd, *rs, -1)),
        ("neg", [Gpr(rd), Gpr(rs)]) => out.words.push(r(0x33, 0, 0x20, *rd, ZERO, *rs)),
        ("negw", [Gpr(rd), Gpr(rs)]) => out.words.push(r(0x3b, 0, 0x20, *rd, ZERO, *rs)),
        ("sext.w", [Gpr(rd), Gpr(rs)]) => out.words.push(i(0x1b, 0, *rd, *rs, 0)),
        ("seqz", [Gpr(rd), Gpr(rs)]) => out.words.push(i(0x13, 3, *rd, *rs, 1)),
        ("snez", [Gpr(rd), Gpr(rs)]) => out.words.push(r(0x33, 3, 0, *rd, ZERO, *rs)),
        ("sltz", [Gpr(rd), Gpr(rs)]) => out.words.push(r(0x33, 2, 0, *rd, *rs, ZERO)),
        ("sgtz", [Gpr(rd), Gpr(rs)]) => out.words.push(r(0x33, 2, 0, *rd, ZERO, *rs)),
        ("beqz" | "bnez", [Gpr(rs), to]) => {
            let imm = distance(to, 13, &mut out, Fixup::Branch).ok_or_else(unwritten)??;
            out.words.push(b(u32::from(mnemonic == "bnez"), *rs, ZERO, imm));
        }
        ("j", [to]) => {
            let imm = distance(to, 21, &mut out, Fixup::Jal).ok_or_else(unwritten)??;
            out.words.push(j(ZERO, imm));
        }
        ("jr", [Gpr(rs)]) => out.words.push(i(0x67, 0, ZERO, *rs, 0)),
        ("jalr", [Gpr(rs)]) => out.words.push(i(0x67, 0, RA, *rs, 0)),
        ("ret", []) => out.words.push(i(0x67, 0, ZERO, RA, 0)),
        ("call" | "tail", [Symbol(Operator::Plain)]) => {
            let (link, through) = if mnemonic == "call" { (RA, RA) } else { (ZERO, T1) };
            out.fixups.push((0, Fixup::CallPlt));
            out.words.push(u(0x17, through, 0));
            out.words.push(i(0x67, 0, link, through, 0));
        }
        ("lla" | "la" | "la.tls.ie", [Gpr(rd), Symbol(Operator::Plain)]) => {
            let got = mnemonic != "lla";
            let hi = match mnemonic {
                "lla" => Fixup::PcrelHi20,
                "la" => Fixup::GotHi20,
                _ => Fixup::TlsGotHi20,
            };
            out.fixups.push((0, hi));
            out.fixups.push((1, Fixup::PcrelLo12I));
            out.words.push(u(0x17, *rd, 0));
            out.words.push(if got { i(0x03, 3, *rd, *rd, 0) } else { i(0x13, 0, *rd, *rd, 0) });
        }
        // A constant added to or taken from a register when it may not fit twelve bits, which is
        // how the frame moves the stack pointer. Not a mnemonic GNU as has: the listing writes it
        // as the instructions here, and the constant goes through `t5` when it does not fit, which
        // is the scratch register held back for this.
        ("addi.far" | "subi.far", [Gpr(rd), Gpr(rs), Imm(imm)]) => {
            let sub = mnemonic == "subi.far";
            let near = if sub { imm.checked_neg() } else { Some(*imm) };
            match near.filter(|&near| fits(near, 12)) {
                Some(near) => out.words.push(i(0x13, 0, *rd, *rs, near)),
                None => {
                    li(FAR, *imm, &mut out.words);
                    out.words.push(r(0x33, 0, if sub { 0x20 } else { 0 }, *rd, *rs, FAR));
                }
            }
        }
        // An address any distance from a base register, which is how the frame reaches a slot far
        // from the stack pointer. The constant is built in the register the address goes in, so
        // that one may not be the base when it does not fit.
        ("lea.far", [Gpr(rd), Mem(Addr { base, offset: Offset::Imm(imm) })]) => {
            if fits(*imm, 12) {
                out.words.push(i(0x13, 0, *rd, *base, *imm));
            } else if rd != base {
                li(*rd, *imm, &mut out.words);
                out.words.push(r(0x33, 0, 0, *rd, *base, *rd));
            } else {
                return Err(immediate(*imm));
            }
        }
        ("add", [Gpr(rd), Gpr(rs1), Gpr(rs2), Symbol(Operator::TprelAdd)]) => {
            out.fixups.push((0, Fixup::TprelAdd));
            out.words.push(r(0x33, 0, 0, *rd, *rs1, *rs2));
        }
        ("addi", [Gpr(rd), Gpr(rs1), Symbol(op @ (Operator::Lo | Operator::TprelLo))]) => {
            let fixup = if *op == Operator::Lo { Fixup::Lo12I } else { Fixup::TprelLo12I };
            out.fixups.push((0, fixup));
            out.words.push(i(0x13, 0, *rd, *rs1, 0));
        }
        _ => return Err(unwritten()),
    }
    Ok(out)
}

/// The distance a branch or a jump carries, from a number of bytes or from a label left for a
/// fixup, checked against how many bits the instruction has for it.
///
/// `None` for an operand that is neither, and an error inside for a number that does not fit or
/// is odd, since every instruction is at least two bytes from the next.
fn distance(to: &Value, bits: u32, out: &mut Encoded, fixup: Fixup) -> Option<Result<i64, Error>> {
    match *to {
        Value::Imm(imm) if fits(imm, bits) && imm % 2 == 0 => Some(Ok(imm)),
        Value::Imm(imm) => Some(Err(Error::Immediate { mnemonic: String::from("branch"), imm })),
        Value::Symbol(Operator::Plain) => {
            out.fixups.push((0, fixup));
            Some(Ok(0))
        }
        _ => None,
    }
}

/// A floating point instruction, or `None` for a mnemonic that is not one.
fn float(mnemonic: &str, values: &[Value], out: &mut Encoded) -> Option<Result<(), Error>> {
    use Value::{Fpr, Gpr, Round as Rm};
    let unwritten = || Error::Unwritten { mnemonic: mnemonic.to_owned(), operands: values.len() };
    let mut parts = mnemonic.split('.');
    let base = parts.next()?;
    let first = parts.next()?;
    let second = parts.next();
    if parts.next().is_some() {
        return None;
    }
    // The rounding mode, when the text gives one, which is always last.
    let (values, rounding) = match values {
        [rest @ .., Rm(round)] => (rest, Some(round.bits())),
        _ => (values, None),
    };
    let dynamic = rounding.unwrap_or(7);
    let word = match (base, second) {
        ("fadd" | "fsub" | "fmul" | "fdiv", None) => {
            let fmt = format(first)?;
            let funct = match base {
                "fadd" => 0x00,
                "fsub" => 0x04,
                "fmul" => 0x08,
                _ => 0x0c,
            };
            let [Fpr(rd), Fpr(rs1), Fpr(rs2)] = *values else { return Some(Err(unwritten())) };
            r(0x53, dynamic, funct | fmt, rd, rs1, rs2)
        }
        ("fsqrt", None) => {
            let fmt = format(first)?;
            let [Fpr(rd), Fpr(rs1)] = *values else { return Some(Err(unwritten())) };
            r(0x53, dynamic, 0x2c | fmt, rd, rs1, 0)
        }
        ("fsgnj" | "fsgnjn" | "fsgnjx" | "fmin" | "fmax", None) if rounding.is_none() => {
            let fmt = format(first)?;
            let (funct, f3) = match base {
                "fsgnj" => (0x10, 0),
                "fsgnjn" => (0x10, 1),
                "fsgnjx" => (0x10, 2),
                "fmin" => (0x14, 0),
                _ => (0x14, 1),
            };
            let [Fpr(rd), Fpr(rs1), Fpr(rs2)] = *values else { return Some(Err(unwritten())) };
            r(0x53, f3, funct | fmt, rd, rs1, rs2)
        }
        ("fmv" | "fneg" | "fabs", None) if rounding.is_none() && format(first).is_some() => {
            let fmt = format(first)?;
            let f3 = match base {
                "fmv" => 0,
                "fneg" => 1,
                _ => 2,
            };
            let [Fpr(rd), Fpr(rs)] = *values else { return Some(Err(unwritten())) };
            r(0x53, f3, 0x10 | fmt, rd, rs, rs)
        }
        ("feq" | "flt" | "fle", None) if rounding.is_none() => {
            let fmt = format(first)?;
            let f3 = match base {
                "feq" => 2,
                "flt" => 1,
                _ => 0,
            };
            let [Gpr(rd), Fpr(rs1), Fpr(rs2)] = *values else { return Some(Err(unwritten())) };
            r(0x53, f3, 0x50 | fmt, rd, rs1, rs2)
        }
        ("fmadd" | "fmsub" | "fnmsub" | "fnmadd", None) => {
            let fmt = format(first)?;
            let op = match base {
                "fmadd" => 0x43,
                "fmsub" => 0x47,
                "fnmsub" => 0x4b,
                _ => 0x4f,
            };
            let [Fpr(rd), Fpr(rs1), Fpr(rs2), Fpr(rs3)] = *values else {
                return Some(Err(unwritten()));
            };
            r(op, dynamic, (u32::from(rs3) << 2) | fmt, rd, rs1, rs2)
        }
        // Between the two float formats. Widening is exact, so GNU as writes it with the rounding
        // field at nearest rather than at dynamic, and so does this.
        ("fcvt", Some(from)) if format(first).is_some() && format(from).is_some() => {
            let (to, from) = (format(first)?, format(from)?);
            if to == from {
                return Some(Err(unwritten()));
            }
            let [Fpr(rd), Fpr(rs)] = *values else { return Some(Err(unwritten())) };
            let f3 = if to == 1 { rounding.unwrap_or(0) } else { dynamic };
            r(0x53, f3, 0x20 | to, rd, rs, u8::try_from(from).ok()?)
        }
        // A float to an integer.
        ("fcvt", Some(from)) if integer(first).is_some() => {
            let (kind, fmt) = (integer(first)?, format(from)?);
            let [Gpr(rd), Fpr(rs)] = *values else { return Some(Err(unwritten())) };
            r(0x53, dynamic, 0x60 | fmt, rd, rs, u8::try_from(kind).ok()?)
        }
        // An integer to a float. A thirty two bit integer fits a double exactly, which is the
        // other case GNU as writes at nearest.
        ("fcvt", Some(from)) if integer(from).is_some() => {
            let (fmt, kind) = (format(first)?, integer(from)?);
            let [Fpr(rd), Gpr(rs)] = *values else { return Some(Err(unwritten())) };
            let exact = fmt == 1 && kind < 2;
            let f3 = if exact { rounding.unwrap_or(0) } else { dynamic };
            r(0x53, f3, 0x68 | fmt, rd, rs, u8::try_from(kind).ok()?)
        }
        // The bits moved across between the files unchanged.
        ("fmv", Some(to)) if rounding.is_none() => match (first, to, values) {
            ("x", "w", [Gpr(rd), Fpr(rs)]) => r(0x53, 0, 0x70, *rd, *rs, 0),
            ("x", "d", [Gpr(rd), Fpr(rs)]) => r(0x53, 0, 0x71, *rd, *rs, 0),
            ("w", "x", [Fpr(rd), Gpr(rs)]) => r(0x53, 0, 0x78, *rd, *rs, 0),
            ("d", "x", [Fpr(rd), Gpr(rs)]) => r(0x53, 0, 0x79, *rd, *rs, 0),
            _ => return Some(Err(unwritten())),
        },
        _ => return None,
    };
    out.words.push(word);
    Some(Ok(()))
}

/// Whether the instruction of that mnemonic can carry that displacement from a base register,
/// which is twelve signed bits for everything that has one.
#[must_use]
pub fn reaches(disp: i64) -> bool {
    fits(disp, 12)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Value::{Fpr, Gpr, Imm, Mem};

    fn one(mnemonic: &str, values: &[Value]) -> u32 {
        let encoded = encode(mnemonic, values).unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(encoded.words.len(), 1, "{mnemonic}");
        encoded.words[0]
    }

    fn at(base: u8, disp: i64) -> Value {
        Mem(Addr { base, offset: Offset::Imm(disp) })
    }

    /// Words GNU as 2.42 writes for `-march=rv64g`, read back with objdump.
    #[test]
    fn the_words_are_the_ones_gnu_as_writes() {
        assert_eq!(one("add", &[Gpr(10), Gpr(11), Gpr(12)]), 0x00c5_8533);
        assert_eq!(one("subw", &[Gpr(10), Gpr(11), Gpr(12)]), 0x40c5_853b);
        assert_eq!(one("addi", &[Gpr(2), Gpr(2), Imm(-16)]), 0xff01_0113);
        assert_eq!(one("srai", &[Gpr(10), Gpr(10), Imm(56)]), 0x4385_5513);
        assert_eq!(one("sraiw", &[Gpr(10), Gpr(10), Imm(3)]), 0x4035_551b);
        assert_eq!(one("sd", &[Gpr(1), at(2, 8)]), 0x0011_3423);
        assert_eq!(one("ld", &[Gpr(1), at(2, 8)]), 0x0081_3083);
        assert_eq!(one("fld", &[Fpr(8), at(2, -8)]), 0xff81_3407);
        assert_eq!(one("fcvt.d.w", &[Fpr(10), Gpr(10)]), 0xd205_0553);
        assert_eq!(one("fcvt.d.l", &[Fpr(10), Gpr(10)]), 0xd225_7553);
        assert_eq!(one("fcvt.s.d", &[Fpr(10), Fpr(11)]), 0x4015_f553);
        assert_eq!(one("fcvt.d.s", &[Fpr(10), Fpr(11)]), 0x4205_8553);
        assert_eq!(one("fadd.d", &[Fpr(10), Fpr(11), Fpr(12)]), 0x02c5_f553);
        assert_eq!(one("fcvt.w.d", &[Gpr(10), Fpr(10), Value::Round(Round::Rtz)]), 0xc205_1553);
        assert_eq!(one("fmadd.d", &[Fpr(10), Fpr(11), Fpr(12), Fpr(13)]), 0x6ac5_f543);
        assert_eq!(one("fsqrt.d", &[Fpr(10), Fpr(11)]), 0x5a05_f553);
        assert_eq!(one("fcvt.d.lu", &[Fpr(10), Gpr(10)]), 0xd235_7553);
        assert_eq!(one("fcvt.d.wu", &[Fpr(10), Gpr(10)]), 0xd215_0553);
        assert_eq!(one("fcvt.s.w", &[Fpr(10), Gpr(10)]), 0xd005_7553);
        assert_eq!(one("fcvt.wu.d", &[Gpr(10), Fpr(10), Value::Round(Round::Rtz)]), 0xc215_1553);
        assert_eq!(one("fmv.x.d", &[Gpr(10), Fpr(10)]), 0xe205_0553);
        assert_eq!(one("feq.d", &[Gpr(10), Fpr(10), Fpr(11)]), 0xa2b5_2553);
        assert_eq!(one("fneg.d", &[Fpr(10), Fpr(11)]), 0x22b5_9553);
        assert_eq!(one("sgtz", &[Gpr(10), Gpr(11)]), 0x00b0_2533);
        assert_eq!(one("sltz", &[Gpr(10), Gpr(11)]), 0x0005_a533);
        assert_eq!(one("ret", &[]), 0x0000_8067);
        assert_eq!(one("ebreak", &[]), 0x0010_0073);
        assert_eq!(one("fence", &[Value::Order(3), Value::Order(3)]), 0x0330_000f);
    }

    /// The A extension, the CSR instructions and the system ones the kernel writes, with the words
    /// GNU as writes for `-march=rv64g_zifencei_zihintpause`.
    #[test]
    fn the_atomics_and_the_system_instructions_are_the_words_gnu_as_writes() {
        let words: &[(&str, &[Value], u32)] = &[
            ("lr.w", &[Gpr(15), at(10, 0)], 0x1005_27af),
            ("sc.d.rl", &[Gpr(15), Gpr(11), at(10, 0)], 0x1ab5_37af),
            ("amoswap.w.aqrl", &[Gpr(15), Gpr(11), at(10, 0)], 0x0eb5_27af),
            ("amoadd.d", &[Gpr(0), Gpr(11), at(10, 0)], 0x00b5_302f),
            ("amoor.w.aq", &[Gpr(15), Gpr(11), at(10, 0)], 0x44b5_27af),
            ("amomaxu.d", &[Gpr(15), Gpr(11), at(10, 0)], 0xe0b5_37af),
            ("csrr", &[Gpr(10), Imm(0x100)], 0x1000_2573),
            ("csrw", &[Imm(0x105), Gpr(10)], 0x1055_1073),
            ("csrs", &[Imm(0x100), Imm(2)], 0x1001_6073),
            ("csrc", &[Imm(0x104), Gpr(10)], 0x1045_3073),
            ("csrrw", &[Gpr(10), Imm(0x140), Gpr(11)], 0x1405_9573),
            ("csrrci", &[Gpr(10), Imm(0x100), Imm(2)], 0x1001_7573),
            ("rdtime", &[Gpr(10)], 0xc010_2573),
            ("fence.i", &[], 0x0000_100f),
            ("fence.tso", &[], 0x8330_000f),
            ("pause", &[], 0x0100_000f),
            ("wfi", &[], 0x1050_0073),
            ("sret", &[], 0x1020_0073),
            ("sfence.vma", &[], 0x1200_0073),
            ("sfence.vma", &[Gpr(10)], 0x1205_0073),
            ("sfence.vma", &[Gpr(10), Gpr(11)], 0x12b5_0073),
            ("bgt", &[Gpr(10), Gpr(11), Imm(8)], 0x00a5_c463),
            ("bleu", &[Gpr(10), Gpr(11), Imm(8)], 0x00a5_f463),
            ("blez", &[Gpr(10), Imm(8)], 0x00a0_5463),
            ("bgtz", &[Gpr(10), Imm(8)], 0x00a0_4463),
            ("zext.b", &[Gpr(10), Gpr(11)], 0x0ff5_f513),
            ("jalr", &[Gpr(1), Gpr(10), Imm(4)], 0x0045_00e7),
        ];
        for &(mnemonic, values, word) in words {
            assert_eq!(one(mnemonic, values), word, "{mnemonic} {values:?}");
        }
        // An atomic takes no distance, and a CSR number past twelve bits is refused.
        assert!(encode("amoadd.w", &[Gpr(15), Gpr(11), at(10, 4)]).is_err());
        assert!(encode("csrr", &[Gpr(10), Imm(0x1000)]).is_err());
        assert!(encode("csrsi", &[Imm(0x100), Imm(32)]).is_err());
    }

    #[test]
    fn a_constant_is_as_many_words_as_gnu_as_writes_for_it() {
        let words = |imm| encode("li", &[Gpr(10), Imm(imm)]).unwrap().words;
        assert_eq!(words(-1), [0xfff0_0513]);
        assert_eq!(words(0x7fff_f800), [0x8000_0537, 0x8005_051b]);
        assert_eq!(words(0x0012_3000), [0x0012_3537]);
        assert_eq!(li_words(0x1234_5678_9abc_def0), 8);
        assert_eq!(li_words(0x8000_0000), 2);
        // Wider than thirty two bits, where the upper part of twelve bits is an `addiw`.
        assert_eq!(words(0x3fe0_0000_0000_0000), [0x1ff0_051b, 0x0355_1513]);
        assert_eq!(words(-0x8000_0001), [0xfff0_051b, 0x01f5_1513, 0xfff5_0513]);
        assert_eq!(words(0x1_2345_6789), [0x0009_2537, 0xa2b5_051b, 0x00d5_1513, 0x7895_0513]);
    }

    /// Every constant `li` builds is the constant, which is checked by running the words.
    #[test]
    fn a_constant_comes_out_as_itself() {
        let run = |words: &[u32]| {
            let mut reg: i64 = 0;
            for &word in words {
                let imm = i64::from((word as i32) >> 20);
                reg = match word & 0x7f {
                    0x37 => i64::from((word & 0xffff_f000) as i32),
                    0x1b => {
                        let from = if (word >> 15) & 31 == 0 { 0 } else { reg };
                        i64::from(from.wrapping_add(imm) as i32)
                    }
                    0x13 if (word >> 12) & 7 == 1 => reg << (imm & 63),
                    0x13 => {
                        let from = if (word >> 15) & 31 == 0 { 0 } else { reg };
                        from.wrapping_add(imm)
                    }
                    _ => panic!("{word:#x}"),
                };
            }
            reg
        };
        for imm in [
            0,
            1,
            -1,
            2047,
            -2048,
            2048,
            0x7fff_ffff,
            -0x8000_0000,
            0x8000_0000,
            0xffff_ffff,
            0x1234_5678_9abc_def0,
            i64::MAX,
            i64::MIN,
            -0x1234_5678_9abc,
            0x7fff_f800,
            0x0000_0800_0000_0800,
        ] {
            let words = encode("li", &[Gpr(10), Imm(imm)]).unwrap().words;
            assert_eq!(run(&words), imm, "{imm:#x}");
        }
    }

    #[test]
    fn a_symbol_is_left_for_the_linker() {
        let call = encode("call", &[Value::Symbol(Operator::Plain)]).unwrap();
        assert_eq!(call.words, [0x0000_0097, 0x0000_80e7]);
        assert_eq!(call.fixups, [(0, Fixup::CallPlt)]);
        let tail = encode("tail", &[Value::Symbol(Operator::Plain)]).unwrap();
        assert_eq!(tail.words, [0x0000_0317, 0x0003_0067]);
        let la = encode("lla", &[Gpr(10), Value::Symbol(Operator::Plain)]).unwrap();
        assert_eq!(la.fixups, [(0, Fixup::PcrelHi20), (1, Fixup::PcrelLo12I)]);
        let ie = encode("la.tls.ie", &[Gpr(10), Value::Symbol(Operator::Plain)]).unwrap();
        assert_eq!(ie.words, [0x0000_0517, 0x0005_3503]);
        assert_eq!(ie.fixups, [(0, Fixup::TlsGotHi20), (1, Fixup::PcrelLo12I)]);
        assert_eq!(Fixup::TlsGotHi20.elf(), 21);
        let branch = encode("bnez", &[Gpr(10), Value::Symbol(Operator::Plain)]).unwrap();
        assert_eq!((branch.words[0], branch.fixups[0].1.elf()), (0x0005_1063, 16));
        assert_eq!(one("beqz", &[Gpr(10), Imm(8)]), 0x0005_0463);
        assert_eq!(one("j", &[Imm(-4)]), 0xffdf_f06f);
    }

    #[test]
    fn a_far_constant_goes_through_t5_and_a_far_address_through_its_own_register() {
        let words = |mnemonic, imm| encode(mnemonic, &[Gpr(2), Gpr(2), Imm(imm)]).unwrap().words;
        assert_eq!(words("addi.far", 16), [0x0101_0113]);
        assert_eq!(words("subi.far", 2048), [0x8001_0113]);
        // li t5, 4096 and sub sp, sp, t5.
        assert_eq!(words("subi.far", 4096), [0x0000_1f37, 0x41e1_0133]);
        let lea = |rd, disp| encode("lea.far", &[Gpr(rd), at(8, disp)]);
        assert_eq!(lea(10, -16).unwrap().words, [0xff04_0513]);
        // li a0, 8192 and add a0, s0, a0.
        assert_eq!(lea(10, 8192).unwrap().words, [0x0000_2537, 0x00a4_0533]);
        assert!(lea(8, 8192).is_err());
    }

    #[test]
    fn what_does_not_fit_is_refused() {
        let far = encode("addi", &[Gpr(10), Gpr(10), Imm(2048)]);
        assert!(matches!(far, Err(Error::Immediate { imm: 2048, .. })));
        assert!(encode("ld", &[Gpr(10), at(2, 4096)]).is_err());
        assert!(encode("slli", &[Gpr(10), Gpr(10), Imm(64)]).is_err());
        assert!(encode("slliw", &[Gpr(10), Gpr(10), Imm(32)]).is_err());
        assert!(encode("b.ne", &[Value::Symbol(Operator::Plain)]).is_err());
        assert!(encode("add", &[Gpr(10), Fpr(10), Gpr(10)]).is_err());
        assert!(reaches(-2048) && !reaches(2048));
    }

    /// A distance filled in later is the word the encoder writes for the same distance now.
    #[test]
    fn a_distance_filled_in_later_is_the_word_written_with_it() {
        let named = |mnemonic, values: &[Value]| {
            let encoded = encode(mnemonic, values).unwrap_or_else(|why| panic!("{why}"));
            (encoded.words[0], encoded.fixups[0].1)
        };
        let symbol = Value::Symbol(Operator::Plain);
        for distance in [-4096, -2, 8, 4094] {
            let (word, fixup) = named("bne", &[Gpr(10), Gpr(11), symbol]);
            let filled = fixup.apply(word, distance);
            assert_eq!(filled, Some(one("bne", &[Gpr(10), Gpr(11), Imm(distance)])), "{distance}");
        }
        for distance in [-(1 << 20), -2, 16, (1 << 20) - 2] {
            let (word, fixup) = named("jal", &[Gpr(1), symbol]);
            assert_eq!(fixup.apply(word, distance), Some(one("jal", &[Gpr(1), Imm(distance)])));
        }
        let (word, fixup) = named("beqz", &[Gpr(10), symbol]);
        assert_eq!(fixup.apply(word, 4096), None);
        assert_eq!(fixup.apply(word, 3), None);
        assert_eq!(Fixup::CallPlt.apply(0, 8), None);
        assert!(Fixup::TprelAdd.thread() && !Fixup::PcrelHi20.thread());
    }
}
