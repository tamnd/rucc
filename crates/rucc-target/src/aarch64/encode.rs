//! What each AArch64 instruction is as a word.
//!
//! Design: `spec/cross-compile/06-abis.md` section 6.2 and `spec/11-asm-objects-debug.md` section
//! 11.1.
//!
//! Every instruction on this machine is four bytes, and most of the work an x86-64 encoder does is
//! not here: there are no prefixes, no addressing byte and no choice of length. What is here is
//! the other half of the problem, which is that one mnemonic in the text is several instructions in
//! the manual. `mov` is an `orr`, an `add`, a `movz`, a `movn` or a logical immediate depending on
//! its operands, `cmp` is a `subs` that throws its result away, and `lsl` with a number is a
//! bitfield move. The names a person writes are the ones this takes, and it picks the instruction
//! the way GNU as does, so the word for a line of text is the word GNU as writes for it. The tests
//! check that against a list of words GNU as 2.42 wrote, one for every form here.
//!
//! # Registers
//!
//! By number, the way the encoding has them. Thirty one is the zero register in most places and
//! the stack pointer in a few, and which it is belongs to the instruction, so [`Value::Gpr`] with
//! thirty one is always the zero register and [`Value::Sp`] is always the stack pointer. An
//! instruction given one where it can only mean the other is an error rather than a word that
//! means something else.
//!
//! # What is left for later
//!
//! A symbol's address, and the distance to a label. An instruction that names one is written with
//! zeros where the number goes and comes back with the [`Fixup`] that says how to fill them in,
//! which is either a relocation in the object or a patch once the label has a place. The vector
//! instructions past the two a scalar back end uses to copy and to clear a register are not here,
//! and neither are the atomics from the large system extensions, which arrive with the target
//! features that have them.

use std::fmt;

use crate::aarch64::system::PSTATE;

mod atomic;
mod neon;

/// How much of a general purpose register an instruction reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    /// The low thirty two bits, which is what `w0` names.
    W,
    /// All sixty four, which is what `x0` names.
    X,
}

impl Width {
    /// The bit most instructions carry at the top to say they are sixty four bits.
    fn sf(self) -> u32 {
        match self {
            Width::W => 0,
            Width::X => 1,
        }
    }

    /// How many bits that is.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Width::W => 32,
            Width::X => 64,
        }
    }
}

/// How much of a vector register a scalar instruction reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    /// One byte, `b0`.
    B,
    /// Two bytes, `h0`, which is a half precision number when it is a number.
    H,
    /// Four bytes, `s0`, a `float`.
    S,
    /// Eight bytes, `d0`, a `double`.
    D,
    /// All sixteen, `q0`.
    Q,
}

impl Scalar {
    /// The field the floating point instructions use to say which precision they work in.
    fn ftype(self) -> Option<u32> {
        match self {
            Scalar::S => Some(0b00),
            Scalar::D => Some(0b01),
            Scalar::H => Some(0b11),
            Scalar::B | Scalar::Q => None,
        }
    }
}

/// How the lanes of a whole vector register are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrangement {
    /// Eight bytes in the low half, `v0.8b`.
    B8,
    /// Sixteen bytes, `v0.16b`.
    B16,
    /// Four two byte lanes in the low half, `v0.4h`.
    H4,
    /// Eight two byte lanes, `v0.8h`.
    H8,
    /// Two four byte lanes in the low half, `v0.2s`.
    S2,
    /// Four four byte lanes, `v0.4s`.
    S4,
    /// One eight byte lane, `v0.1d`, which only the loads and stores of lists take.
    D1,
    /// Two eight byte lanes, `v0.2d`.
    D2,
    /// One sixteen byte lane, `v0.1q`, which only the polynomial multiply of two doublewords
    /// writes.
    Q1,
}

impl Arrangement {
    /// The arrangement a register is written with after its dot, `16b` or `2d`.
    #[must_use]
    pub fn named(name: &str) -> Option<Arrangement> {
        Some(match name {
            "8b" => Arrangement::B8,
            "16b" => Arrangement::B16,
            "4h" => Arrangement::H4,
            "8h" => Arrangement::H8,
            "2s" => Arrangement::S2,
            "4s" => Arrangement::S4,
            "1d" => Arrangement::D1,
            "2d" => Arrangement::D2,
            "1q" => Arrangement::Q1,
            _ => return None,
        })
    }

    /// How it is written after the dot.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Arrangement::B8 => "8b",
            Arrangement::B16 => "16b",
            Arrangement::H4 => "4h",
            Arrangement::H8 => "8h",
            Arrangement::S2 => "2s",
            Arrangement::S4 => "4s",
            Arrangement::D1 => "1d",
            Arrangement::D2 => "2d",
            Arrangement::Q1 => "1q",
        }
    }
}

/// How a register operand is shifted before it is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shift {
    /// Left.
    Lsl,
    /// Right, filling with zeros.
    Lsr,
    /// Right, filling with the sign.
    Asr,
    /// Round, which only the logical instructions have.
    Ror,
}

/// How a register operand is widened before it is used, in the order the encoding numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extend {
    /// The low byte, zero extended.
    Uxtb,
    /// The low two bytes, zero extended.
    Uxth,
    /// The low four bytes, zero extended.
    Uxtw,
    /// All eight, which is what a shift left is written as where only an extension can be.
    Uxtx,
    /// The low byte, sign extended.
    Sxtb,
    /// The low two bytes, sign extended.
    Sxth,
    /// The low four bytes, sign extended.
    Sxtw,
    /// All eight.
    Sxtx,
}

impl Extend {
    /// The number the encoding gives it.
    fn option(self) -> u32 {
        self as u32
    }

    /// Whether the register it widens is named as a `w` register.
    fn reads_w(self) -> bool {
        !matches!(self, Extend::Uxtx | Extend::Sxtx)
    }
}

/// A condition on the flags, numbered the way the encoding numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Carry set, which after a comparison is unsigned higher or the same. Also `cs`.
    Hs,
    /// Carry clear, unsigned lower. Also `cc`.
    Lo,
    /// Negative.
    Mi,
    /// Positive or zero.
    Pl,
    /// Overflow.
    Vs,
    /// No overflow.
    Vc,
    /// Unsigned higher.
    Hi,
    /// Unsigned lower or the same.
    Ls,
    /// Signed greater or equal.
    Ge,
    /// Signed less.
    Lt,
    /// Signed greater.
    Gt,
    /// Signed less or equal.
    Le,
    /// Always.
    Al,
    /// Also always, and never what a person means.
    Nv,
}

impl Cond {
    /// Every condition, in the order the encoding numbers them.
    const ALL: [Cond; 16] = [
        Cond::Eq,
        Cond::Ne,
        Cond::Hs,
        Cond::Lo,
        Cond::Mi,
        Cond::Pl,
        Cond::Vs,
        Cond::Vc,
        Cond::Hi,
        Cond::Ls,
        Cond::Ge,
        Cond::Lt,
        Cond::Gt,
        Cond::Le,
        Cond::Al,
        Cond::Nv,
    ];

    /// The condition that holds when this one does not.
    ///
    /// The two that always hold have no opposite, and the aliases that need one refuse them.
    #[must_use]
    pub fn invert(self) -> Option<Cond> {
        match self {
            Cond::Al | Cond::Nv => None,
            other => Some(Cond::ALL[(other as usize) ^ 1]),
        }
    }

    /// The condition with that name, including the two second names the carry ones have.
    #[must_use]
    pub fn named(name: &str) -> Option<Cond> {
        Some(match name {
            "eq" => Cond::Eq,
            "ne" => Cond::Ne,
            "hs" | "cs" => Cond::Hs,
            "lo" | "cc" => Cond::Lo,
            "mi" => Cond::Mi,
            "pl" => Cond::Pl,
            "vs" => Cond::Vs,
            "vc" => Cond::Vc,
            "hi" => Cond::Hi,
            "ls" => Cond::Ls,
            "ge" => Cond::Ge,
            "lt" => Cond::Lt,
            "gt" => Cond::Gt,
            "le" => Cond::Le,
            "al" => Cond::Al,
            "nv" => Cond::Nv,
            _ => return None,
        })
    }

    fn bits(self) -> u32 {
        self as u32
    }
}

/// Which part of a symbol's address an operand asks for, spelled in the text as `:lo12:` and the
/// like in front of the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// The address itself, or the distance to it, which is what a bare name means.
    Plain,
    /// The low twelve bits of the address, `:lo12:`.
    Lo12,
    /// The page of the address's entry in the global offset table, `:got:`.
    Got,
    /// The low twelve bits of that entry's address, `:got_lo12:`.
    GotLo12,
    /// The page of the entry in the global offset table that holds a thread-local variable's
    /// offset from the thread pointer, `:gottprel:`.
    GotTprel,
    /// The low twelve bits of that entry's address, `:gottprel_lo12:`.
    GotTprelLo12,
    /// Bits twelve to twenty three of the offset from the thread pointer, `:tprel_hi12:`.
    TprelHi12,
    /// The low twelve bits of it, `:tprel_lo12_nc:`.
    TprelLo12Nc,
    /// Bits twelve to twenty three of the offset from the start of the section, `:secrel_hi12:`,
    /// which is how a Windows program finds a thread-local variable in its block once the block's
    /// address has been read out of the thread's slot for the image.
    SecrelHi12,
    /// The low twelve bits of it, `:secrel_lo12:`.
    SecrelLo12,
}

/// What is added to the base register of an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offset {
    /// A number.
    Imm(i64),
    /// A register, widened and shifted.
    ///
    /// A shift left is written as [`Extend::Uxtx`], which is what the encoding makes it. The
    /// amount is `None` when the text gave none, which is a different word from one that gave
    /// zero on a byte access.
    Reg {
        /// Which register.
        reg: u8,
        /// How it is widened, which also says whether it is named as a `w` or an `x` register.
        extend: Extend,
        /// How far it is shifted, which the machine only allows as nothing or the size of the
        /// access.
        amount: Option<u8>,
    },
    /// Part of a symbol's address, which is left for a relocation to fill in.
    Symbol(Operator),
    /// A number of whole vector lengths, `#3, mul vl`, which only the SVE loads and stores take.
    Vl(i64),
}

/// When the base register is moved by the offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Never. The access is at the base plus the offset.
    Offset,
    /// Before the access, which is at the new base. `[x0, #8]!`.
    Pre,
    /// After the access, which is at the old base. `[x0], #8`.
    Post,
}

/// An address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Addr {
    /// The register the address starts from, where thirty one is the stack pointer, since the
    /// zero register cannot be a base.
    pub base: u8,
    /// What is added to it.
    pub offset: Offset,
    /// When the base is written back.
    pub mode: Mode,
}

/// What one operand of an instruction is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    /// A general purpose register, where thirty one is the zero register.
    Gpr(Width, u8),
    /// The stack pointer, as `sp` or `wsp`.
    Sp(Width),
    /// A vector register read as a scalar, `s0` or `d0`.
    Fp(Scalar, u8),
    /// A whole vector register with its lanes, `v0.16b`.
    Vector(Arrangement, u8),
    /// One lane of a vector register, `v0.s[1]`: the lane size, the register, and the index.
    Element(Scalar, u8, u8),
    /// A list of registers of one arrangement that follow each other, `{v0.16b, v1.16b}`: the
    /// arrangement, the first register, and how many there are. The one after thirty one is
    /// zero.
    List(Arrangement, u8, u8),
    /// A number.
    Imm(i64),
    /// A floating point number, which only `fmov` and the comparisons with zero take.
    Float(f64),
    /// How the operand before this one is shifted, or how far a wide move's number is.
    Shift(Shift, u8),
    /// How the operand before this one is widened, and how far it is then shifted.
    Extend(Extend, Option<u8>),
    /// A condition.
    Cond(Cond),
    /// An address in memory.
    Mem(Addr),
    /// A symbol, or part of its address, which is left for a relocation or a patch.
    Symbol(Operator),
    /// Which accesses a barrier orders, as the number the encoding gives it. `ish` is eleven.
    Barrier(u8),
    /// A system register, as the sixteen bits `mrs` and `msr` carry for it.
    System(u16),
    /// What a `prfm` is asked to do, as the five bits the encoding gives it. `pldl1keep` is zero.
    Prefetch(u8),
    /// A scalable vector register of SVE, `z0`, taken whole.
    Z(u8),
    /// A predicate register of SVE, `p0`, and the size of its lanes when the text gave one, as in
    /// `p0.b`.
    Pred(u8, Option<Scalar>),
    /// A slice of the SME array `za`, chosen by one of `w12` to `w15` and a number added to it,
    /// as in `za[w12, #0]`, kept as the register's number and the number.
    Za(u8, u8),
    /// A field of the processor state `msr` writes an immediate to, as where it is in the table of
    /// them. `daifset` is one.
    Pstate(u8),
}

/// How the zeros an instruction was written with are to be filled in.
///
/// Each of these but the last three is one relocation type in the ELF ABI for this machine, and the
/// names are that document's without the `R_AARCH64_` in front. The last three are the offset from
/// the start of the section, which ELF has no relocation for on this machine and COFF does, and
/// their names are COFF's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixup {
    /// The distance to where `b` goes, in words, in twenty six bits.
    Jump26,
    /// The distance to what `bl` calls, the same way. A linker may send it through a veneer.
    Call26,
    /// The distance a conditional branch, `cbz` or `cbnz` goes, in words, in nineteen bits.
    CondBr19,
    /// The distance `tbz` or `tbnz` goes, in words, in fourteen bits.
    TestBr14,
    /// The distance to what a literal load reads, in words, in nineteen bits.
    Literal19,
    /// The distance to what `adr` names, in bytes, in twenty one bits.
    AdrLo21,
    /// The distance in pages from this one to the page `adrp` names.
    AdrPage21,
    /// The low twelve bits of an address, added by `add`.
    AddLo12,
    /// The low twelve bits of an address, as the offset of a one byte access.
    Ldst8Lo12,
    /// The same for a two byte access, which carries them divided by two.
    Ldst16Lo12,
    /// The same for a four byte access.
    Ldst32Lo12,
    /// The same for an eight byte access.
    Ldst64Lo12,
    /// The same for a sixteen byte access.
    Ldst128Lo12,
    /// The page of a symbol's entry in the global offset table, for `adrp`.
    GotPage21,
    /// The low twelve bits of that entry, as the offset of the eight byte load that reads it.
    GotLo12,
    /// The page of the entry holding a thread-local variable's offset from the thread pointer.
    GotTprelPage21,
    /// The low twelve bits of that entry, as the offset of the eight byte load that reads it.
    GotTprelLo12Nc,
    /// Bits twelve to twenty three of the offset from the thread pointer, for `add` with a shift.
    TprelHi12,
    /// The low twelve bits of it, for `add`.
    TprelLo12Nc,
    /// Bits twelve to twenty three of the offset from the start of the section, for `add` with a
    /// shift.
    SecrelHigh12A,
    /// The low twelve bits of it, for `add`.
    SecrelLow12A,
    /// The low twelve bits of it, as the offset of an access of the size the instruction says,
    /// which carries them divided by that size.
    SecrelLow12L,
}

