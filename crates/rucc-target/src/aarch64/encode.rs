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
///
/// Only the three that the instructions here take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrangement {
    /// Eight bytes in the low half, `v0.8b`.
    B8,
    /// Sixteen bytes, `v0.16b`.
    B16,
    /// Two eight byte lanes, `v0.2d`.
    D2,
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
    /// A system register, as the fifteen bits `mrs` and `msr` carry for it.
    System(u16),
}

/// How the zeros an instruction was written with are to be filled in.
///
/// Each of these is one relocation type in the ELF ABI for this machine, and the names are that
/// document's without the `R_AARCH64_` in front.
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
}

impl Fixup {
    /// The relocation type the ELF ABI gives it.
    #[must_use]
    pub fn elf(self) -> u32 {
        match self {
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
        }
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
            Fixup::AddLo12 | Fixup::TprelLo12Nc => Some(word | low << 10),
            Fixup::TprelHi12 => {
                let high = u32::try_from(value >> 12).ok().filter(|&high| high < 1 << 12)?;
                (value >= 0).then_some(word | high << 10)
            }
            Fixup::Ldst8Lo12 => Some(word | low << 10),
            Fixup::Ldst16Lo12 => scaled_low(word, low, 1),
            Fixup::Ldst32Lo12 => scaled_low(word, low, 2),
            Fixup::Ldst64Lo12 | Fixup::GotLo12 | Fixup::GotTprelLo12Nc => scaled_low(word, low, 3),
            Fixup::Ldst128Lo12 => scaled_low(word, low, 4),
        }
    }
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
        if let Some(cond) = m.strip_prefix("b.") {
            let cond = Cond::named(cond).ok_or_else(|| self.unwritten())?;
            return match values {
                [Value::Symbol(Operator::Plain)] => {
                    Ok((0x5400_0000 | cond.bits(), Some(Fixup::CondBr19)))
                }
                _ => Err(self.unwritten()),
            };
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
            "sdiv" => self.two_source(0b00_0011, values)?,
            "udiv" => self.two_source(0b00_0010, values)?,
            "lslv" => self.two_source(0b00_1000, values)?,
            "lsrv" => self.two_source(0b00_1001, values)?,
            "asrv" => self.two_source(0b00_1010, values)?,
            "rorv" => self.two_source(0b00_1011, values)?,
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
            "ldp" | "stp" | "ldpsw" => self.pair(values)?,
            "ldxr" | "ldxrb" | "ldxrh" | "ldaxr" | "ldaxrb" | "ldaxrh" | "ldar" | "ldarb"
            | "ldarh" | "stlr" | "stlrb" | "stlrh" => self.exclusive(None, values)?,
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
            "nop" | "yield" | "isb" if values.is_empty() => match m {
                "nop" => 0xd503_201f,
                "yield" => 0xd503_203f,
                _ => 0xd503_3fdf,
            },
            "brk" | "svc" | "hlt" => match values {
                [Value::Imm(imm)] => {
                    let imm = self.number(*imm, 1 << 16)?;
                    let base = match m {
                        "brk" => 0xd420_0000,
                        "svc" => 0xd400_0001,
                        _ => 0xd440_0000,
                    };
                    base | imm << 5
                }
                _ => return Err(self.unwritten()),
            },
            "dmb" | "dsb" => match values {
                [Value::Barrier(option)] if *option < 16 => {
                    let base = if m == "dmb" { 0xd503_30bf } else { 0xd503_309f };
                    base | u32::from(*option) << 8
                }
                _ => return Err(self.unwritten()),
            },
            "mrs" => match values {
                [t, Value::System(field)] => {
                    let (width, rt) = self.zr(t)?;
                    if width != Width::X {
                        return Err(self.register());
                    }
                    0xd530_0000 | u32::from(*field & 0x7fff) << 5 | rt
                }
                _ => return Err(self.unwritten()),
            },
            "msr" => match values {
                [Value::System(field), t] => {
                    let (width, rt) = self.zr(t)?;
                    if width != Width::X {
                        return Err(self.register());
                    }
                    0xd510_0000 | u32::from(*field & 0x7fff) << 5 | rt
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
            "movi" => match values {
                [Value::Fp(Scalar::D, d), Value::Imm(0)] => 0x2f00_e400 | u32::from(*d),
                [Value::Vector(Arrangement::D2, d), Value::Imm(0)] => 0x6f00_e400 | u32::from(*d),
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
            [Value::Vector(a, d), Value::Vector(b, n)] if a == b && *a != Arrangement::D2 => {
                let q = u32::from(*a == Arrangement::B16);
                let (d, n) = (u32::from(*d), u32::from(*n));
                q << 30 | 0x0ea0_1c00 | n << 16 | n << 5 | d
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
    fn two_source(self, opcode: u32, values: &[Value]) -> Result<u32, Error> {
        let [d, n, mm] = values else {
            return Err(self.unwritten());
        };
        let ((wd, rd), (wn, rn), (wm, rmm)) = (self.zr(d)?, self.zr(n)?, self.zr(mm)?);
        let width = self.same(self.same(wd, wn)?, wm)?;
        Ok(width.sf() << 31 | 0x1ac0_0000 | rmm << 16 | opcode << 10 | rn << 5 | rd)
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
        let access = self.access(t)?;
        let rt = match *t {
            Value::Gpr(_, number) => u32::from(number),
            Value::Fp(_, number) => u32::from(number),
            _ => return Err(self.unwritten()),
        };
        let unscaled_only = m.starts_with("ldur") || m.starts_with("stur");
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
                    front | imm9 << 12 | rn << 5
                }
            }
            (Offset::Imm(imm), mode) if !unscaled_only => {
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
        let base = base.strip_prefix("ur").or_else(|| base.strip_prefix('r'));
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
        let mode = match addr.mode {
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