impl Fixup {
    /// The relocation type the ELF ABI gives it, or `None` for the three it has no type for.
    #[must_use]
    pub fn elf(self) -> Option<u32> {
        Some(match self {
            Fixup::Literal19 => 273,
            Fixup::AdrLo21 => 274,
            Fixup::AdrPage21 => 275,
            Fixup::AddLo12 => 277,
            Fixup::Ldst8Lo12 => 278,
            Fixup::TestBr14 => 279,
            Fixup::CondBr19 => 280,
            Fixup::Jump26 => 282,
            Fixup::Call26 => 283,
            Fixup::Ldst16Lo12 => 284,
            Fixup::Ldst32Lo12 => 285,
            Fixup::Ldst64Lo12 => 286,
            Fixup::Ldst128Lo12 => 299,
            Fixup::GotPage21 => 311,
            Fixup::GotLo12 => 312,
            Fixup::GotTprelPage21 => 541,
            Fixup::GotTprelLo12Nc => 542,
            Fixup::TprelHi12 => 549,
            Fixup::TprelLo12Nc => 551,
            Fixup::SecrelHigh12A | Fixup::SecrelLow12A | Fixup::SecrelLow12L => return None,
        })
    }

    /// The name the ELF ABI gives it, which is what `readelf` and `objdump` print.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Fixup::Jump26 => "R_AARCH64_JUMP26",
            Fixup::Call26 => "R_AARCH64_CALL26",
            Fixup::CondBr19 => "R_AARCH64_CONDBR19",
            Fixup::TestBr14 => "R_AARCH64_TSTBR14",
            Fixup::Literal19 => "R_AARCH64_LD_PREL_LO19",
            Fixup::AdrLo21 => "R_AARCH64_ADR_PREL_LO21",
            Fixup::AdrPage21 => "R_AARCH64_ADR_PREL_PG_HI21",
            Fixup::AddLo12 => "R_AARCH64_ADD_ABS_LO12_NC",
            Fixup::Ldst8Lo12 => "R_AARCH64_LDST8_ABS_LO12_NC",
            Fixup::Ldst16Lo12 => "R_AARCH64_LDST16_ABS_LO12_NC",
            Fixup::Ldst32Lo12 => "R_AARCH64_LDST32_ABS_LO12_NC",
            Fixup::Ldst64Lo12 => "R_AARCH64_LDST64_ABS_LO12_NC",
            Fixup::Ldst128Lo12 => "R_AARCH64_LDST128_ABS_LO12_NC",
            Fixup::GotPage21 => "R_AARCH64_ADR_GOT_PAGE",
            Fixup::GotLo12 => "R_AARCH64_LD64_GOT_LO12_NC",
            Fixup::GotTprelPage21 => "R_AARCH64_TLSIE_ADR_GOTTPREL_PAGE21",
            Fixup::GotTprelLo12Nc => "R_AARCH64_TLSIE_LD64_GOTTPREL_LO12_NC",
            Fixup::TprelHi12 => "R_AARCH64_TLSLE_ADD_TPREL_HI12",
            Fixup::TprelLo12Nc => "R_AARCH64_TLSLE_ADD_TPREL_LO12_NC",
            Fixup::SecrelHigh12A => "IMAGE_REL_ARM64_SECREL_HIGH12A",
            Fixup::SecrelLow12A => "IMAGE_REL_ARM64_SECREL_LOW12A",
            Fixup::SecrelLow12L => "IMAGE_REL_ARM64_SECREL_LOW12L",
        }
    }

    /// The word with the number filled in, or `None` when the number does not fit.
    ///
    /// For the branches, the literal load and `adr` the number is the distance in bytes from the
    /// instruction to where it points. For `adrp` and the global offset table page it is the
    /// distance in bytes from the page the instruction is on to the page it names, which is a
    /// multiple of four thousand and ninety six. For the rest it is the address, or the offset
    /// from the thread pointer, whose part the instruction carries. The ones whose names end in
    /// `NC` do not check that the rest of it is zero, which is what the name says.
    #[must_use]
    pub fn apply(self, word: u32, value: i64) -> Option<u32> {
        let low = (value & 0xfff) as u32;
        match self {
            Fixup::Jump26 | Fixup::Call26 => Some(word | pc_relative(value, 26)?),
            Fixup::CondBr19 | Fixup::Literal19 => Some(word | pc_relative(value, 19)? << 5),
            Fixup::TestBr14 => Some(word | pc_relative(value, 14)? << 5),
            Fixup::AdrLo21 => Some(word | adr_bits(value)?),
            Fixup::AdrPage21 | Fixup::GotPage21 | Fixup::GotTprelPage21 => {
                if value & 0xfff != 0 {
                    return None;
                }
                Some(word | adr_bits(value >> 12)?)
            }
            Fixup::AddLo12 | Fixup::TprelLo12Nc | Fixup::SecrelLow12A => Some(word | low << 10),
            Fixup::TprelHi12 | Fixup::SecrelHigh12A => {
                let high = u32::try_from(value >> 12).ok().filter(|&high| high < 1 << 12)?;
                (value >= 0).then_some(word | high << 10)
            }
            Fixup::Ldst8Lo12 => Some(word | low << 10),
            Fixup::Ldst16Lo12 => scaled_low(word, low, 1),
            Fixup::Ldst32Lo12 => scaled_low(word, low, 2),
            Fixup::Ldst64Lo12 | Fixup::GotLo12 | Fixup::GotTprelLo12Nc => scaled_low(word, low, 3),
            Fixup::Ldst128Lo12 => scaled_low(word, low, 4),
            Fixup::SecrelLow12L => scaled_low(word, low, access_scale(word)),
        }
    }
}

/// How many bits the offset of a load or store with an unsigned offset is shifted by, which is the
/// log of the size of the access: the two size bits at the top, and four for a whole vector
/// register, which says so with a bit of the opcode as well.
fn access_scale(word: u32) -> u32 {
    let size = word >> 30;
    if word & 0x0480_0000 == 0x0480_0000 { size + 4 } else { size }
}

/// A distance in bytes as that many bits of words, or `None` when it is not a whole number of
/// words or does not fit.
fn pc_relative(value: i64, bits: u32) -> Option<u32> {
    if value & 3 != 0 {
        return None;
    }
    signed(value >> 2, bits)
}

/// A number as the low `bits` bits of its two's complement, when it fits in them.
fn signed(value: i64, bits: u32) -> Option<u32> {
    let half = 1i64 << (bits - 1);
    ((-half..half).contains(&value)).then(|| (value as u32) & ((1u32 << bits) - 1))
}

/// A twenty one bit number the way `adr` and `adrp` carry it, with the low two bits in one place
/// and the other nineteen in another.
fn adr_bits(value: i64) -> Option<u32> {
    let bits = signed(value, 21)?;
    Some((bits & 3) << 29 | (bits >> 2) << 5)
}

/// The low twelve bits of an address as the offset of an access that carries it divided by its
/// size, or `None` when the address is not aligned to that size.
fn scaled_low(word: u32, low: u32, scale: u32) -> Option<u32> {
    (low & ((1 << scale) - 1) == 0).then_some(word | (low >> scale) << 10)
}

/// One instruction as a word, and what is left to fill in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoded {
    /// The instruction, with zeros where a symbol's address or a label's distance goes.
    pub word: u32,
    /// How to fill those in, when the instruction named something.
    pub fixup: Option<Fixup>,
}

/// Why an instruction could not be encoded.
///
/// Each is a bug in whatever produced the instruction rather than anything a C program could ask
/// for, so they say which instruction it was and not much more.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// Nothing here encodes that mnemonic with those operands.
    Unwritten {
        /// The mnemonic that was asked for.
        mnemonic: String,
        /// How many operands it was given.
        operands: usize,
    },
    /// A number that no form of the instruction can carry.
    Immediate {
        /// The mnemonic that was asked for.
        mnemonic: String,
        /// The number.
        imm: i64,
    },
    /// Operands of different widths where the instruction needs them to be the same, or the
    /// stack pointer where only the zero register can be, or the other way round.
    Register {
        /// The mnemonic that was asked for.
        mnemonic: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unwritten { mnemonic, operands } => {
                write!(f, "no encoding for {mnemonic} with {operands} operands of those kinds")
            }
            Error::Immediate { mnemonic, imm } => {
                write!(f, "no form of {mnemonic} can carry the immediate {imm}")
            }
            Error::Register { mnemonic } => {
                write!(f, "{mnemonic} was given a register it cannot name there")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Writes one instruction.
///
/// The operands are in the order the text writes them, with a shift or an extension as an operand
/// of its own after the register it applies to, which is how the text writes those too.
///
/// # Errors
///
/// When nothing here encodes that mnemonic with those operands, when a number does not fit any
/// form of it, and when a register is one the instruction cannot name where it is. See [`Error`].
pub fn encode(mnemonic: &str, values: &[Value]) -> Result<Encoded, Error> {
    let at = At { mnemonic, operands: values.len() };
    let (word, fixup) = at.encode(values)?;
    Ok(Encoded { word, fixup })
}

/// The instruction being encoded, which every error names.
#[derive(Clone, Copy)]
struct At<'a> {
    mnemonic: &'a str,
    operands: usize,
}

/// A word, and what is left to fill in.
type Word = (u32, Option<Fixup>);

impl At<'_> {
    fn unwritten(self) -> Error {
        Error::Unwritten { mnemonic: self.mnemonic.to_owned(), operands: self.operands }
    }

    fn immediate(self, imm: i64) -> Error {
        Error::Immediate { mnemonic: self.mnemonic.to_owned(), imm }
    }

    fn register(self) -> Error {
        Error::Register { mnemonic: self.mnemonic.to_owned() }
    }

    /// The memory copy and set instructions, `cpyfp [x0]!, [x1]!, x2!` and `setp [x0]!, x1!, x2`,
    /// each of which is written three times, for the start, the middle and the end of the work.
    /// The letters after that say which accesses are unprivileged and which do not stream.
    fn mops(self, values: &[Value]) -> Result<Option<u32>, Error> {
        const COPY: [&str; 16] = [
            "", "wt", "rt", "t", "wn", "wtwn", "rtwn", "twn", "rn", "wtrn", "rtrn", "trn", "n",
            "wtn", "rtn", "tn",
        ];
        const SET: [&str; 4] = ["", "t", "n", "tn"];
        let m = self.mnemonic;
        let (base, rest, copy) = if let Some(rest) = m.strip_prefix("cpyf") {
            (0x1900_0400, rest, true)
        } else if let Some(rest) = m.strip_prefix("cpy") {
            (0x1D00_0400, rest, true)
        } else if let Some(rest) = m.strip_prefix("setg") {
            (0x1DC0_0400, rest, false)
        } else if let Some(rest) = m.strip_prefix("set") {
            (0x19C0_0400, rest, false)
        } else {
            return Ok(None);
        };
        let stage = match rest.get(..1) {
            Some("p") => 0,
            Some("m") => 1,
            Some("e") => 2,
            _ => return Ok(None),
        };
        let options: &[&str] = if copy { &COPY } else { &SET };
        let Some(option) = options.iter().position(|&option| option == &rest[1..]) else {
            return Ok(None);
        };
        let option = option as u32;
        let walked = |value: &Value| match value {
            Value::Mem(Addr { base, offset: Offset::Imm(0), mode: Mode::Pre }) if *base != 31 => {
                Ok(u32::from(*base))
            }
            _ => Err(self.register()),
        };
        let x = |value: &Value| match value {
            Value::Gpr(Width::X, reg) => Ok(u32::from(*reg)),
            _ => Err(self.register()),
        };
        let word = match (copy, values) {
            (true, [d, s, n]) => {
                base | stage << 22 | option << 12 | walked(s)? << 16 | x(n)? << 5 | walked(d)?
            }
            (false, [d, n, s]) => {
                base | (stage << 2 | option) << 12 | x(s)? << 16 | x(n)? << 5 | walked(d)?
            }
            _ => return Err(self.unwritten()),
        };
        Ok(Some(word))
    }

    /// The SVE instructions the kernel saves and restores a task's vector state with: a whole `z`
    /// or `p` register loaded or stored at a number of vector lengths from a base, `pfalse`, which
    /// clears a predicate, and `rdffr` and `wrffr`, which move the first fault register to and from
    /// one. Any other line with an SVE register in it has no encoding.
    fn sve(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let m = self.mnemonic;
        let store = if m == "str" { 0x6000_0000 } else { 0 };
        let word = match (m, values) {
            ("ldr" | "str", [Value::Z(t), Value::Mem(addr)]) => {
                0x8580_4000 | store | self.vector_lengths(addr)? | u32::from(*t)
            }
            ("ldr" | "str", [Value::Pred(t, None), Value::Mem(addr)]) => {
                0x8580_0000 | store | self.vector_lengths(addr)? | u32::from(*t)
            }
            // The SME array is loaded and stored a slice at a time, and the address moves by as
            // many vector lengths as the slice number does, which the text writes twice.
            ("ldr" | "str", [Value::Za(v, slice), Value::Mem(addr)]) => {
                let times = match addr.offset {
                    Offset::Imm(0) => 0,
                    Offset::Vl(times) => times,
                    _ => return Err(self.unwritten()),
                };
                if addr.mode != Mode::Offset || times != i64::from(*slice) {
                    return Err(self.unwritten());
                }
                let store = if m == "str" { 0x0020_0000 } else { 0 };
                0xe100_0000
                    | store
                    | u32::from(v - 12) << 13
                    | u32::from(addr.base) << 5
                    | u32::from(*slice)
            }
            ("pfalse", [Value::Pred(d, Some(Scalar::B))]) => 0x2518_e400 | u32::from(*d),
            ("rdffr", [Value::Pred(d, Some(Scalar::B))]) => 0x2519_f000 | u32::from(*d),
            ("wrffr", [Value::Pred(n, Some(Scalar::B))]) => 0x2528_9000 | u32::from(*n) << 5,
            _ if values
                .iter()
                .any(|value| matches!(value, Value::Z(_) | Value::Pred(..) | Value::Za(..))) =>
            {
                return Err(self.unwritten());
            }
            _ => return Ok(None),
        };
        Ok(Some(word))
    }

    /// The base and the nine bit signed number of vector lengths of an SVE load or store, which
    /// the encoding splits into its top six bits and its bottom three.
    fn vector_lengths(self, addr: &Addr) -> Result<u32, Error> {
        let times = match (addr.offset, addr.mode) {
            (Offset::Imm(0), Mode::Offset) => 0,
            (Offset::Vl(times), Mode::Offset) => times,
            _ => return Err(self.unwritten()),
        };
        if !(-256..256).contains(&times) {
            return Err(self.immediate(times));
        }
        let field = (times & 0x1ff) as u32;
        Ok(field >> 3 << 16 | (field & 7) << 10 | u32::from(addr.base) << 5)
    }

    /// A general register where thirty one is the zero register.
    fn zr(self, value: &Value) -> Result<(Width, u32), Error> {
        match *value {
            Value::Gpr(width, number) if number < 32 => Ok((width, u32::from(number))),
            Value::Sp(_) => Err(self.register()),
            _ => Err(self.unwritten()),
        }
    }

    /// A general register where thirty one is the stack pointer.
    fn sp(self, value: &Value) -> Result<(Width, u32), Error> {
        match *value {
            Value::Gpr(width, number) if number < 31 => Ok((width, u32::from(number))),
            Value::Sp(width) => Ok((width, 31)),
            Value::Gpr(_, _) => Err(self.register()),
            _ => Err(self.unwritten()),
        }
    }

    /// Two widths that have to be the same.
    fn same(self, a: Width, b: Width) -> Result<Width, Error> {
        if a == b { Ok(a) } else { Err(self.register()) }
    }

    /// A number that has to be in a range.
    fn number(self, imm: i64, below: i64) -> Result<u32, Error> {
        if (0..below).contains(&imm) { Ok(imm as u32) } else { Err(self.immediate(imm)) }
    }

    /// A vector register read as a scalar in a precision the floating point instructions have.
    fn float(self, value: &Value) -> Result<(u32, u32), Error> {
        match *value {
            Value::Fp(scalar, number) => {
                Ok((scalar.ftype().ok_or_else(|| self.unwritten())?, u32::from(number)))
            }
            _ => Err(self.unwritten()),
        }
    }

    fn encode(self, values: &[Value]) -> Result<Word, Error> {
        let m = self.mnemonic;
        if neon::wanted(m, values) {
            return self.neon(values).map(|word| (word, None));
        }
        if let Some(atomic) = atomic::wanted(m) {
            return self.atomic(atomic, values).map(|word| (word, None));
        }
        if let Some(cond) = m.strip_prefix("b.") {
            let cond = Cond::named(cond).ok_or_else(|| self.unwritten())?;
            return match values {
                [Value::Symbol(Operator::Plain)] => {
                    Ok((0x5400_0000 | cond.bits(), Some(Fixup::CondBr19)))
                }
                _ => Err(self.unwritten()),
            };
        }
        if let Some(word) = self.mops(values)? {
            return Ok((word, None));
        }
        if let Some(word) = self.sve(values)? {
            return Ok((word, None));
        }
        let word = match m {
            "add" => return self.arith(false, false, values),
            "adds" => return self.arith(false, true, values),
            "sub" => return self.arith(true, false, values),
            "subs" => return self.arith(true, true, values),
            "cmp" | "cmn" => {
                let width = match values.first() {
                    Some(Value::Sp(width) | Value::Gpr(width, _)) => *width,
                    _ => return Err(self.unwritten()),
                };
                let mut all = vec![Value::Gpr(width, 31)];
                all.extend_from_slice(values);
                return self.arith(m == "cmp", true, &all);
            }
            "neg" | "negs" => match values {
                [d, rest @ ..] if !rest.is_empty() => {
                    let (width, _) = self.zr(d)?;
                    let mut all = vec![*d, Value::Gpr(width, 31)];
                    all.extend_from_slice(rest);
                    return self.arith(true, m == "negs", &all);
                }
                _ => return Err(self.unwritten()),
            },
            "mov" => return self.mov(values),
            "and" | "bic" => self.logical(0b00, m == "bic", values)?,
            "orr" | "orn" => self.logical(0b01, m == "orn", values)?,
            "eor" | "eon" => self.logical(0b10, m == "eon", values)?,
            "ands" | "bics" => self.logical(0b11, m == "bics", values)?,
            "tst" => match values {
                [n, rest @ ..] => {
                    let (width, _) = self.zr(n)?;
                    let mut all = vec![Value::Gpr(width, 31), *n];
                    all.extend_from_slice(rest);
                    self.logical(0b11, false, &all)?
                }
                [] => return Err(self.unwritten()),
            },
            "mvn" => match values {
                [d, rest @ ..] => {
                    let (width, _) = self.zr(d)?;
                    let mut all = vec![*d, Value::Gpr(width, 31)];
                    all.extend_from_slice(rest);
                    self.logical(0b01, true, &all)?
                }
                [] => return Err(self.unwritten()),
            },
            "movz" => self.wide(0b10, values)?,
            "movn" => self.wide(0b00, values)?,
            "movk" => self.wide(0b11, values)?,
            "madd" | "msub" | "mul" | "mneg" => self.multiply(values)?,
            "smaddl" | "umaddl" | "smsubl" | "umsubl" | "smull" | "umull" => self.long(values)?,
            "smulh" | "umulh" => match values {
                [d, n, mm] => {
                    let ((wd, rd), (wn, rn), (wm, rmm)) = (self.zr(d)?, self.zr(n)?, self.zr(mm)?);
                    if [wd, wn, wm] != [Width::X; 3] {
                        return Err(self.register());
                    }
                    let base = if m == "smulh" { 0x9b40_7c00 } else { 0x9bc0_7c00 };
                    base | rmm << 16 | rn << 5 | rd
                }
                _ => return Err(self.unwritten()),
            },
            "adc" => self.carry(0x1a00_0000, values)?,
            "adcs" => self.carry(0x3a00_0000, values)?,
            "sbc" => self.carry(0x5a00_0000, values)?,
            "sbcs" => self.carry(0x7a00_0000, values)?,
            // Taking away from zero with the borrow, which is `sbc` with the zero register first.
            "ngc" | "ngcs" => match values {
                [d, m] => {
                    let base = if self.mnemonic == "ngc" { 0x5a00_0000 } else { 0x7a00_0000 };
                    let (width, _) = self.zr(d)?;
                    self.carry(base, &[*d, Value::Gpr(width, 31), *m])?
                }
                _ => return Err(self.unwritten()),
            },
            "sdiv" => self.two_source(0b00_0011, values)?,
            "udiv" => self.two_source(0b00_0010, values)?,
            "lslv" => self.two_source(0b00_1000, values)?,
            "lsrv" => self.two_source(0b00_1001, values)?,
            "asrv" => self.two_source(0b00_1010, values)?,
            "rorv" => self.two_source(0b00_1011, values)?,
            "crc32b" | "crc32h" | "crc32w" | "crc32x" | "crc32cb" | "crc32ch" | "crc32cw"
            | "crc32cx" => self.crc(values)?,
            "lsl" | "lsr" | "asr" | "ror" => self.shift(values)?,
            "sxtb" | "sxth" | "sxtw" | "uxtb" | "uxth" | "uxtw" => self.extend(values)?,
            "ubfm" | "sbfm" | "bfm" | "ubfx" | "sbfx" | "bfxil" | "bfi" | "ubfiz" | "sbfiz" => {
                self.bitfield(values)?
            }
            "extr" => match values {
                [d, n, mm, Value::Imm(lsb)] => {
                    let ((wd, rd), (wn, rn), (wm, rmm)) = (self.zr(d)?, self.zr(n)?, self.zr(mm)?);
                    let width = self.same(self.same(wd, wn)?, wm)?;
                    let lsb = self.number(*lsb, i64::from(width.bits()))?;
                    extr(width, rd, rn, rmm, lsb)
                }
                _ => return Err(self.unwritten()),
            },
            "csel" | "csinc" | "csinv" | "csneg" => match values {
                [d, n, mm, Value::Cond(cond)] => self.select(m, d, n, mm, *cond)?,
                _ => return Err(self.unwritten()),
            },
            "cset" | "csetm" => match values {
                [d, Value::Cond(cond)] => {
                    let (width, _) = self.zr(d)?;
                    let zero = Value::Gpr(width, 31);
                    let invert = cond.invert().ok_or_else(|| self.unwritten())?;
                    let base = if m == "cset" { "csinc" } else { "csinv" };
                    self.select(base, d, &zero, &zero, invert)?
                }
                _ => return Err(self.unwritten()),
            },
            "cinc" | "cinv" | "cneg" => match values {
                [d, n, Value::Cond(cond)] => {
                    let invert = cond.invert().ok_or_else(|| self.unwritten())?;
                    let base = match m {
                        "cinc" => "csinc",
                        "cinv" => "csinv",
                        _ => "csneg",
                    };
                    self.select(base, d, n, n, invert)?
                }
                _ => return Err(self.unwritten()),
            },
            "ccmp" | "ccmn" => self.compare_conditionally(values)?,
            "rbit" => self.one_source(|_| 0b00_0000, values)?,
            "rev16" => self.one_source(|_| 0b00_0001, values)?,
            "rev32" => match values {
                [Value::Gpr(Width::X, _), _] => self.one_source(|_| 0b00_0010, values)?,
                _ => return Err(self.unwritten()),
            },
            "rev" => self.one_source(|width| 0b10 | width.sf(), values)?,
            "clz" => self.one_source(|_| 0b00_0100, values)?,
            "cls" => self.one_source(|_| 0b00_0101, values)?,
            "ldp" | "stp" | "ldpsw" | "ldnp" | "stnp" => self.pair(values)?,
            "ldxr" | "ldxrb" | "ldxrh" | "ldaxr" | "ldaxrb" | "ldaxrh" | "ldar" | "ldarb"
            | "ldarh" | "stlr" | "stlrb" | "stlrh" => self.exclusive(None, values)?,
            "ldapr" | "ldaprb" | "ldaprh" => self.rcpc(values)?,
            "ldxp" | "ldaxp" => self.exclusive_pair(None, values)?,
            "stxp" | "stlxp" => match values {
                [s, rest @ ..] => {
                    let (width, rs) = self.zr(s)?;
                    if width != Width::W {
                        return Err(self.register());
                    }
                    self.exclusive_pair(Some(rs), rest)?
                }
                [] => return Err(self.unwritten()),
            },
            // How long a vector is, in bytes or in predicate bits, times a number, which the
            // kernel reads to size the state of SVE and SME it saves.
            "rdvl" | "rdsvl" => match values {
                [d, Value::Imm(imm)] => {
                    let (width, rd) = self.zr(d)?;
                    if width != Width::X {
                        return Err(self.register());
                    }
                    let base = if m == "rdvl" { 0x04bf_5000 } else { 0x04bf_5800 };
                    base | self.vector_times(*imm)? << 5 | rd
                }
                _ => return Err(self.unwritten()),
            },
            "addvl" | "addpl" => match values {
                [d, n, Value::Imm(imm)] => {
                    let (width, rd) = self.sp(d)?;
                    let (other, rn) = self.sp(n)?;
                    if width != Width::X || other != Width::X {
                        return Err(self.register());
                    }
                    let base = if m == "addvl" { 0x0420_5000 } else { 0x0460_5000 };
                    base | rn << 16 | self.vector_times(*imm)? << 5 | rd
                }
                _ => return Err(self.unwritten()),
            },
            "stxr" | "stxrb" | "stxrh" | "stlxr" | "stlxrb" | "stlxrh" => match values {
                [s, rest @ ..] => {
                    let (width, rs) = self.zr(s)?;
                    if width != Width::W {
                        return Err(self.register());
                    }
                    self.exclusive(Some(rs), rest)?
                }
                [] => return Err(self.unwritten()),
            },
            "b" | "bl" => match values {
                [Value::Symbol(Operator::Plain)] => {
                    return Ok(if m == "b" {
                        (0x1400_0000, Some(Fixup::Jump26))
                    } else {
                        (0x9400_0000, Some(Fixup::Call26))
                    });
                }
                _ => return Err(self.unwritten()),
            },
            "cbz" | "cbnz" => match values {
                [t, Value::Symbol(Operator::Plain)] => {
                    let (width, rt) = self.zr(t)?;
                    let op = u32::from(m == "cbnz");
                    return Ok((
                        width.sf() << 31 | 0x3400_0000 | op << 24 | rt,
                        Some(Fixup::CondBr19),
                    ));
                }
                _ => return Err(self.unwritten()),
            },
            "tbz" | "tbnz" => match values {
                [t, Value::Imm(bit), Value::Symbol(Operator::Plain)] => {
                    let (width, rt) = self.zr(t)?;
                    let bit = self.number(*bit, i64::from(width.bits()))?;
                    let op = u32::from(m == "tbnz");
                    return Ok((
                        (bit >> 5) << 31 | 0x3600_0000 | op << 24 | (bit & 31) << 19 | rt,
                        Some(Fixup::TestBr14),
                    ));
                }
                _ => return Err(self.unwritten()),
            },
            "adr" | "adrp" => match values {
                [d, Value::Symbol(op)] => {
                    let (width, rd) = self.zr(d)?;
                    if width != Width::X {
                        return Err(self.register());
                    }
                    let fixup = match (m, op) {
                        ("adr", Operator::Plain) => Fixup::AdrLo21,
                        ("adrp", Operator::Plain) => Fixup::AdrPage21,
                        ("adrp", Operator::Got) => Fixup::GotPage21,
                        ("adrp", Operator::GotTprel) => Fixup::GotTprelPage21,
                        _ => return Err(self.unwritten()),
                    };
                    let page = u32::from(m == "adrp");
                    return Ok((page << 31 | 0x1000_0000 | rd, Some(fixup)));
                }
                _ => return Err(self.unwritten()),
            },
            "br" | "blr" | "ret" => {
                let rn = match values {
                    [] if m == "ret" => 30,
                    [n] => match self.zr(n)? {
                        (Width::X, rn) => rn,
                        _ => return Err(self.register()),
                    },
                    _ => return Err(self.unwritten()),
                };
                let op = match m {
                    "br" => 0b00,
                    "blr" => 0b01,
                    _ => 0b10,
                };
                0xd61f_0000 | op << 21 | rn << 5
            }
            // The hint space, where a machine without the feature a hint is for runs it as a nop.
            // That is why the pointer authentication a return address is signed with and the
            // landing pads of branch target identification are written here and nowhere else.
            "hint" => match values {
                [Value::Imm(imm)] => 0xd503_201f | self.number(*imm, 128)? << 5,
                _ => return Err(self.unwritten()),
            },
            _ if values.is_empty() && hint(m).is_some() => 0xd503_201f | hint(m).unwrap_or(0) << 5,
            "nop" | "yield" | "isb" if values.is_empty() => match m {
                "nop" => 0xd503_201f,
                "yield" => 0xd503_203f,
                _ => 0xd503_3fdf,
            },
            "brk" | "svc" | "hvc" | "smc" | "hlt" => match values {
                [Value::Imm(imm)] => {
                    let imm = self.number(*imm, 1 << 16)?;
                    let base = match m {
                        "brk" => 0xd420_0000,
                        "svc" => 0xd400_0001,
                        "hvc" => 0xd400_0002,
                        "smc" => 0xd400_0003,
                        _ => 0xd440_0000,
                    };
                    base | imm << 5
                }
                _ => return Err(self.unwritten()),
            },
            "dmb" | "dsb" | "isb" => match values {
                [Value::Barrier(option)] if *option < 16 => {
                    let base = match m {
                        "dmb" => 0xd503_30bf,
                        "dsb" => 0xd503_309f,
                        _ => 0xd503_30df,
                    };
                    base | u32::from(*option) << 8
                }
                [Value::Imm(option)] => {
                    let base = match m {
                        "dmb" => 0xd503_30bf,
                        "dsb" => 0xd503_309f,
                        _ => 0xd503_30df,
                    };
                    base | self.number(*option, 16)? << 8
                }
                _ => return Err(self.unwritten()),
            },
            // The barriers with no option. `ssbb` and `pssbb` are `dsb` with the two numbers no
            // option has a name for.
            "clrex" | "sb" | "ssbb" | "pssbb" | "eret" | "eretaa" | "eretab" | "drps"
                if values.is_empty() =>
            {
                match m {
                    "clrex" => 0xd503_3f5f,
                    "sb" => 0xd503_30ff,
                    "ssbb" => 0xd503_309f,
                    "pssbb" => 0xd503_349f,
                    "eret" => 0xd69f_03e0,
                    "eretaa" => 0xd69f_0bff,
                    "eretab" => 0xd69f_0fff,
                    _ => 0xd6bf_03e0,
                }
            }
            "clrex" => match values {
                [Value::Imm(imm)] => 0xd503_305f | self.number(*imm, 16)? << 8,
                _ => return Err(self.unwritten()),
            },
            // The system instructions, which `tlbi`, `ic`, `dc` and `at` were read as.
            "sys" | "sysl" => {
                let (fields, t) = match (m, values) {
                    ("sys", [op1, crn, crm, op2, t]) => ([op1, crn, crm, op2], t),
                    ("sysl", [t, op1, crn, crm, op2]) => ([op1, crn, crm, op2], t),
                    _ => return Err(self.unwritten()),
                };
                let mut word = if m == "sys" { 0xd508_0000 } else { 0xd528_0000 };
                for (field, (shift, below)) in
                    fields.into_iter().zip([(16, 8), (12, 16), (8, 16), (5, 8)])
                {
                    let Value::Imm(field) = field else {
                        return Err(self.unwritten());
                    };
                    word |= self.number(*field, below)? << shift;
                }
                match self.zr(t)? {
                    (Width::X, rt) => word | rt,
                    _ => return Err(self.register()),
                }
            }
            "mrs" => match values {
                [t, Value::System(field)] => {
                    let (width, rt) = self.zr(t)?;
                    if width != Width::X {
                        return Err(self.register());
                    }
                    0xd520_0000 | u32::from(*field) << 5 | rt
                }
                _ => return Err(self.unwritten()),
            },
            "msr" => match values {
                // A field of the processor state from an immediate, in the space beside the hints,
                // with the immediate in `CRm`. The ones that are a single bit take the top three
                // bits of `CRm` too.
                [Value::Pstate(at), Value::Imm(imm)] => {
                    let Some(&(_, field, high)) = PSTATE.get(usize::from(*at)) else {
                        return Err(self.unwritten());
                    };
                    let crm = match high {
                        None => self.number(*imm, 16)?,
                        Some(high) => u32::from(high) << 1 | self.number(*imm, 2)?,
                    };
                    let (op1, op2) = (u32::from(field >> 3), u32::from(field & 7));
                    0xd500_401f | op1 << 16 | crm << 8 | op2 << 5
                }
                [Value::System(field), t] => {
                    let (width, rt) = self.zr(t)?;
                    if width != Width::X {
                        return Err(self.register());
                    }
                    0xd500_0000 | u32::from(*field) << 5 | rt
                }
                _ => return Err(self.unwritten()),
            },
            "fmul" => self.float_two(0b0000, values)?,
            "fdiv" => self.float_two(0b0001, values)?,
            "fadd" => self.float_two(0b0010, values)?,
            "fsub" => self.float_two(0b0011, values)?,
            "fmax" => self.float_two(0b0100, values)?,
            "fmin" => self.float_two(0b0101, values)?,
            "fmaxnm" => self.float_two(0b0110, values)?,
            "fminnm" => self.float_two(0b0111, values)?,
            "fnmul" => self.float_two(0b1000, values)?,
            "fabs" => self.float_one(0b00_0001, values)?,
            "fneg" => self.float_one(0b00_0010, values)?,
            "fsqrt" => self.float_one(0b00_0011, values)?,
            "frintn" => self.float_one(0b00_1000, values)?,
            "frintp" => self.float_one(0b00_1001, values)?,
            "frintm" => self.float_one(0b00_1010, values)?,
            "frintz" => self.float_one(0b00_1011, values)?,
            "frinta" => self.float_one(0b00_1100, values)?,
            "frintx" => self.float_one(0b00_1110, values)?,
            "frinti" => self.float_one(0b00_1111, values)?,
            "fmadd" | "fmsub" | "fnmadd" | "fnmsub" => match values {
                [d, n, mm, a] => {
                    let ((td, rd), (tn, rn), (tm, rmm), (ta, ra)) =
                        (self.float(d)?, self.float(n)?, self.float(mm)?, self.float(a)?);
                    if [tn, tm, ta] != [td; 3] {
                        return Err(self.register());
                    }
                    let (o1, o0) = match m {
                        "fmadd" => (0, 0),
                        "fmsub" => (0, 1),
                        "fnmadd" => (1, 0),
                        _ => (1, 1),
                    };
                    0x1f00_0000
                        | td << 22
                        | o1 << 21
                        | rmm << 16
                        | o0 << 15
                        | ra << 10
                        | rn << 5
                        | rd
                }
                _ => return Err(self.unwritten()),
            },
            "fcmp" | "fcmpe" => {
                let signalling = if m == "fcmpe" { 0b1_0000 } else { 0 };
                match values {
                    [n, Value::Float(zero)] if *zero == 0.0 => {
                        let (tn, rn) = self.float(n)?;
                        0x1e20_2000 | tn << 22 | rn << 5 | 0b1000 | signalling
                    }
                    [n, mm] => {
                        let ((tn, rn), (tm, rmm)) = (self.float(n)?, self.float(mm)?);
                        if tn != tm {
                            return Err(self.register());
                        }
                        0x1e20_2000 | tn << 22 | rmm << 16 | rn << 5 | signalling
                    }
                    _ => return Err(self.unwritten()),
                }
            }
            "fccmp" | "fccmpe" => match values {
                [n, mm, Value::Imm(nzcv), Value::Cond(cond)] => {
                    let ((tn, rn), (tm, rmm)) = (self.float(n)?, self.float(mm)?);
                    if tn != tm {
                        return Err(self.register());
                    }
                    let nzcv = self.number(*nzcv, 16)?;
                    let signalling = if m == "fccmpe" { 0b1_0000 } else { 0 };
                    0x1e20_0400
                        | tn << 22
                        | rmm << 16
                        | cond.bits() << 12
                        | rn << 5
                        | signalling
                        | nzcv
                }
                _ => return Err(self.unwritten()),
            },
            "fcsel" => match values {
                [d, n, mm, Value::Cond(cond)] => {
                    let ((td, rd), (tn, rn), (tm, rmm)) =
                        (self.float(d)?, self.float(n)?, self.float(mm)?);
                    if [tn, tm] != [td; 2] {
                        return Err(self.register());
                    }
                    0x1e20_0c00 | td << 22 | rmm << 16 | cond.bits() << 12 | rn << 5 | rd
                }
                _ => return Err(self.unwritten()),
            },
            "fmov" => self.fmov(values)?,
            "fcvt" => match values {
                [d, n] => {
                    let ((td, rd), (tn, rn)) = (self.float(d)?, self.float(n)?);
                    if td == tn {
                        return Err(self.register());
                    }
                    0x1e20_4000 | tn << 22 | (0b00_0100 | td) << 15 | rn << 5 | rd
                }
                _ => return Err(self.unwritten()),
            },
            "scvtf" | "ucvtf" => match values {
                [d, n] => {
                    let ((td, rd), (width, rn)) = (self.float(d)?, self.zr(n)?);
                    let op = u32::from(m == "ucvtf");
                    width.sf() << 31 | 0x1e22_0000 | td << 22 | op << 16 | rn << 5 | rd
                }
                _ => return Err(self.unwritten()),
            },
            "fcvtns" | "fcvtnu" | "fcvtps" | "fcvtpu" | "fcvtms" | "fcvtmu" | "fcvtzs"
            | "fcvtzu" | "fcvtas" | "fcvtau" => match values {
                [d, n] => {
                    let ((width, rd), (tn, rn)) = (self.zr(d)?, self.float(n)?);
                    let (rmode, opcode) = match &m[4..] {
                        "ns" => (0b00, 0b000),
                        "nu" => (0b00, 0b001),
                        "ps" => (0b01, 0b000),
                        "pu" => (0b01, 0b001),
                        "ms" => (0b10, 0b000),
                        "mu" => (0b10, 0b001),
                        "zs" => (0b11, 0b000),
                        "zu" => (0b11, 0b001),
                        "as" => (0b00, 0b100),
                        _ => (0b00, 0b101),
                    };
                    width.sf() << 31
                        | 0x1e20_0000
                        | tn << 22
                        | rmode << 19
                        | opcode << 16
                        | rn << 5
                        | rd
                }
                _ => return Err(self.unwritten()),
            },
            _ => return self.memory(values),
        };
        Ok((word, None))
    }

    /// `add`, `adds`, `sub` and `subs`, in all three of their forms.
    fn arith(self, sub: bool, flags: bool, values: &[Value]) -> Result<Word, Error> {
        let (sub, flags) = (u32::from(sub), u32::from(flags));
        // The destination is the stack pointer at thirty one unless the instruction sets the
        // flags, in which case it is the zero register, and that is what makes `cmp` work.
        let dest = |value: &Value| if flags == 1 { self.zr(value) } else { self.sp(value) };
        match values {
            [d, n, Value::Imm(imm), rest @ ..] => {
                let ((wd, rd), (wn, rn)) = (dest(d)?, self.sp(n)?);
                let width = self.same(wd, wn)?;
                let mut high = match rest {
                    [] | [Value::Shift(Shift::Lsl, 0)] => 0,
                    [Value::Shift(Shift::Lsl, 12)] => 1,
                    _ => return Err(self.unwritten()),
                };
                // A negative number is the other instruction with the positive one, which is
                // what GNU as writes for it.
                let (sub, mut imm) = if *imm < 0 && high == 0 {
                    (sub ^ 1, imm.checked_neg().ok_or_else(|| self.immediate(*imm))?)
                } else {
                    (sub, *imm)
                };
                if high == 0 && imm >= 1 << 12 && imm & 0xfff == 0 {
                    high = 1;
                    imm >>= 12;
                }
                let imm = self.number(imm, 1 << 12)?;
                let word = width.sf() << 31
                    | sub << 30
                    | flags << 29
                    | 0x1100_0000
                    | high << 22
                    | imm << 10
                    | rn << 5
                    | rd;
                Ok((word, None))
            }
            [d, n, Value::Symbol(op), rest @ ..] if sub == 0 && flags == 0 => {
                let ((wd, rd), (wn, rn)) = (dest(d)?, self.sp(n)?);
                let width = self.same(wd, wn)?;
                let (fixup, high) = match (op, rest) {
                    (Operator::Lo12, []) => (Fixup::AddLo12, 0),
                    (Operator::TprelLo12Nc, []) => (Fixup::TprelLo12Nc, 0),
                    (Operator::TprelHi12, [Value::Shift(Shift::Lsl, 12)]) => (Fixup::TprelHi12, 1),
                    (Operator::SecrelLo12, []) => (Fixup::SecrelLow12A, 0),
                    // With the shift said or not, since clang writes it without and reads both.
                    (Operator::SecrelHi12, [] | [Value::Shift(Shift::Lsl, 12)]) => {
                        (Fixup::SecrelHigh12A, 1)
                    }
                    _ => return Err(self.unwritten()),
                };
                Ok((width.sf() << 31 | 0x1100_0000 | high << 22 | rn << 5 | rd, Some(fixup)))
            }
            [d, n, mm, rest @ ..] => {
                let wants_extend = matches!(rest, [Value::Extend(_, _)])
                    || matches!(d, Value::Sp(_))
                    || matches!(n, Value::Sp(_));
                if wants_extend {
                    let ((wd, rd), (wn, rn), (wm, rmm)) = (dest(d)?, self.sp(n)?, self.zr(mm)?);
                    let width = self.same(wd, wn)?;
                    let (extend, amount) = match rest {
                        [] => (default_extend(width), 0),
                        [Value::Shift(Shift::Lsl, amount)] => (default_extend(width), *amount),
                        [Value::Extend(extend, amount)] => (*extend, amount.unwrap_or(0)),
                        _ => return Err(self.unwritten()),
                    };
                    // A thirty two bit instruction only has `w` registers to widen.
                    let reads =
                        if width == Width::W || extend.reads_w() { Width::W } else { Width::X };
                    if wm != reads {
                        return Err(self.register());
                    }
                    let amount = self.number(i64::from(amount), 5)?;
                    let word = width.sf() << 31
                        | sub << 30
                        | flags << 29
                        | 0x0b20_0000
                        | rmm << 16
                        | extend.option() << 13
                        | amount << 10
                        | rn << 5
                        | rd;
                    return Ok((word, None));
                }
                let ((wd, rd), (wn, rn), (wm, rmm)) = (self.zr(d)?, self.zr(n)?, self.zr(mm)?);
                let width = self.same(self.same(wd, wn)?, wm)?;
                let (shift, amount) = match rest {
                    [] => (0, 0),
                    [Value::Shift(Shift::Ror, _)] => return Err(self.unwritten()),
                    [Value::Shift(shift, amount)] => (*shift as u32, *amount),
                    _ => return Err(self.unwritten()),
                };
                let amount = self.number(i64::from(amount), i64::from(width.bits()))?;
                let word = width.sf() << 31
                    | sub << 30
                    | flags << 29
                    | 0x0b00_0000
                    | shift << 22
                    | rmm << 16
                    | amount << 10
                    | rn << 5
                    | rd;
                Ok((word, None))
            }
            _ => Err(self.unwritten()),
        }
    }

    /// `mov`, which is five instructions depending on what it is given.
    fn mov(self, values: &[Value]) -> Result<Word, Error> {
        let word = match values {
            [d @ Value::Sp(_), n] | [d, n @ Value::Sp(_)] => {
                let ((wd, rd), (wn, rn)) = (self.sp(d)?, self.sp(n)?);
                self.same(wd, wn)?.sf() << 31 | 0x1100_0000 | rn << 5 | rd
            }
            [d @ Value::Gpr(_, _), n @ Value::Gpr(_, _)] => {
                let ((wd, rd), (wn, rn)) = (self.zr(d)?, self.zr(n)?);
                self.same(wd, wn)?.sf() << 31 | 0x2a00_03e0 | rn << 16 | rd
            }
            [d, Value::Imm(imm)] => {
                let (width, rd) = match *d {
                    Value::Gpr(width, number) => (width, u32::from(number)),
                    Value::Sp(width) => (width, 31),
                    _ => return Err(self.unwritten()),
                };
                let value = narrow(*imm, width).ok_or_else(|| self.immediate(*imm))?;
                if !matches!(d, Value::Sp(_)) {
                    if let Some(word) = wide_move(width, rd, value) {
                        return Ok((word, None));
                    }
                }
                let (n, immr, imms) = bitmask(value, width).ok_or_else(|| self.immediate(*imm))?;
                width.sf() << 31 | 0x3200_0000 | n << 22 | immr << 16 | imms << 10 | 31 << 5 | rd
            }
            _ => return Err(self.unwritten()),
        };
        Ok((word, None))
    }

    /// The logical instructions, with a register or with a pattern of bits.
    fn logical(self, opc: u32, invert: bool, values: &[Value]) -> Result<u32, Error> {
        match values {
            [d, n, Value::Imm(imm)] => {
                // `and` with a pattern can write the stack pointer, which is how a frame is
                // aligned. `ands` cannot, since its thirty one is the zero register.
                let (wd, rd) = if opc == 0b11 { self.zr(d)? } else { self.sp(d)? };
                let (wn, rn) = self.zr(n)?;
                let width = self.same(wd, wn)?;
                let value = narrow(*imm, width).ok_or_else(|| self.immediate(*imm))?;
                let value = if invert { !value & mask(width) } else { value };
                let (n, immr, imms) = bitmask(value, width).ok_or_else(|| self.immediate(*imm))?;
                Ok(width.sf() << 31
                    | opc << 29
                    | 0x1200_0000
                    | n << 22
                    | immr << 16
                    | imms << 10
                    | rn << 5
                    | rd)
            }
            [d, n, mm, rest @ ..] => {
                let ((wd, rd), (wn, rn), (wm, rmm)) = (self.zr(d)?, self.zr(n)?, self.zr(mm)?);
                let width = self.same(self.same(wd, wn)?, wm)?;
                let (shift, amount) = match rest {
                    [] => (0, 0),
                    [Value::Shift(shift, amount)] => (*shift as u32, *amount),
                    _ => return Err(self.unwritten()),
                };
                let amount = self.number(i64::from(amount), i64::from(width.bits()))?;
                Ok(width.sf() << 31
                    | opc << 29
                    | 0x0a00_0000
                    | shift << 22
                    | u32::from(invert) << 21
                    | rmm << 16
                    | amount << 10
                    | rn << 5
                    | rd)
            }
            _ => Err(self.unwritten()),
        }
    }

    /// `movz`, `movn` and `movk`, with sixteen bits and where they go.
    fn wide(self, opc: u32, values: &[Value]) -> Result<u32, Error> {
        let (d, imm, shift) = match values {
            [d, Value::Imm(imm)] => (d, *imm, 0),
            [d, Value::Imm(imm), Value::Shift(Shift::Lsl, shift)] => (d, *imm, *shift),
            _ => return Err(self.unwritten()),
        };
        let (width, rd) = self.zr(d)?;
        let imm = self.number(imm, 1 << 16)?;
        if shift % 16 != 0 || u32::from(shift) >= width.bits() {
            return Err(self.immediate(i64::from(shift)));
        }
        Ok(width.sf() << 31 | opc << 29 | 0x1280_0000 | u32::from(shift / 16) << 21 | imm << 5 | rd)
    }

    /// `madd` and `msub`, and `mul` and `mneg`, which are them adding to zero.
    fn multiply(self, values: &[Value]) -> Result<u32, Error> {
        let zero = |width| Value::Gpr(width, 31);
        let (d, n, mm, a) = match values {
            [d, n, mm, a] => (d, n, mm, *a),
            [d, n, mm] => (d, n, mm, zero(self.zr(d)?.0)),
            _ => return Err(self.unwritten()),
        };
        let ((wd, rd), (wn, rn), (wm, rmm), (wa, ra)) =
            (self.zr(d)?, self.zr(n)?, self.zr(mm)?, self.zr(&a)?);
        let width = self.same(self.same(self.same(wd, wn)?, wm)?, wa)?;
        let o0 = u32::from(matches!(self.mnemonic, "msub" | "mneg"));
        Ok(width.sf() << 31 | 0x1b00_0000 | rmm << 16 | o0 << 15 | ra << 10 | rn << 5 | rd)
    }

    /// The multiplies that take two thirty two bit numbers and give a sixty four bit one.
    fn long(self, values: &[Value]) -> Result<u32, Error> {
        let (d, n, mm, a) = match values {
            [d, n, mm, a] => (d, n, mm, *a),
            [d, n, mm] => (d, n, mm, Value::Gpr(Width::X, 31)),
            _ => return Err(self.unwritten()),
        };
        let ((wd, rd), (wn, rn), (wm, rmm), (wa, ra)) =
            (self.zr(d)?, self.zr(n)?, self.zr(mm)?, self.zr(&a)?);
        if [wd, wn, wm, wa] != [Width::X, Width::W, Width::W, Width::X] {
            return Err(self.register());
        }
        let m = self.mnemonic;
        let unsigned = u32::from(m.starts_with('u'));
        let o0 = u32::from(m.ends_with("subl"));
        Ok(0x9b20_0000 | unsigned << 23 | rmm << 16 | o0 << 15 | ra << 10 | rn << 5 | rd)
    }

    /// The instructions with two register sources and no other operand.
    /// The adds and subtracts that take the carry flag in, which gcc writes for the upper half of
    /// a sixteen byte integer: `sf op S 11010000 Rm 000000 Rn Rd`, where the base has `op` and `S`.
    fn carry(self, base: u32, values: &[Value]) -> Result<u32, Error> {
        let [d, n, mm] = values else {
            return Err(self.unwritten());
        };
        let ((wd, rd), (wn, rn), (wm, rmm)) = (self.zr(d)?, self.zr(n)?, self.zr(mm)?);
        let width = self.same(self.same(wd, wn)?, wm)?;
        Ok(width.sf() << 31 | base | rmm << 16 | rn << 5 | rd)
    }

    fn two_source(self, opcode: u32, values: &[Value]) -> Result<u32, Error> {
        let [d, n, mm] = values else {
            return Err(self.unwritten());
        };
        let ((wd, rd), (wn, rn), (wm, rmm)) = (self.zr(d)?, self.zr(n)?, self.zr(mm)?);
        let width = self.same(self.same(wd, wn)?, wm)?;
        Ok(width.sf() << 31 | 0x1ac0_0000 | rmm << 16 | opcode << 10 | rn << 5 | rd)
    }

    /// The CRC32 and CRC32C steps from the CRC32 extension, which are two source instructions
    /// with `010`, the polynomial and the size of the data where the others have their opcode:
    /// `sf 0 0 11010110 Rm 010 C sz Rn Rd`, with `C` set for CRC32C, which is the Castagnoli
    /// polynomial, and `sz` counting bytes, halves, words and doublewords.
    ///
    /// The running value in and out is always a `w` register. The data is a `w` register too
    /// except in the `x` forms, which read a whole `x` register and are the only ones with `sf`
    /// set. GNU as refuses any other widths, and so does this.
    fn crc(self, values: &[Value]) -> Result<u32, Error> {
        let rest = self.mnemonic.strip_prefix("crc32").ok_or_else(|| self.unwritten())?;
        let (castagnoli, size) = match rest.strip_prefix('c') {
            Some(size) => (1, size),
            None => (0, rest),
        };
        let sz: u32 = match size {
            "b" => 0b00,
            "h" => 0b01,
            "w" => 0b10,
            "x" => 0b11,
            _ => return Err(self.unwritten()),
        };
        let [d, n, mm] = values else {
            return Err(self.unwritten());
        };
        let ((wd, rd), (wn, rn), (wm, rmm)) = (self.zr(d)?, self.zr(n)?, self.zr(mm)?);
        let data = if sz == 0b11 { Width::X } else { Width::W };
        if [wd, wn, wm] != [Width::W, Width::W, data] {
            return Err(self.register());
        }
        Ok(data.sf() << 31 | 0x1ac0_4000 | rmm << 16 | castagnoli << 12 | sz << 10 | rn << 5 | rd)
    }

    /// The instructions with one register source.
    fn one_source(self, opcode: impl Fn(Width) -> u32, values: &[Value]) -> Result<u32, Error> {
        let [d, n] = values else {
            return Err(self.unwritten());
        };
        let ((wd, rd), (wn, rn)) = (self.zr(d)?, self.zr(n)?);
        let width = self.same(wd, wn)?;
        Ok(width.sf() << 31 | 0x5ac0_0000 | opcode(width) << 10 | rn << 5 | rd)
    }

    /// The shifts, by a register or by a number.
    fn shift(self, values: &[Value]) -> Result<u32, Error> {
        let m = self.mnemonic;
        match values {
            [d, n, Value::Imm(amount)] => {
                let ((wd, rd), (wn, rn)) = (self.zr(d)?, self.zr(n)?);
                let width = self.same(wd, wn)?;
                let bits = width.bits();
                let amount = self.number(*amount, i64::from(bits))?;
                Ok(match m {
                    "lsl" => {
                        bitfield(0b10, width, rd, rn, (bits - amount) % bits, bits - 1 - amount)
                    }
                    "lsr" => bitfield(0b10, width, rd, rn, amount, bits - 1),
                    "asr" => bitfield(0b00, width, rd, rn, amount, bits - 1),
                    _ => extr(width, rd, rn, rn, amount),
                })
            }
            [_, _, _] => {
                let opcode = match m {
                    "lsl" => 0b00_1000,
                    "lsr" => 0b00_1001,
                    "asr" => 0b00_1010,
                    _ => 0b00_1011,
                };
                self.two_source(opcode, values)
            }
            _ => Err(self.unwritten()),
        }
    }

    /// The extensions, which are bitfield moves of the low eight, sixteen or thirty two bits.
    fn extend(self, values: &[Value]) -> Result<u32, Error> {
        let [d, n] = values else {
            return Err(self.unwritten());
        };
        let ((width, rd), (wn, rn)) = (self.zr(d)?, self.zr(n)?);
        if wn != Width::W {
            return Err(self.register());
        }
        let m = self.mnemonic;
        let top = match &m[3..] {
            "b" => 7,
            "h" => 15,
            _ => 31,
        };
        if m == "uxtw" {
            // Writing a `w` register clears the top half, so this is a move and GNU as writes it
            // as one.
            return Ok(0x2a00_03e0 | rn << 16 | rd);
        }
        if m.starts_with('u') {
            // The zero extensions only exist on `w` registers, for the same reason.
            if width != Width::W {
                return Err(self.register());
            }
            return Ok(bitfield(0b10, Width::W, rd, rn, 0, top));
        }
        if top == 31 && width != Width::X {
            return Err(self.register());
        }
        Ok(bitfield(0b00, width, rd, rn, 0, top))
    }

    /// The bitfield moves and the names that are them with a field in place of the two numbers.
    fn bitfield(self, values: &[Value]) -> Result<u32, Error> {
        let [d, n, Value::Imm(a), Value::Imm(b)] = values else {
            return Err(self.unwritten());
        };
        let ((wd, rd), (wn, rn)) = (self.zr(d)?, self.zr(n)?);
        let width = self.same(wd, wn)?;
        let bits = i64::from(width.bits());
        let m = self.mnemonic;
        let opc = match m {
            "sbfm" | "sbfx" | "sbfiz" => 0b00,
            "bfm" | "bfxil" | "bfi" => 0b01,
            _ => 0b10,
        };
        let (immr, imms) = match m {
            "ubfm" | "sbfm" | "bfm" => (*a, *b),
            // A field from `lsb` that is `width` wide, moved down to the bottom.
            "ubfx" | "sbfx" | "bfxil" => {
                if *b < 1 || a + b > bits {
                    return Err(self.immediate(*b));
                }
                (*a, a + b - 1)
            }
            // The bottom `width` bits, moved up to `lsb`.
            _ => {
                if *b < 1 || a + b > bits {
                    return Err(self.immediate(*b));
                }
                ((bits - a) % bits, b - 1)
            }
        };
        let (immr, imms) = (self.number(immr, bits)?, self.number(imms, bits)?);
        Ok(bitfield(opc, width, rd, rn, immr, imms))
    }

    /// The conditional selects.
    fn select(self, m: &str, d: &Value, n: &Value, mm: &Value, cond: Cond) -> Result<u32, Error> {
        let ((wd, rd), (wn, rn), (wm, rmm)) = (self.zr(d)?, self.zr(n)?, self.zr(mm)?);
        let width = self.same(self.same(wd, wn)?, wm)?;
        let (op, o2) = match m {
            "csel" => (0, 0),
            "csinc" => (0, 1),
            "csinv" => (1, 0),
            _ => (1, 1),
        };
        Ok(width.sf() << 31
            | op << 30
            | 0x1a80_0000
            | rmm << 16
            | cond.bits() << 12
            | o2 << 10
            | rn << 5
            | rd)
    }

    /// `ccmp` and `ccmn`, with a register or a five bit number.
    fn compare_conditionally(self, values: &[Value]) -> Result<u32, Error> {
        let [n, mm, Value::Imm(nzcv), Value::Cond(cond)] = values else {
            return Err(self.unwritten());
        };
        let (width, rn) = self.zr(n)?;
        let nzcv = self.number(*nzcv, 16)?;
        let (second, immediate) = match mm {
            Value::Imm(imm) => (self.number(*imm, 32)?, 1),
            other => {
                let (wm, rmm) = self.zr(other)?;
                self.same(width, wm)?;
                (rmm, 0)
            }
        };
        let op = u32::from(self.mnemonic == "ccmp");
        Ok(width.sf() << 31
            | op << 30
            | 0x3a40_0000
            | second << 16
            | cond.bits() << 12
            | immediate << 11
            | rn << 5
            | nzcv)
    }

    /// The floating point instructions with two sources.
    fn float_two(self, opcode: u32, values: &[Value]) -> Result<u32, Error> {
        let [d, n, mm] = values else {
            return Err(self.unwritten());
        };
        let ((td, rd), (tn, rn), (tm, rmm)) = (self.float(d)?, self.float(n)?, self.float(mm)?);
        if [tn, tm] != [td; 2] {
            return Err(self.register());
        }
        Ok(0x1e20_0800 | td << 22 | rmm << 16 | opcode << 12 | rn << 5 | rd)
    }

    /// The floating point instructions with one source of the same precision.
    fn float_one(self, opcode: u32, values: &[Value]) -> Result<u32, Error> {
        let [d, n] = values else {
            return Err(self.unwritten());
        };
        let ((td, rd), (tn, rn)) = (self.float(d)?, self.float(n)?);
        if td != tn {
            return Err(self.register());
        }
        Ok(0x1e20_4000 | td << 22 | opcode << 15 | rn << 5 | rd)
    }

    /// `fmov`, between two vector registers, across the two files, or from a number.
    fn fmov(self, values: &[Value]) -> Result<u32, Error> {
        match values {
            [Value::Fp(_, _), Value::Fp(_, _)] => self.float_one(0, values),
            [d @ Value::Fp(_, _), n @ Value::Gpr(_, _)] => {
                let ((td, rd), (width, rn)) = (self.float(d)?, self.zr(n)?);
                self.across(td, width)?;
                Ok(width.sf() << 31 | 0x1e27_0000 | td << 22 | rn << 5 | rd)
            }
            [d @ Value::Gpr(_, _), n @ Value::Fp(_, _)] => {
                let ((width, rd), (tn, rn)) = (self.zr(d)?, self.float(n)?);
                self.across(tn, width)?;
                Ok(width.sf() << 31 | 0x1e26_0000 | tn << 22 | rn << 5 | rd)
            }
            [d, Value::Float(number)] => {
                let (td, rd) = self.float(d)?;
                let imm8 = float_imm8(*number).ok_or_else(|| self.immediate(*number as i64))?;
                Ok(0x1e20_1000 | td << 22 | imm8 << 13 | rd)
            }
            _ => Err(self.unwritten()),
        }
    }

    /// Whether a move between the files is between two of the same size, which is the only kind
    /// of `fmov` that is not a conversion.
    fn across(self, ftype: u32, width: Width) -> Result<(), Error> {
        match (ftype, width) {
            (0b00, Width::W) | (0b01, Width::X) | (0b11, _) => Ok(()),
            _ => Err(self.register()),
        }
    }

    /// The loads and stores of one register.
    fn memory(self, values: &[Value]) -> Result<Word, Error> {
        let m = self.mnemonic;
        let (t, place) = match values {
            [t, place] => (t, place),
            _ => return Err(self.unwritten()),
        };
        // A prefetch is a load of eight bytes as far as the encoding goes, with the one opc a load
        // of that size does not use, and what it is asked to do where the register would be.
        let prefetch = m == "prfm" || m == "prfum";
        let (access, rt) = match *t {
            Value::Prefetch(operation) if prefetch && operation < 32 => {
                (Access { size: 3, v: 0, opc: 0b10, scale: 3 }, u32::from(operation))
            }
            Value::Gpr(_, number) if !prefetch => (self.access(t)?, u32::from(number)),
            Value::Fp(_, number) if !prefetch => (self.access(t)?, u32::from(number)),
            _ => return Err(self.unwritten()),
        };
        // `ldtr` and `sttr` are the unprivileged accesses, which the kernel reads and writes user
        // memory with. They take the unscaled offset and nothing else, with `10` where `ldur` has
        // `00`, and only the general purpose registers.
        let unprivileged = m.starts_with("ldtr") || m.starts_with("sttr");
        if unprivileged && access.v != 0 {
            return Err(self.register());
        }
        let unscaled_only =
            m.starts_with("ldur") || m.starts_with("stur") || m == "prfum" || unprivileged;
        let front = access.size << 30 | 0b111 << 27 | access.v << 26 | access.opc << 22 | rt;
        let addr = match place {
            Value::Mem(addr) => *addr,
            Value::Symbol(Operator::Plain) => {
                // A load from a place near the instruction. Only the plain `ldr` and `ldrsw` have
                // this form, and only as loads.
                let opc = match (m, access.v, access.scale) {
                    ("ldr", _, 2) => 0b00,
                    ("ldr", _, 3) => 0b01,
                    ("ldr", 1, 4) | ("ldrsw", 0, _) => 0b10,
                    _ => return Err(self.unwritten()),
                };
                return Ok((opc << 30 | 0b011 << 27 | access.v << 26 | rt, Some(Fixup::Literal19)));
            }
            _ => return Err(self.unwritten()),
        };
        let rn = u32::from(addr.base);
        if rn > 31 {
            return Err(self.register());
        }
        let word = match (addr.offset, addr.mode) {
            (Offset::Imm(imm), Mode::Offset) => {
                let step = 1i64 << access.scale;
                let scaled = imm / step;
                if !unscaled_only && imm % step == 0 && (0..1 << 12).contains(&scaled) {
                    front | 1 << 24 | (scaled as u32) << 10 | rn << 5
                } else {
                    let imm9 = signed(imm, 9).ok_or_else(|| self.immediate(imm))?;
                    front | imm9 << 12 | u32::from(unprivileged) << 11 | rn << 5
                }
            }
            (Offset::Imm(imm), mode) if !unscaled_only && !prefetch => {
                let imm9 = signed(imm, 9).ok_or_else(|| self.immediate(imm))?;
                let index = if mode == Mode::Pre { 0b11 } else { 0b01 };
                front | imm9 << 12 | index << 10 | rn << 5
            }
            (Offset::Reg { reg, extend, amount }, Mode::Offset) if !unscaled_only => {
                if !matches!(extend, Extend::Uxtw | Extend::Uxtx | Extend::Sxtw | Extend::Sxtx) {
                    return Err(self.unwritten());
                }
                let s = match amount {
                    None => 0,
                    Some(amount) if u32::from(amount) == access.scale => 1,
                    Some(amount) => return Err(self.immediate(i64::from(amount))),
                };
                front
                    | 1 << 21
                    | u32::from(reg & 31) << 16
                    | extend.option() << 13
                    | s << 12
                    | 0b10 << 10
                    | rn << 5
            }
            (Offset::Symbol(op), Mode::Offset) if !unscaled_only => {
                let fixup = match (op, access.scale) {
                    (Operator::Lo12, 0) => Fixup::Ldst8Lo12,
                    (Operator::Lo12, 1) => Fixup::Ldst16Lo12,
                    (Operator::Lo12, 2) => Fixup::Ldst32Lo12,
                    (Operator::Lo12, 3) => Fixup::Ldst64Lo12,
                    (Operator::Lo12, 4) => Fixup::Ldst128Lo12,
                    (Operator::GotLo12, 3) if m == "ldr" && access.v == 0 => Fixup::GotLo12,
                    (Operator::GotTprelLo12, 3) if m == "ldr" && access.v == 0 => {
                        Fixup::GotTprelLo12Nc
                    }
                    (Operator::SecrelLo12, _) => Fixup::SecrelLow12L,
                    _ => return Err(self.unwritten()),
                };
                return Ok((front | 1 << 24 | rn << 5, Some(fixup)));
            }
            _ => return Err(self.unwritten()),
        };
        Ok((word, None))
    }

    /// What a load or store of one register reads or writes, from its name and its register.
    fn access(self, t: &Value) -> Result<Access, Error> {
        let m = self.mnemonic;
        let load = m.starts_with("ld");
        let base = m.strip_prefix("ld").or_else(|| m.strip_prefix("st")).unwrap_or(m);
        let base = base
            .strip_prefix("ur")
            .or_else(|| base.strip_prefix("tr"))
            .or_else(|| base.strip_prefix('r'));
        let Some(base) = base else {
            return Err(self.unwritten());
        };
        let gpr = |size, opc| Access { size, v: 0, opc, scale: size };
        let access = match (base, *t, load) {
            ("", Value::Gpr(Width::W, _), _) => gpr(2, u32::from(load)),
            ("", Value::Gpr(Width::X, _), _) => gpr(3, u32::from(load)),
            ("", Value::Fp(scalar, _), _) => {
                let (size, opc, scale) = match scalar {
                    Scalar::B => (0, 0, 0),
                    Scalar::H => (1, 0, 1),
                    Scalar::S => (2, 0, 2),
                    Scalar::D => (3, 0, 3),
                    Scalar::Q => (0, 0b10, 4),
                };
                Access { size, v: 1, opc: opc | u32::from(load), scale }
            }
            ("b", Value::Gpr(Width::W, _), _) => gpr(0, u32::from(load)),
            ("h", Value::Gpr(Width::W, _), _) => gpr(1, u32::from(load)),
            ("sb", Value::Gpr(width, _), true) => gpr(0, 0b11 - width.sf()),
            ("sh", Value::Gpr(width, _), true) => gpr(1, 0b11 - width.sf()),
            ("sw", Value::Gpr(Width::X, _), true) => gpr(2, 0b10),
            (_, Value::Gpr(_, _) | Value::Fp(_, _), _) => return Err(self.register()),
            _ => return Err(self.unwritten()),
        };
        Ok(access)
    }

    /// `ldp`, `stp` and `ldpsw`.
    fn pair(self, values: &[Value]) -> Result<u32, Error> {
        let [t, t2, Value::Mem(addr)] = values else {
            return Err(self.unwritten());
        };
        let load = u32::from(self.mnemonic.starts_with("ld"));
        let (opc, v, scale, rt, rt2) = match (*t, *t2) {
            (Value::Gpr(a, rt), Value::Gpr(b, rt2)) if a == b => {
                let signed = self.mnemonic == "ldpsw";
                match (a, signed) {
                    (Width::W, false) => (0b00, 0, 2, rt, rt2),
                    (Width::X, false) => (0b10, 0, 3, rt, rt2),
                    (Width::X, true) => (0b01, 0, 2, rt, rt2),
                    (Width::W, true) => return Err(self.register()),
                }
            }
            (Value::Fp(a, rt), Value::Fp(b, rt2)) if a == b && self.mnemonic != "ldpsw" => {
                match a {
                    Scalar::S => (0b00, 1, 2, rt, rt2),
                    Scalar::D => (0b01, 1, 3, rt, rt2),
                    Scalar::Q => (0b10, 1, 4, rt, rt2),
                    _ => return Err(self.register()),
                }
            }
            _ => return Err(self.register()),
        };
        let Offset::Imm(imm) = addr.offset else {
            return Err(self.unwritten());
        };
        let step = 1i64 << scale;
        if imm % step != 0 {
            return Err(self.immediate(imm));
        }
        let imm7 = signed(imm / step, 7).ok_or_else(|| self.immediate(imm))?;
        // The pair that hints the data will not be wanted again, which the kernel's `copy_page`
        // writes with, has the one mode and a zero where the others say which.
        let streaming = self.mnemonic.ends_with("np");
        let mode = match addr.mode {
            Mode::Offset if streaming => 0b00,
            _ if streaming => return Err(self.unwritten()),
            Mode::Post => 0b01,
            Mode::Offset => 0b10,
            Mode::Pre => 0b11,
        };
        Ok(opc << 30
            | 0b101 << 27
            | v << 26
            | mode << 23
            | load << 22
            | imm7 << 15
            | u32::from(rt2) << 10
            | u32::from(addr.base) << 5
            | u32::from(rt))
    }

    /// The exclusive and the ordered loads and stores, which take a bare base and nothing else.
    fn exclusive(self, status: Option<u32>, values: &[Value]) -> Result<u32, Error> {
        let [t, Value::Mem(addr)] = values else {
            return Err(self.unwritten());
        };
        if addr.offset != Offset::Imm(0) || addr.mode != Mode::Offset {
            return Err(self.unwritten());
        }
        let (width, rt) = self.zr(t)?;
        let m = self.mnemonic;
        let size = match m.as_bytes().last() {
            Some(b'b') => 0b00,
            Some(b'h') => 0b01,
            _ if width == Width::X => 0b11,
            _ => 0b10,
        };
        if size < 0b10 && width != Width::W {
            return Err(self.register());
        }
        let load = u32::from(m.starts_with("ld"));
        // Ordered is bit twenty three, which the plain `ldar` and `stlr` have and the exclusive
        // ones do not, and acquire or release is bit fifteen, which all of them but `ldxr` and
        // `stxr` have.
        let ordered = u32::from(m.starts_with("ldar") || m.starts_with("stlr"));
        let acquire = u32::from(!(m.starts_with("ldxr") || m.starts_with("stxr")));
        let rs = status.unwrap_or(31);
        if status.is_none() && load == 0 && ordered == 0 {
            return Err(self.unwritten());
        }
        Ok(size << 30
            | 0x0800_0000
            | ordered << 23
            | load << 22
            | rs << 16
            | acquire << 15
            | 31 << 10
            | u32::from(addr.base) << 5
            | rt)
    }

    /// The exclusive loads and stores of two registers, which take a bare base the way the ones of
    /// one do and are the halves of the kernel's sixteen byte `cmpxchg`.
    fn exclusive_pair(self, status: Option<u32>, values: &[Value]) -> Result<u32, Error> {
        let [t, t2, Value::Mem(addr)] = values else {
            return Err(self.unwritten());
        };
        if addr.offset != Offset::Imm(0) || addr.mode != Mode::Offset {
            return Err(self.unwritten());
        }
        let (width, rt) = self.zr(t)?;
        let (other, rt2) = self.zr(t2)?;
        if other != width {
            return Err(self.register());
        }
        let m = self.mnemonic;
        let load = u32::from(m.starts_with("ld"));
        let ordered = u32::from(m.starts_with("lda") || m.starts_with("stl"));
        Ok(u32::from(width == Width::X) << 30
            | 0x8820_0000
            | load << 22
            | status.unwrap_or(31) << 16
            | ordered << 15
            | rt2 << 10
            | u32::from(addr.base) << 5
            | rt)
    }

    /// The six bit signed multiple `rdvl` and `addvl` take.
    fn vector_times(self, imm: i64) -> Result<u32, Error> {
        if !(-32..=31).contains(&imm) {
            return Err(self.immediate(imm));
        }
        Ok((imm & 0x3f) as u32)
    }

    /// The loads that acquire only against the stores that release, which ARMv8.3 added and a
    /// kernel built with link time optimization reads every `READ_ONCE` with.
    fn rcpc(self, values: &[Value]) -> Result<u32, Error> {
        let [t, Value::Mem(addr)] = values else {
            return Err(self.unwritten());
        };
        if addr.offset != Offset::Imm(0) || addr.mode != Mode::Offset {
            return Err(self.unwritten());
        }
        let (width, rt) = self.zr(t)?;
        let size = match self.mnemonic {
            "ldaprb" => 0b00,
            "ldaprh" => 0b01,
            _ if width == Width::X => 0b11,
            _ => 0b10,
        };
        if size < 0b10 && width != Width::W {
            return Err(self.register());
        }
        Ok(size << 30 | 0x38bf_c000 | u32::from(addr.base) << 5 | rt)
    }
}

/// What one load or store reads or writes.
struct Access {
    /// The size field, which for a sixteen byte vector access is zero.
    size: u32,
    /// Whether the register is a vector register.
    v: u32,
    /// The field that says load, store or sign extend.
    opc: u32,
    /// How many bytes, as a power of two, which is what an offset is counted in.
    scale: u32,
}

/// The extension that means no extension in an instruction of that width.
/// The number of the hint a name in the hint space is, which `hint` takes as its operand. These
/// are the waits for an interrupt or an event and the sending of one, and the pointer
/// authentication ones that sign and check the return address in x30 against the
/// stack pointer or zero, with the A key or the B key. The landing pads of branch target
/// identification are hints too, which the reader writes as `hint` since `bti` has a name after it.
pub(crate) fn hint(name: &str) -> Option<u32> {
    Some(match name {
        "wfe" => 2,
        "wfi" => 3,
        "sev" => 4,
        "sevl" => 5,
        "dgh" => 6,
        "xpaclri" => 7,
        "pacia1716" => 8,
        "pacib1716" => 10,
        "autia1716" => 12,
        "autib1716" => 14,
        "esb" => 16,
        "csdb" => 20,
        "paciaz" => 24,
        "paciasp" => 25,
        "pacibz" => 26,
        "pacibsp" => 27,
        "autiaz" => 28,
        "autiasp" => 29,
        "autibz" => 30,
        "autibsp" => 31,
        _ => return None,
    })
}

fn default_extend(width: Width) -> Extend {
    match width {
        Width::W => Extend::Uxtw,
        Width::X => Extend::Uxtx,
    }
}

/// Every bit of a register of that width.
fn mask(width: Width) -> u64 {
    match width {
        Width::W => 0xffff_ffff,
        Width::X => u64::MAX,
    }
}

/// A number as the bits a register of that width holds, when it is one: for a `w` register
/// anything from the most negative thirty two bit number to the largest unsigned one.
fn narrow(imm: i64, width: Width) -> Option<u64> {
    match width {
        Width::X => Some(imm as u64),
        Width::W => (-(1i64 << 31)..1i64 << 32).contains(&imm).then_some(imm as u64 & 0xffff_ffff),
    }
}

/// `movz` or `movn` for a number, when one of them can write it, preferring `movz` the way GNU as
/// does.
fn wide_move(width: Width, rd: u32, value: u64) -> Option<u32> {
    let halves = width.bits() / 16;
    let single = |value: u64| {
        (0..halves).find(|&hw| value & !(0xffff << (hw * 16)) == 0).map(|hw| {
            let imm = ((value >> (hw * 16)) & 0xffff) as u32;
            (hw, imm)
        })
    };
    if let Some((hw, imm)) = single(value) {
        return Some(width.sf() << 31 | 0x5280_0000 | hw << 21 | imm << 5 | rd);
    }
    let inverted = !value & mask(width);
    let (hw, imm) = single(inverted)?;
    Some(width.sf() << 31 | 0x1280_0000 | hw << 21 | imm << 5 | rd)
}

/// Whether one of gcc's AArch64 immediate letters in an `asm` constraint takes that constant, as
/// gcc decides it. `I` and `J` are what `add` and `sub` carry, twelve bits shifted by nothing or
/// by twelve, `K` and `L` are the patterns a thirty two and a sixty four bit logical instruction
/// carry, and `M` and `N` are what one `mov` writes at each width. A constant a letter does not
/// take goes in a register, which is the `r` the kernel's atomics write next to every one of them.
/// Any other letter takes any constant.
pub fn takes(letter: char, number: i128) -> bool {
    let add =
        |n: i128| (0..=0xfff).contains(&n) || (n & 0xfff == 0 && (0..=0xff_f000).contains(&n));
    let bits = |width: Width| match (i64::try_from(number), u64::try_from(number)) {
        (Ok(signed), _) => narrow(signed, width),
        (_, Ok(unsigned)) if width == Width::X => Some(unsigned),
        _ => None,
    };
    let logical = |width: Width| bits(width).is_some_and(|value| bitmask(value, width).is_some());
    let moved = |width: Width| {
        logical(width) || bits(width).is_some_and(|value| wide_move(width, 0, value).is_some())
    };
    match letter {
        'I' => add(number),
        'J' => add(-number),
        'K' => logical(Width::W),
        'L' => logical(Width::X),
        'M' => moved(Width::W),
        'N' => moved(Width::X),
        _ => true,
    }
}

/// The three fields a logical instruction carries a pattern of bits in, when it can carry that
/// one.
///
/// A pattern is a run of ones, rotated, repeated to fill the register in pieces of two, four,
/// eight, sixteen, thirty two or sixty four bits. So the piece is found first, by halving while
/// both halves are the same, then how many ones it has, then how far a run of that many at the
/// bottom has to be rotated right to be the piece. Nothing and everything are not patterns.
fn bitmask(value: u64, width: Width) -> Option<(u32, u32, u32)> {
    let value = match width {
        Width::W => (value & 0xffff_ffff) | value << 32,
        Width::X => value,
    };
    if value == 0 || value == u64::MAX {
        return None;
    }
    let mut size = 64u32;
    while size > 2 {
        let half = size / 2;
        let low = (1u64 << half) - 1;
        if value & low != (value >> half) & low {
            break;
        }
        size = half;
    }
    let piece_mask = if size == 64 { u64::MAX } else { (1u64 << size) - 1 };
    let piece = value & piece_mask;
    let ones = piece.count_ones();
    let run = (1u64 << ones) - 1;
    let rotate = |bits: u64, by: u32| {
        if by == 0 { bits } else { ((bits >> by) | (bits << (size - by))) & piece_mask }
    };
    let immr = (0..size).find(|&by| rotate(run, by) == piece)?;
    let n = u32::from(size == 64);
    let imms = (!(size * 2 - 1) & 0x3f) | (ones - 1);
    Some((n, immr, imms))
}

/// A bitfield move.
fn bitfield(opc: u32, width: Width, rd: u32, rn: u32, immr: u32, imms: u32) -> u32 {
    width.sf() << 31
        | opc << 29
        | 0x1300_0000
        | width.sf() << 22
        | immr << 16
        | imms << 10
        | rn << 5
        | rd
}

/// An extraction from a pair of registers, which with the same register twice is a rotation.
fn extr(width: Width, rd: u32, rn: u32, rm: u32, lsb: u32) -> u32 {
    width.sf() << 31 | 0x1380_0000 | width.sf() << 22 | rm << 16 | lsb << 10 | rn << 5 | rd
}

/// The eight bits `fmov` carries a number in, when it can carry that one.
///
/// Those are the numbers that are sixteen to thirty one sixteenths times a power of two from an
/// eighth to sixteen, either sign. There are two hundred and fifty six of them, so this tries each.
fn float_imm8(number: f64) -> Option<u32> {
    (0..256u32).find(|&imm8| {
        let sign = if imm8 & 0x80 != 0 { -1.0 } else { 1.0 };
        let b = (imm8 >> 6) & 1;
        let cd = ((imm8 >> 4) & 3) as i32;
        let exponent = if b == 0 { 1 + cd } else { cd - 3 };
        let fraction = f64::from(16 + (imm8 & 15)) / 16.0;
        sign * fraction * 2f64.powi(exponent) == number
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aarch64::read;

    #[test]
    fn every_word_gnu_as_wrote_is_the_word_written_here() {
        let mut wrong = Vec::new();
        let mut count = 0;
        for line in include_str!("golden.txt").lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let mut fields = line.split('\t');
            let (Some(word), Some(text)) = (fields.next(), fields.next()) else {
                panic!("a line of golden.txt has no text: {line}");
            };
            let want =
                u32::from_str_radix(word, 16).expect("golden.txt has a word that is not hex");
            let fixup = fields.next();
            count += 1;
            let got = read(text)
                .map_err(|e| e.to_string())
                .and_then(|line| encode(&line.mnemonic, &line.values).map_err(|e| e.to_string()));
            match got {
                Ok(got) if got.word == want && got.fixup.map(Fixup::name) == fixup => {}
                Ok(got) => wrong.push(format!(
                    "{text}: wrote {:08x} {:?}, GNU as wrote {want:08x} {fixup:?}",
                    got.word,
                    got.fixup.map(Fixup::name)
                )),
                Err(e) => wrong.push(format!("{text}: {e}")),
            }
        }
        assert!(count > 400, "golden.txt has only {count} lines");
        assert!(wrong.is_empty(), "{} of {count} lines differ:\n{}", wrong.len(), wrong.join("\n"));
    }

    /// The carry instructions take three registers of one width, none of them the stack pointer,
    /// which is what GNU as holds them to as well.
    #[test]
    fn the_carry_instructions_refuse_mixed_widths_and_the_stack_pointer() {
        for text in ["adc x1, w2, x3", "adc sp, x2, x3", "sbcs w1, w2, sp", "ngc x1"] {
            let line = read(text).expect("a line");
            assert!(encode(&line.mnemonic, &line.values).is_err(), "{text}");
        }
    }

    /// The words for the CRC32 extension, from the encoding in the Arm ARM, C6.2 under `CRC32B`
    /// and `CRC32CB` and their siblings, and the same words LLVM's assembler writes for these
    /// lines under `-march=armv8-a+crc`. GNU as writes them as well, since the encoding has no
    /// choice in it.
    #[test]
    fn the_crc32_steps_are_the_words_the_manual_gives() {
        for (text, word) in [
            ("crc32b w0, w1, w2", 0x1ac2_4020),
            ("crc32h w3, w4, w5", 0x1ac5_4483),
            ("crc32w w6, w7, w8", 0x1ac8_48e6),
            ("crc32x w9, w10, x11", 0x9acb_4d49),
            ("crc32cb w0, w1, w2", 0x1ac2_5020),
            ("crc32ch w3, w4, w5", 0x1ac5_5483),
            ("crc32cw w6, w7, w8", 0x1ac8_58e6),
            ("crc32cx w9, w10, x11", 0x9acb_5d49),
            ("crc32cx w0, w0, xzr", 0x9adf_5c00),
            ("crc32cw wzr, w30, w29", 0x1add_5bdf),
        ] {
            let line = read(text).expect("a line");
            let got = encode(&line.mnemonic, &line.values).expect("a word");
            assert_eq!((got.word, got.fixup), (word, None), "{text}");
        }
        // The widths the instruction does not have, which GNU as refuses too.
        for text in [
            "crc32x w0, w1, w2",
            "crc32cb w0, w1, x2",
            "crc32cw x0, w1, w2",
            "crc32cx x0, x1, x2",
            "crc32cb w0, w1",
            "crc32cb w0, wsp, w2",
        ] {
            let line = read(text).expect("a line");
            assert!(encode(&line.mnemonic, &line.values).is_err(), "{text}");
        }
        assert!(encode("crc32c", &[Value::Gpr(Width::W, 0); 3]).is_err());
        assert!(encode("crc32cq", &[Value::Gpr(Width::W, 0); 3]).is_err());
    }

    #[test]
    fn the_pair_exclusives_vector_lengths_and_unprivileged_atomics_are_llvm_mc_words() {
        for (text, word) in [
            ("ldxp x4, x3, [x0]", 0xc87f_0c04),
            ("ldaxp x4, x3, [x0]", 0xc87f_8c04),
            ("ldxp w4, w3, [x0]", 0x887f_0c04),
            ("stxp w5, x4, x3, [x0]", 0xc825_0c04),
            ("stlxp w5, x4, x3, [x0]", 0xc825_8c04),
            ("rdvl x5, #-32", 0x04bf_5405),
            ("rdsvl x1, #1", 0x04bf_5821),
            ("addvl sp, sp, #-2", 0x043f_57df),
            ("addpl x0, x1, #3", 0x0461_5060),
            ("cast x0, x2, [x3]", 0xc980_7c62),
            ("casalt x0, x2, [x3]", 0xc9c0_fc62),
            ("caspalt x0, x1, x2, x3, [x4]", 0x49c0_fc82),
            ("ldtadd w0, w1, [x2]", 0x1920_0441),
            ("ldtaddal x0, x1, [x2]", 0x59e0_0441),
            ("ldtclr x0, x1, [x2]", 0x5920_1441),
            ("swptal w0, w1, [x2]", 0x19e0_8441),
            ("sttclrl w0, [x2]", 0x1960_145f),
            ("sttset x0, [x2]", 0x5920_345f),
        ] {
            let line = read(text).expect("a line");
            let got = encode(&line.mnemonic, &line.values).expect("a word");
            assert_eq!((got.word, got.fixup), (word, None), "{text}");
        }
        // What the instructions do not have, which llvm-mc refuses too.
        for text in [
            "ldxp x4, w3, [x0]",
            "stxp x5, x4, x3, [x0]",
            "rdvl w0, #1",
            "rdvl x0, #32",
            "casalt w0, w2, [x3]",
            "ldteor x0, x1, [x2]",
            "ldtaddb w0, w1, [x2]",
        ] {
            let line = read(text).expect("a line");
            assert!(encode(&line.mnemonic, &line.values).is_err(), "{text}");
        }
    }

    #[test]
    fn the_sve_state_saves_are_llvm_mc_words() {
        for (text, word) in [
            ("ldr z0, [x0]", 0x8580_4000),
            ("ldr z31, [x2, #-256, mul vl]", 0x85a0_405f),
            ("ldr z5, [sp, #255, mul vl]", 0x859f_5fe5),
            ("str z3, [x1, #7, mul vl]", 0xe580_5c23),
            ("str z9, [x0, #9, MUL VL]", 0xe581_4409),
            ("ldr p0, [x0]", 0x8580_0000),
            ("ldr p15, [x2, #15, mul vl]", 0x8581_1c4f),
            ("str p7, [x1, #-1, mul vl]", 0xe5bf_1c27),
            ("pfalse p3.b", 0x2518_e403),
            ("rdffr p2.b", 0x2519_f002),
            ("wrffr p4.b", 0x2528_9080),
            // A slice of the SME array, which is how the kernel saves and loads `za`.
            ("str za[w12, #0], [x0]", 0xe120_0000),
            ("ldr za[w12, 0], [x3]", 0xe100_0060),
            ("str za[w15, 3], [x9, #3, mul vl]", 0xe120_6123),
            ("ldr za[w13, #0], [sp]", 0xe100_23e0),
        ] {
            let line = read(text).expect("a line");
            let got = encode(&line.mnemonic, &line.values).expect("a word");
            assert_eq!((got.word, got.fixup), (word, None), "{text}");
        }
        // What the instructions do not have, which llvm-mc refuses too.
        for text in [
            "ldr z0, [x0, #256, mul vl]",
            "str z0, [x0, #-257, mul vl]",
            "ldr z0, [x0, #1]",
            "ldr z0, [x0], #16",
            "ldr z0, [x0, x1]",
            "ldr p0.b, [x0]",
            "pfalse p0",
            "pfalse p0.h",
            "wrffr p0",
            "ldr x0, [x1, #1, mul vl]",
            "ldr za[w11, 0], [x0]",
            "ldr za[w12, 16], [x0, #16, mul vl]",
            "ldr za[w12, 1], [x0, #2, mul vl]",
            "ldr za[w12, 1], [x0]",
            "ldr za[x12, 0], [x0]",
        ] {
            let refused =
                read(text).map_or(true, |line| encode(&line.mnemonic, &line.values).is_err());
            assert!(refused, "{text}");
        }
    }

    #[test]
    fn the_non_temporal_pairs_are_llvm_mc_words() {
        // The kernel's `copy_page` under hibernation, which streams a page through `ldnp` and
        // `stnp`. There is no pre or post index form of either.
        for (text, word) in [
            ("stnp x1, x2, [x0]", 0xa800_0801),
            ("ldnp x3, x4, [x0, #16]", 0xa841_1003),
            ("stnp q0, q1, [x0, #-32]", 0xac3f_0400),
            ("ldnp d0, d1, [x2, #8]", 0x6c40_8440),
            ("ldnp w5, w6, [sp, #4]", 0x2840_9be5),
        ] {
            let line = read(text).expect("a line");
            let got = encode(&line.mnemonic, &line.values).expect("a word");
            assert_eq!((got.word, got.fixup), (word, None), "{text}");
        }
        for text in ["ldnp x3, x4, [x0, #16]!", "stnp x1, x2, [x0], #16"] {
            let line = read(text).expect("a line");
            assert!(encode(&line.mnemonic, &line.values).is_err(), "{text}");
        }
    }

    #[test]
    fn the_crypto_and_memory_copy_words_are_the_words_llvm_mc_writes() {
        for (text, word) in [
            ("aese v3.16b, v5.16b", 0x4e28_48a3),
            ("aesimc v0.16b, v1.16b", 0x4e28_7820),
            ("sha1h s3, s5", 0x5e28_08a3),
            ("sha1c q3, s5, v7.4s", 0x5e07_00a3),
            ("sha256h2 q3, q5, v7.4s", 0x5e07_50a3),
            ("sha512su1 v3.2d, v5.2d, v7.2d", 0xce67_88a3),
            ("eor3 v0.16b, v1.16b, v2.16b, v3.16b", 0xce02_0c20),
            ("xar v0.2d, v1.2d, v2.2d, #10", 0xce82_2820),
            ("pmull v3.1q, v5.1d, v7.1d", 0x0ee7_e0a3),
            ("pmull2 v3.1q, v5.2d, v7.2d", 0x4ee7_e0a3),
            ("cpyfp [x3]!, [x5]!, x7!", 0x1905_04e3),
            ("cpyertrn [x0]!, [x1]!, x2!", 0x1d81_a440),
            ("setp [x3]!, x5!, x7", 0x19c7_04a3),
            ("setgmtn [x3]!, x5!, xzr", 0x1ddf_74a3),
            ("ldtr x1, [x2, #8]", 0xf840_8841),
            ("sttrb w1, [x2, #-3]", 0x381f_d841),
            ("msr s0_3_c1_c0_0, x5", 0xd503_1005),
            ("mrs x5, s3_0_c15_c2_0", 0xd538_f205),
        ] {
            let line = read(text).expect("a line");
            let got = encode(&line.mnemonic, &line.values).expect("a word");
            assert_eq!((got.word, got.fixup), (word, None), "{text}");
        }
        // Shapes the instructions do not have, which llvm-mc refuses too.
        for text in ["add v0.1q, v1.1q, v2.1q", "cpyfp [x3], [x5]!, x7!", "setp [x3]!, w5!, x7"] {
            let line = read(text).expect("a line");
            assert!(encode(&line.mnemonic, &line.values).is_err(), "{text}");
        }
    }

    #[test]
    fn a_filled_in_branch_carries_its_distance_in_words() {
        assert_eq!(Fixup::Jump26.apply(0x1400_0000, 8), Some(0x1400_0002));
        assert_eq!(Fixup::Call26.apply(0x9400_0000, -4), Some(0x97ff_ffff));
        assert_eq!(Fixup::CondBr19.apply(0x5400_0000, -4), Some(0x54ff_ffe0));
        assert_eq!(Fixup::TestBr14.apply(0x3600_0000, 4), Some(0x3600_0020));
        // Not a whole number of words, and too far.
        assert_eq!(Fixup::Jump26.apply(0x1400_0000, 2), None);
        assert_eq!(Fixup::TestBr14.apply(0x3600_0000, 1 << 15), None);
    }

    #[test]
    fn a_filled_in_address_splits_the_way_the_machine_reads_it() {
        assert_eq!(Fixup::AdrLo21.apply(0x1000_0000, 1), Some(0x3000_0000));
        assert_eq!(Fixup::AdrLo21.apply(0x1000_0000, 4), Some(0x1000_0020));
        assert_eq!(Fixup::AdrPage21.apply(0x9000_0000, 0x1000), Some(0xb000_0000));
        assert_eq!(Fixup::AdrPage21.apply(0x9000_0000, 0x800), None);
        assert_eq!(Fixup::AddLo12.apply(0x9100_0000, 0x1234), Some(0x9108_d000));
        assert_eq!(Fixup::Ldst64Lo12.apply(0xf940_0000, 0x1238), Some(0xf941_1c00));
        assert_eq!(Fixup::Ldst64Lo12.apply(0xf940_0000, 0x1234), None);
        assert_eq!(Fixup::TprelHi12.apply(0x9140_0000, 0x5000), Some(0x9140_1400));
    }

    #[test]
    fn an_offset_into_the_section_is_the_word_clang_writes_for_windows() {
        // What clang for aarch64-w64-mingw32 writes for the last two instructions of a Windows
        // thread-local access and for a load straight from the variable. The high half is written
        // with the shift and without it, and both are the same word.
        for (text, word, fixup) in [
            ("add x8, x8, :secrel_hi12:counter", 0x9140_0108, Fixup::SecrelHigh12A),
            ("add x8, x8, :secrel_hi12:counter, lsl #12", 0x9140_0108, Fixup::SecrelHigh12A),
            ("add x8, x8, :secrel_lo12:counter", 0x9100_0108, Fixup::SecrelLow12A),
            ("ldr w9, [x8, :secrel_lo12:counter]", 0xb940_0109, Fixup::SecrelLow12L),
        ] {
            let line = read(text).expect("a line");
            let got = encode(&line.mnemonic, &line.values).expect("a word");
            assert_eq!((got.word, got.fixup), (word, Some(fixup)), "{text}");
            assert_eq!(fixup.elf(), None, "ELF has no relocation for {text}");
        }
        // The low bits of a load are scaled by its size, which is in the word.
        assert_eq!(Fixup::SecrelLow12L.apply(0xb940_0109, 0x388), Some(0xb943_8909));
        assert_eq!(Fixup::SecrelLow12L.apply(0x3dc0_0000, 0x20), Some(0x3dc0_0800));
        assert_eq!(Fixup::SecrelLow12L.apply(0x3dc0_0000, 0x28), None);
    }

    #[test]
    fn a_register_in_the_wrong_place_is_an_error_and_not_another_word() {
        // The zero register as a base for an immediate add is the stack pointer's number.
        let zr = Value::Gpr(Width::X, 31);
        let x0 = Value::Gpr(Width::X, 0);
        assert!(encode("add", &[x0, zr, Value::Imm(1)]).is_err());
        // And the stack pointer where only the zero register can be.
        assert!(encode("adds", &[Value::Sp(Width::X), x0, Value::Imm(1)]).is_err());
        assert!(encode("add", &[x0, Value::Gpr(Width::W, 1), Value::Imm(1)]).is_err());
        assert!(encode("and", &[x0, x0, Value::Imm(0)]).is_err());
        assert!(encode("movz", &[x0, Value::Imm(1 << 16)]).is_err());
    }

    #[test]
    fn every_float_fmov_can_carry_comes_back_to_itself() {
        assert_eq!(float_imm8(1.0), Some(0x70));
        assert_eq!(float_imm8(0.0), None);
        assert_eq!(float_imm8(0.1), None);
        assert_eq!(float_imm8(32.0), None);
    }
}
