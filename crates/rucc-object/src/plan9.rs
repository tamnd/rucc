//! Plan 9 object files: the `.6` files that `6l` reads and the `.7` files that `7l` reads.
//!
//! Design: tamnd/rucc#4156, and 07.1 and 07.2 of the rucc-plan9 plan.
//!
//! A Plan 9 object is not machine code. It is the list of `Prog` records that `6c` or `7c` made,
//! and the linker chooses the encodings, lays out the code and inserts the frame adjustment. The
//! file has no header and no sections. It is a run of records, each an opcode, a line and two or
//! three operands, and it ends with an `AEND` record.
//!
//! The writer follows `outcode`, `outhist`, `zname` and `zaddr` in `6c/swt.c` and `7c/swt.c`. The
//! reader follows `ldobj` and `zaddr` in `6l/obj.c` and `7l/obj.c`. Two rules make the bytes the
//! same as the bytes from kencc:
//!
//! - A symbol is written by name once and is then a slot in a cache of [`NSYM`] slots. The slots
//!   are given in order from 1, and after the last slot the next one is 1 again. A symbol has the
//!   slot that it was last given, and the slot holds a symbol only with one type. The reader keeps
//!   the same table, so a slot means what the writer meant.
//! - The file and line history comes first, as one `AHISTORY` record for each entry. The name of
//!   a file is split on `/`, and each part is an `ANAME` record of type `D_FILE` in slot 1. A
//!   relative name follows the parts of the directory of the compilation.

use std::fmt;

use rucc_base::hash::{Map, Set};

#[path = "plan9/amd64_ops.rs"]
mod amd64_ops;
#[path = "plan9/arm64_ops.rs"]
mod arm64_ops;

/// The number of slots in the symbol cache, `NSYM` in `6.out.h` and `7.out.h`.
pub const NSYM: usize = 50;

/// The length of a short string operand, `NSNAME`.
pub const NSNAME: usize = 8;

/// The `TEXT` flag that turns profiling off for a function.
pub const NOPROF: u8 = 1 << 0;

/// The `TEXT` and `GLOBL` flag that lets the linker keep one of several copies.
pub const DUPOK: u8 = 1 << 1;

/// The opcodes and the operand types of `6c/6.out.h`.
pub mod amd64 {
    pub use super::amd64_ops::NAMES;

    /// `AXXX`, the opcode of no instruction.
    pub const AXXX: u16 = 0;
    /// `ADATA`.
    pub const ADATA: u16 = 45;
    /// `AGLOBL`.
    pub const AGLOBL: u16 = 54;
    /// `AHISTORY`.
    pub const AHISTORY: u16 = 56;
    /// `ANAME`.
    pub const ANAME: u16 = 132;
    /// `ATEXT`.
    pub const ATEXT: u16 = 226;
    /// `AEND`.
    pub const AEND: u16 = 335;
    /// `ASIGNAME`.
    pub const ASIGNAME: u16 = 338;
    /// `ARET`, which is also the tail call when it has a constant and a symbol.
    pub const ARET: u16 = 169;

    /// `D_AL`, the first of the byte registers.
    pub const D_AL: u8 = 0;
    /// `D_CL`, the count of a shift.
    pub const D_CL: u8 = 1;
    /// `D_AX`, the first of the 64-bit registers. `D_R15` is `D_AX` + 15.
    pub const D_AX: u8 = 16;
    /// `D_CX`.
    pub const D_CX: u8 = 17;
    /// `D_DX`.
    pub const D_DX: u8 = 18;
    /// `D_BX`.
    pub const D_BX: u8 = 19;
    /// `D_SP`, the stack pointer.
    pub const D_SP: u8 = 20;
    /// `D_BP`, which holds the first argument.
    pub const D_BP: u8 = 21;
    /// `D_SI`.
    pub const D_SI: u8 = 22;
    /// `D_DI`.
    pub const D_DI: u8 = 23;
    /// `D_R8`.
    pub const D_R8: u8 = 24;
    /// `D_AH`.
    pub const D_AH: u8 = 32;
    /// `D_F0`, the first of the x87 registers.
    pub const D_F0: u8 = 36;
    /// `D_M0`, the first of the MMX registers.
    pub const D_M0: u8 = 44;
    /// `D_X0`, the first of the SSE registers.
    pub const D_X0: u8 = 52;
    /// `D_CS`, the first of the segment registers.
    pub const D_CS: u8 = 68;
    /// `D_CR`, the first of the control registers.
    pub const D_CR: u8 = 79;
    /// `D_DR`, the first of the debug registers.
    pub const D_DR: u8 = 95;
    /// `D_TR`, the first of the test registers.
    pub const D_TR: u8 = 103;
    /// `D_NONE`.
    pub const D_NONE: u8 = 111;
    /// `D_BRANCH`.
    pub const D_BRANCH: u8 = 112;
    /// `D_EXTERN`.
    pub const D_EXTERN: u8 = 113;
    /// `D_STATIC`.
    pub const D_STATIC: u8 = 114;
    /// `D_AUTO`.
    pub const D_AUTO: u8 = 115;
    /// `D_PARAM`.
    pub const D_PARAM: u8 = 116;
    /// `D_CONST`.
    pub const D_CONST: u8 = 117;
    /// `D_FCONST`.
    pub const D_FCONST: u8 = 118;
    /// `D_SCONST`.
    pub const D_SCONST: u8 = 119;
    /// `D_ADDR`. The type of the address is then in the index field.
    pub const D_ADDR: u8 = 120;
    /// `D_FILE`.
    pub const D_FILE: u8 = 121;
    /// `D_FILE1`.
    pub const D_FILE1: u8 = 122;
    /// `D_INDIR`. A memory operand is this added to the type of its base.
    pub const D_INDIR: u8 = 123;

    /// The operand has a type byte.
    pub const T_TYPE: u8 = 1 << 0;
    /// The operand has an index register and a scale.
    pub const T_INDEX: u8 = 1 << 1;
    /// The operand has a 4-byte offset.
    pub const T_OFFSET: u8 = 1 << 2;
    /// The operand is a floating point constant.
    pub const T_FCONST: u8 = 1 << 3;
    /// The operand names a symbol slot.
    pub const T_SYM: u8 = 1 << 4;
    /// The operand is a short string.
    pub const T_SCONST: u8 = 1 << 5;
    /// The offset has 4 more bytes.
    pub const T_64: u8 = 1 << 6;
}

/// The opcodes and the operand types of `7c/7.out.h`.
pub mod arm64 {
    pub use super::arm64_ops::NAMES;

    /// `AXXX`, the opcode of no instruction.
    pub const AXXX: u16 = 0;
    /// `ATEXT`.
    pub const ATEXT: u16 = 309;
    /// `ADATA`.
    pub const ADATA: u16 = 310;
    /// `AGLOBL`.
    pub const AGLOBL: u16 = 311;
    /// `AHISTORY`.
    pub const AHISTORY: u16 = 312;
    /// `ANAME`.
    pub const ANAME: u16 = 313;
    /// `ASIGNAME`.
    pub const ASIGNAME: u16 = 320;
    /// `AEND`.
    pub const AEND: u16 = 323;

    /// `NREG`, the number of registers and the register field of an operand with none.
    pub const NREG: u8 = 32;

    /// `D_GOK`.
    pub const D_GOK: u8 = 0;
    /// `D_NONE`.
    pub const D_NONE: u8 = 1;
    /// `D_EXTERN`.
    pub const D_EXTERN: u8 = 2;
    /// `D_STATIC`.
    pub const D_STATIC: u8 = 3;
    /// `D_AUTO`.
    pub const D_AUTO: u8 = 4;
    /// `D_PARAM`.
    pub const D_PARAM: u8 = 5;
    /// `D_BRANCH`.
    pub const D_BRANCH: u8 = 6;
    /// `D_OREG`, an offset from a register.
    pub const D_OREG: u8 = 7;
    /// `D_XPRE`, a pre-indexed address.
    pub const D_XPRE: u8 = 8;
    /// `D_XPOST`, a post-indexed address.
    pub const D_XPOST: u8 = 9;
    /// `D_CONST`, a 32-bit constant.
    pub const D_CONST: u8 = 10;
    /// `D_DCONST`, a 64-bit constant.
    pub const D_DCONST: u8 = 11;
    /// `D_FCONST`.
    pub const D_FCONST: u8 = 12;
    /// `D_SCONST`.
    pub const D_SCONST: u8 = 13;
    /// `D_REG`.
    pub const D_REG: u8 = 14;
    /// `D_SP`.
    pub const D_SP: u8 = 15;
    /// `D_FREG`.
    pub const D_FREG: u8 = 16;
    /// `D_VREG`.
    pub const D_VREG: u8 = 17;
    /// `D_SPR`, a special register.
    pub const D_SPR: u8 = 18;
    /// `D_FILE`.
    pub const D_FILE: u8 = 19;
    /// `D_OCONST`.
    pub const D_OCONST: u8 = 20;
    /// `D_FILE1`.
    pub const D_FILE1: u8 = 21;
    /// `D_SHIFT`, a shifted register.
    pub const D_SHIFT: u8 = 22;
    /// `D_PAIR`, a pair of registers. `7l` cannot read it.
    pub const D_PAIR: u8 = 23;
    /// `D_ADDR`.
    pub const D_ADDR: u8 = 24;
    /// `D_ADRP`.
    pub const D_ADRP: u8 = 25;
    /// `D_ADRLO`.
    pub const D_ADRLO: u8 = 26;
    /// `D_EXTREG`, an extended register.
    pub const D_EXTREG: u8 = 27;
    /// `D_ROFF`, a register offset.
    pub const D_ROFF: u8 = 28;
    /// `D_COND`, a condition.
    pub const D_COND: u8 = 29;
}

/// The two machines that have a Plan 9 object format here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Machine {
    /// amd64, the `.6` file.
    Amd64,
    /// arm64, the `.7` file.
    Arm64,
}

impl Machine {
    /// The letter of the toolchain, which is also the extension of the object.
    #[must_use]
    pub const fn letter(self) -> char {
        match self {
            Machine::Amd64 => '6',
            Machine::Arm64 => '7',
        }
    }

    /// The name of an opcode, or `None` for a number past `ALAST`.
    #[must_use]
    pub fn opcode_name(self, op: u16) -> Option<&'static str> {
        let names: &[&str] = match self {
            Machine::Amd64 => &amd64::NAMES,
            Machine::Arm64 => &arm64::NAMES,
        };
        names.get(usize::from(op)).copied()
    }

    /// The opcode of a name such as `AMOVQ`, or `None` for a name the machine does not have.
    #[must_use]
    pub fn opcode(self, name: &str) -> Option<u16> {
        static AMD64: std::sync::OnceLock<Map<&'static str, u16>> = std::sync::OnceLock::new();
        static ARM64: std::sync::OnceLock<Map<&'static str, u16>> = std::sync::OnceLock::new();
        let (cell, names): (_, &'static [&'static str]) = match self {
            Machine::Amd64 => (&AMD64, &amd64::NAMES),
            Machine::Arm64 => (&ARM64, &arm64::NAMES),
        };
        let map = cell.get_or_init(|| {
            names.iter().enumerate().map(|(op, &name)| (name, op as u16)).collect()
        });
        map.get(name).copied()
    }

    const fn aname(self) -> u16 {
        match self {
            Machine::Amd64 => amd64::ANAME,
            Machine::Arm64 => arm64::ANAME,
        }
    }

    const fn asigname(self) -> u16 {
        match self {
            Machine::Amd64 => amd64::ASIGNAME,
            Machine::Arm64 => arm64::ASIGNAME,
        }
    }

    const fn d_file(self) -> u8 {
        match self {
            Machine::Amd64 => amd64::D_FILE,
            Machine::Arm64 => arm64::D_FILE,
        }
    }

    const fn d_extern(self) -> u8 {
        match self {
            Machine::Amd64 => amd64::D_EXTERN,
            Machine::Arm64 => arm64::D_EXTERN,
        }
    }
}

/// One operand of an amd64 `Prog`, the `Adr` of `6c/gc.h`.
#[derive(Debug, Clone, PartialEq)]
pub struct Addr6 {
    /// The type: a register, `D_INDIR` added to a register or a name, or one of the kinds.
    pub ty: u8,
    /// The index register, or `D_NONE`. For `D_ADDR` it is the type of the address.
    pub index: u8,
    /// The scale of the index. For `ADATA` it is the width and for `ATEXT` the flags.
    pub scale: u8,
    /// The offset or the constant.
    pub offset: i64,
    /// The symbol, by name.
    pub sym: Option<String>,
    /// The value of a `D_FCONST` operand.
    pub float: f64,
    /// The bytes of a `D_SCONST` operand.
    pub string: [u8; NSNAME],
}

impl Default for Addr6 {
    fn default() -> Self {
        Addr6 {
            ty: amd64::D_NONE,
            index: amd64::D_NONE,
            scale: 0,
            offset: 0,
            sym: None,
            float: 0.0,
            string: [0; NSNAME],
        }
    }
}

impl Addr6 {
    /// An operand of one type and nothing else, such as a register.
    #[must_use]
    pub fn of(ty: u8) -> Self {
        Addr6 { ty, ..Addr6::default() }
    }

    /// The constant `$value`.
    #[must_use]
    pub fn constant(value: i64) -> Self {
        Addr6 { ty: amd64::D_CONST, offset: value, ..Addr6::default() }
    }

    /// A symbol of the type `name`, which is `D_EXTERN`, `D_STATIC`, `D_AUTO` or `D_PARAM`.
    #[must_use]
    pub fn symbol(name: u8, sym: &str, offset: i64) -> Self {
        Addr6 { ty: name, offset, sym: Some(sym.to_owned()), ..Addr6::default() }
    }

    /// The type that the symbol of the operand has in the cache. For an address it is the type
    /// of the thing addressed.
    fn sym_type(&self) -> u8 {
        if self.ty == amd64::D_ADDR { self.index } else { self.ty }
    }
}

/// One amd64 `Prog`.
#[derive(Debug, Clone, PartialEq)]
pub struct Prog6 {
    /// The opcode, an index into [`amd64::NAMES`].
    pub op: u16,
    /// The absolute line, which counts every line that the compiler read.
    pub line: u32,
    /// The first operand.
    pub from: Addr6,
    /// The second operand.
    pub to: Addr6,
}

impl Prog6 {
    /// A record with two operands.
    #[must_use]
    pub fn new(op: u16, line: u32, from: Addr6, to: Addr6) -> Self {
        Prog6 { op, line, from, to }
    }
}

/// One operand of an arm64 `Prog`, the `Adr` of `7c/gc.h`.
#[derive(Debug, Clone, PartialEq)]
pub struct Addr7 {
    /// The type, `D_REG`, `D_OREG` and the rest.
    pub ty: u8,
    /// The register, or [`arm64::NREG`] for none.
    pub reg: u8,
    /// The kind of the symbol, `D_EXTERN`, `D_STATIC`, `D_AUTO`, `D_PARAM` or `D_NONE`.
    pub name: u8,
    /// The offset or the constant.
    pub offset: i64,
    /// The symbol, by name.
    pub sym: Option<String>,
    /// The value of a `D_FCONST` operand.
    pub float: f64,
    /// The bytes of a `D_SCONST` operand.
    pub string: [u8; NSNAME],
}

impl Default for Addr7 {
    fn default() -> Self {
        Addr7 {
            ty: arm64::D_NONE,
            reg: arm64::NREG,
            name: arm64::D_NONE,
            offset: 0,
            sym: None,
            float: 0.0,
            string: [0; NSNAME],
        }
    }
}

impl Addr7 {
    /// An operand of one type and nothing else.
    #[must_use]
    pub fn of(ty: u8) -> Self {
        Addr7 { ty, ..Addr7::default() }
    }

    /// The register `reg` of the type `ty`, which is `D_REG`, `D_FREG`, `D_VREG` or `D_SP`.
    #[must_use]
    pub fn register(ty: u8, reg: u8) -> Self {
        Addr7 { ty, reg, ..Addr7::default() }
    }

    /// The constant `$value`.
    #[must_use]
    pub fn constant(value: i64) -> Self {
        Addr7 { ty: arm64::D_CONST, offset: value, ..Addr7::default() }
    }
}

/// One arm64 `Prog`.
#[derive(Debug, Clone, PartialEq)]
pub struct Prog7 {
    /// The opcode, an index into [`arm64::NAMES`].
    pub op: u16,
    /// The middle register, or [`arm64::NREG`] for none.
    pub reg: u8,
    /// The absolute line.
    pub line: u32,
    /// The first operand.
    pub from: Addr7,
    /// The third operand, which is there when its type is not `D_NONE`.
    pub from3: Addr7,
    /// The last operand.
    pub to: Addr7,
}

impl Prog7 {
    /// A record with two operands and no middle register.
    #[must_use]
    pub fn new(op: u16, line: u32, from: Addr7, to: Addr7) -> Self {
        Prog7 { op, reg: arm64::NREG, line, from, from3: Addr7::default(), to }
    }
}

/// The records of one object.
#[derive(Debug, Clone, PartialEq)]
pub enum Progs {
    /// The records of a `.6` file.
    Amd64(Vec<Prog6>),
    /// The records of a `.7` file.
    Arm64(Vec<Prog7>),
}

impl Progs {
    /// Which file the records are for.
    #[must_use]
    pub const fn machine(&self) -> Machine {
        match self {
            Progs::Amd64(_) => Machine::Amd64,
            Progs::Arm64(_) => Machine::Arm64,
        }
    }
}

/// One entry of the file and line history, the `Hist` of `cc/cc.h`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    /// The file. A push of an `#include` and a `#line` have one, and a pop has none.
    pub name: Option<String>,
    /// The absolute line where the entry starts.
    pub line: u32,
    /// Zero for a push or a pop, the line of a `#line`, and -1 for a `#pragma lib`.
    pub offset: i64,
}

impl History {
    /// The start of a file, the main file or an `#include`.
    #[must_use]
    pub fn push(name: &str, line: u32) -> Self {
        History { name: Some(name.to_owned()), line, offset: 0 }
    }

    /// The end of an `#include`.
    #[must_use]
    pub const fn pop(line: u32) -> Self {
        History { name: None, line, offset: 0 }
    }

    /// A `#line` directive, which says that the next line is `number` of `name`.
    #[must_use]
    pub fn line(name: &str, line: u32, number: i64) -> Self {
        History { name: Some(name.to_owned()), line, offset: number }
    }

    /// A `#pragma lib`, which tells the linker to load the library.
    #[must_use]
    pub fn library(name: &str, line: u32) -> Self {
        History { name: Some(name.to_owned()), line, offset: -1 }
    }
}

/// One object file.
#[derive(Debug, Clone, PartialEq)]
pub struct Object {
    /// The directory of the compilation, the `pathname` of kencc. A relative file name in the
    /// history is written after it.
    pub directory: Option<String>,
    /// The file and line history, in order.
    pub history: Vec<History>,
    /// The records, which end with `AEND`.
    pub progs: Progs,
    /// Whether each external symbol has a signature, which is `-T`.
    pub signed: bool,
    /// The signatures of the external symbols. A symbol that is not here has the signature 0, as
    /// a symbol with no type has in kencc.
    pub signatures: Map<String, u32>,
}

impl Object {
    /// An object with no history and no signatures.
    #[must_use]
    pub fn new(progs: Progs) -> Self {
        Object {
            directory: None,
            history: Vec::new(),
            progs,
            signed: false,
            signatures: Map::default(),
        }
    }
}

/// Why a record cannot be written or read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan9Error {
    /// An arm64 operand of a type that `7l` cannot read.
    Operand {
        /// The type.
        ty: u8,
    },
    /// A symbol name with a NUL byte in it.
    Name(String),
    /// The file ends in the middle of a record.
    Truncated {
        /// Where the record starts.
        at: usize,
    },
    /// A record names a slot past [`NSYM`].
    Slot {
        /// Where the record starts.
        at: usize,
        /// The slot.
        slot: u8,
    },
}

impl fmt::Display for Plan9Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Plan9Error::Operand { ty } => write!(f, "7l cannot read an operand of type {ty}"),
            Plan9Error::Name(name) => write!(f, "the symbol name {name:?} has a NUL byte"),
            Plan9Error::Truncated { at } => write!(f, "the record at {at} is not complete"),
            Plan9Error::Slot { at, slot } => {
                write!(f, "the record at {at} names the symbol slot {slot}")
            }
        }
    }
}

impl std::error::Error for Plan9Error {}

/// The cache of symbol slots that `outcode` keeps.
struct Cache<'a> {
    /// The symbol and the type in each slot.
    slots: [(Option<&'a str>, u8); NSYM],
    /// The slot each symbol was last given, `s->sym`.
    last: Map<&'a str, usize>,
    /// The next slot to give.
    next: usize,
    /// The symbols that already have their signature in the file, `SIGDONE`.
    signed: Set<&'a str>,
}

/// The bytes of an object.
///
/// # Errors
///
/// [`Plan9Error::Operand`] for an arm64 operand that `7l` cannot read, and
/// [`Plan9Error::Name`] for a name that cannot be written.
pub fn write_plan9(object: &Object) -> Result<Vec<u8>, Plan9Error> {
    let machine = object.progs.machine();
    let mut out = Vec::new();
    history(&mut out, object, machine);
    let mut cache =
        Cache { slots: [(None, 0); NSYM], last: Map::default(), next: 1, signed: Set::default() };
    match &object.progs {
        Progs::Amd64(progs) => {
            for p in progs {
                let (sf, st) = slots(
                    &mut out,
                    &mut cache,
                    object,
                    machine,
                    [
                        (p.from.sym.as_deref(), p.from.sym_type()),
                        (p.to.sym.as_deref(), p.to.sym_type()),
                    ],
                )?;
                put_u16(&mut out, p.op);
                put_u32(&mut out, p.line);
                zaddr6(&mut out, &p.from, sf);
                zaddr6(&mut out, &p.to, st);
            }
        }
        Progs::Arm64(progs) => {
            for p in progs {
                let (sf, st) = slots(
                    &mut out,
                    &mut cache,
                    object,
                    machine,
                    [(p.from.sym.as_deref(), p.from.name), (p.to.sym.as_deref(), p.to.name)],
                )?;
                zwrite7(&mut out, p, sf, st)?;
            }
        }
    }
    Ok(out)
}

/// The slots of the two symbols of one record, with the `ANAME` records for the ones that are
/// not in the cache written first. When the second symbol takes the slot of the first, the first
/// is written again, which is the `jackpot` label of `outcode`.
fn slots<'a>(
    out: &mut Vec<u8>,
    cache: &mut Cache<'a>,
    object: &Object,
    machine: Machine,
    syms: [(Option<&'a str>, u8); 2],
) -> Result<(u8, u8), Plan9Error> {
    loop {
        let (sf, _) = slot(out, cache, object, machine, syms[0])?;
        let (st, fresh) = slot(out, cache, object, machine, syms[1])?;
        if fresh && st == sf {
            continue;
        }
        let narrow = |slot: usize| u8::try_from(slot).expect("a slot is less than NSYM");
        return Ok((narrow(sf), narrow(st)));
    }
}

/// The slot of one symbol, and whether it was written now.
fn slot<'a>(
    out: &mut Vec<u8>,
    cache: &mut Cache<'a>,
    object: &Object,
    machine: Machine,
    (sym, ty): (Option<&'a str>, u8),
) -> Result<(usize, bool), Plan9Error> {
    let Some(sym) = sym else {
        return Ok((0, false));
    };
    let mut at = cache.last.get(sym).copied().unwrap_or(0);
    if at >= NSYM {
        at = 0;
    }
    if cache.slots[at] == (Some(sym), ty) {
        return Ok((at, false));
    }
    let at = cache.next;
    cache.last.insert(sym, at);
    zname(out, cache, object, machine, sym, ty, at)?;
    cache.slots[at] = (Some(sym), ty);
    cache.next += 1;
    if cache.next >= NSYM {
        cache.next = 1;
    }
    Ok((at, true))
}

/// The `ANAME` or `ASIGNAME` record that puts a symbol in a slot.
fn zname<'a>(
    out: &mut Vec<u8>,
    cache: &mut Cache<'a>,
    object: &Object,
    machine: Machine,
    sym: &'a str,
    ty: u8,
    at: usize,
) -> Result<(), Plan9Error> {
    if sym.as_bytes().contains(&0) {
        return Err(Plan9Error::Name(sym.to_owned()));
    }
    if object.signed && ty == machine.d_extern() && cache.signed.insert(sym) {
        put_u16(out, machine.asigname());
        put_u32(out, object.signatures.get(sym).copied().unwrap_or(0));
    } else {
        put_u16(out, machine.aname());
    }
    out.push(ty);
    out.push(u8::try_from(at).expect("a slot is less than NSYM"));
    out.extend_from_slice(sym.as_bytes());
    out.push(0);
    Ok(())
}

/// The history records, from `outhist`.
fn history(out: &mut Vec<u8>, object: &Object, machine: Machine) {
    for entry in &object.history {
        let mut parts: Vec<&str> = Vec::new();
        if let Some(name) = entry.name.as_deref() {
            let relative = !name.starts_with('/') && entry.offset == 0;
            match object.directory.as_deref() {
                Some(dir) if relative && dir.starts_with('/') => {
                    split(dir, &mut parts);
                    split(name, &mut parts);
                }
                _ => split(name, &mut parts),
            }
        }
        for part in parts {
            put_u16(out, machine.aname());
            out.push(machine.d_file());
            out.push(1);
            out.push(b'<');
            out.extend_from_slice(part.as_bytes());
            out.push(0);
        }
        let to = entry.offset;
        match machine {
            Machine::Amd64 => {
                put_u16(out, amd64::AHISTORY);
                put_u32(out, entry.line);
                zaddr6(out, &Addr6::default(), 0);
                let ty = if to == 0 { amd64::D_NONE } else { amd64::D_CONST };
                zaddr6(out, &Addr6 { ty, offset: to, ..Addr6::default() }, 0);
            }
            Machine::Arm64 => {
                let ty = if to == 0 { arm64::D_NONE } else { arm64::D_CONST };
                let to = Addr7 { ty, offset: to, ..Addr7::default() };
                let prog = Prog7::new(arm64::AHISTORY, entry.line, Addr7::default(), to);
                zwrite7(out, &prog, 0, 0).expect("a history record has plain operands");
            }
        }
    }
}

/// The parts of a path in the order `outhist` writes them. A leading `/` is a part of its own,
/// and empty parts are left out.
fn split<'a>(path: &'a str, parts: &mut Vec<&'a str>) {
    let mut rest = path;
    if let Some(after) = rest.strip_prefix('/') {
        parts.push("/");
        rest = after;
    }
    parts.extend(rest.split('/').filter(|part| !part.is_empty()));
}

/// One amd64 operand, from `zaddr` in `6c/swt.c`.
fn zaddr6(out: &mut Vec<u8>, a: &Addr6, slot: u8) {
    let mut t = 0;
    if a.index != amd64::D_NONE || a.scale != 0 {
        t |= amd64::T_INDEX;
    }
    if slot != 0 {
        t |= amd64::T_SYM;
    }
    match a.ty {
        amd64::D_FCONST => t |= amd64::T_FCONST,
        amd64::D_SCONST => t |= amd64::T_SCONST,
        ty => {
            if ty != amd64::D_NONE {
                t |= amd64::T_TYPE;
            }
            if a.offset != 0 {
                t |= amd64::T_OFFSET;
                if i64::from(a.offset as i32) != a.offset {
                    t |= amd64::T_64;
                }
            }
        }
    }
    out.push(t);
    if t & amd64::T_INDEX != 0 {
        out.push(a.index);
        out.push(a.scale);
    }
    if t & amd64::T_OFFSET != 0 {
        put_u32(out, a.offset as u32);
        if t & amd64::T_64 != 0 {
            put_u32(out, (a.offset >> 32) as u32);
        }
    }
    if t & amd64::T_SYM != 0 {
        out.push(slot);
    }
    if t & amd64::T_FCONST != 0 {
        let bits = a.float.to_bits();
        put_u32(out, bits as u32);
        put_u32(out, (bits >> 32) as u32);
        return;
    }
    if t & amd64::T_SCONST != 0 {
        out.extend_from_slice(&a.string);
        return;
    }
    if t & amd64::T_TYPE != 0 {
        out.push(a.ty);
    }
}

/// One arm64 record, from `zwrite` in `7c/swt.c`.
fn zwrite7(out: &mut Vec<u8>, p: &Prog7, sf: u8, st: u8) -> Result<(), Plan9Error> {
    put_u16(out, p.op);
    let three = p.from3.ty != arm64::D_NONE;
    out.push(p.reg | if three { 0x40 } else { 0 });
    put_u32(out, p.line);
    zaddr7(out, &p.from, sf)?;
    if three {
        zaddr7(out, &p.from3, 0)?;
    }
    zaddr7(out, &p.to, st)
}

/// One arm64 operand, from `zaddr` in `7c/swt.c`. The types are the ones that `7l` reads.
fn zaddr7(out: &mut Vec<u8>, a: &Addr7, slot: u8) -> Result<(), Plan9Error> {
    let mut ty = a.ty;
    if ty == arm64::D_CONST && i64::from(a.offset as i32) != a.offset {
        ty = arm64::D_DCONST;
    }
    out.extend_from_slice(&[ty, a.reg, slot, a.name]);
    match ty {
        arm64::D_NONE
        | arm64::D_REG
        | arm64::D_SP
        | arm64::D_FREG
        | arm64::D_VREG
        | arm64::D_COND => {}
        arm64::D_OREG
        | arm64::D_XPRE
        | arm64::D_XPOST
        | arm64::D_CONST
        | arm64::D_BRANCH
        | arm64::D_SHIFT
        | arm64::D_EXTREG
        | arm64::D_ROFF
        | arm64::D_SPR => put_u32(out, a.offset as u32),
        arm64::D_DCONST => {
            put_u32(out, a.offset as u32);
            put_u32(out, (a.offset >> 32) as u32);
        }
        arm64::D_SCONST => out.extend_from_slice(&a.string),
        arm64::D_FCONST => {
            let bits = a.float.to_bits();
            put_u32(out, bits as u32);
            put_u32(out, (bits >> 32) as u32);
        }
        ty => return Err(Plan9Error::Operand { ty }),
    }
    Ok(())
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// One record of an object as the linker reads it, with the symbol slots resolved to names.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    /// An `ANAME` or an `ASIGNAME` record.
    Name {
        /// The type of the symbol.
        ty: u8,
        /// The slot.
        slot: u8,
        /// The name. A file name in the history starts with `<`.
        name: String,
        /// The signature, for an `ASIGNAME` record.
        signature: Option<u32>,
    },
    /// An amd64 instruction or pseudo-instruction.
    Amd64(Prog6),
    /// An arm64 instruction or pseudo-instruction.
    Arm64(Prog7),
}

/// The records of an object, as `ldobj` in `6l/obj.c` or `7l/obj.c` reads them.
///
/// # Errors
///
/// [`Plan9Error::Truncated`] for a file that ends in a record and [`Plan9Error::Slot`] for a slot
/// past the cache. The reader stops at the `AEND` record, as the linker does.
pub fn read_plan9(machine: Machine, bytes: &[u8]) -> Result<Vec<Record>, Plan9Error> {
    let mut reader = Reader { bytes, at: 0, slots: vec![None; NSYM + 1] };
    let mut records = Vec::new();
    let end = match machine {
        Machine::Amd64 => amd64::AEND,
        Machine::Arm64 => arm64::AEND,
    };
    while reader.at < bytes.len() {
        let start = reader.at;
        let op = reader.u16(start)?;
        if op == machine.aname() || op == machine.asigname() {
            let signature = if op == machine.asigname() { Some(reader.u32(start)?) } else { None };
            let ty = reader.u8(start)?;
            let slot = reader.u8(start)?;
            let name = reader.name(start)?;
            if usize::from(slot) > NSYM {
                return Err(Plan9Error::Slot { at: start, slot });
            }
            reader.slots[usize::from(slot)] = Some(name.clone());
            records.push(Record::Name { ty, slot, name, signature });
            continue;
        }
        let record = match machine {
            Machine::Amd64 => {
                let line = reader.u32(start)?;
                let from = reader.addr6(start)?;
                let to = reader.addr6(start)?;
                Record::Amd64(Prog6 { op, line, from, to })
            }
            Machine::Arm64 => {
                let flags = reader.u8(start)?;
                let line = reader.u32(start)?;
                let from = reader.addr7(start)?;
                let from3 = if flags & 0x40 != 0 { reader.addr7(start)? } else { Addr7::default() };
                let to = reader.addr7(start)?;
                Record::Arm64(Prog7 { op, reg: flags & 0x3f, line, from, from3, to })
            }
        };
        records.push(record);
        if op == end {
            break;
        }
    }
    Ok(records)
}

/// The state of [`read_plan9`].
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    slots: Vec<Option<String>>,
}

impl Reader<'_> {
    fn take(&mut self, n: usize, start: usize) -> Result<&[u8], Plan9Error> {
        let end = self.at + n;
        let got = self.bytes.get(self.at..end).ok_or(Plan9Error::Truncated { at: start })?;
        self.at = end;
        Ok(got)
    }

    fn u8(&mut self, start: usize) -> Result<u8, Plan9Error> {
        Ok(self.take(1, start)?[0])
    }

    fn u16(&mut self, start: usize) -> Result<u16, Plan9Error> {
        let b = self.take(2, start)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self, start: usize) -> Result<u32, Plan9Error> {
        let b = self.take(4, start)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn name(&mut self, start: usize) -> Result<String, Plan9Error> {
        let rest = &self.bytes[self.at..];
        let len = rest.iter().position(|&b| b == 0).ok_or(Plan9Error::Truncated { at: start })?;
        let name = String::from_utf8_lossy(&rest[..len]).into_owned();
        self.at += len + 1;
        Ok(name)
    }

    fn sym(&mut self, start: usize) -> Result<Option<String>, Plan9Error> {
        let slot = self.u8(start)?;
        if usize::from(slot) > NSYM {
            return Err(Plan9Error::Slot { at: start, slot });
        }
        Ok(if slot == 0 { None } else { self.slots[usize::from(slot)].clone() })
    }

    fn string(&mut self, start: usize) -> Result<[u8; NSNAME], Plan9Error> {
        let mut string = [0; NSNAME];
        string.copy_from_slice(self.take(NSNAME, start)?);
        Ok(string)
    }

    fn float(&mut self, start: usize) -> Result<f64, Plan9Error> {
        let low = u64::from(self.u32(start)?);
        let high = u64::from(self.u32(start)?);
        Ok(f64::from_bits(high << 32 | low))
    }

    /// `zaddr` in `6l/obj.c`.
    fn addr6(&mut self, start: usize) -> Result<Addr6, Plan9Error> {
        let t = self.u8(start)?;
        let mut a = Addr6::default();
        if t & amd64::T_INDEX != 0 {
            a.index = self.u8(start)?;
            a.scale = self.u8(start)?;
        }
        if t & amd64::T_OFFSET != 0 {
            let low = self.u32(start)?;
            a.offset = i64::from(low as i32);
            if t & amd64::T_64 != 0 {
                let high = self.u32(start)?;
                a.offset = (i64::from(high as i32) << 32) | i64::from(low);
            }
        }
        if t & amd64::T_SYM != 0 {
            a.sym = self.sym(start)?;
        }
        if t & amd64::T_FCONST != 0 {
            a.float = self.float(start)?;
            a.ty = amd64::D_FCONST;
        } else if t & amd64::T_SCONST != 0 {
            a.string = self.string(start)?;
            a.ty = amd64::D_SCONST;
        }
        if t & amd64::T_TYPE != 0 {
            a.ty = self.u8(start)?;
        }
        Ok(a)
    }

    /// `zaddr` in `7l/obj.c`. A `D_DCONST` operand is read as the `D_CONST` that it was.
    fn addr7(&mut self, start: usize) -> Result<Addr7, Plan9Error> {
        let head = self.take(4, start)?;
        let (ty, reg, slot, name) = (head[0], head[1], head[2], head[3]);
        if usize::from(slot) > NSYM {
            return Err(Plan9Error::Slot { at: start, slot });
        }
        let sym = if slot == 0 { None } else { self.slots[usize::from(slot)].clone() };
        let mut a = Addr7 { ty, reg, name, sym, ..Addr7::default() };
        match ty {
            arm64::D_NONE
            | arm64::D_REG
            | arm64::D_SP
            | arm64::D_FREG
            | arm64::D_VREG
            | arm64::D_COND => {}
            arm64::D_OREG
            | arm64::D_XPRE
            | arm64::D_XPOST
            | arm64::D_CONST
            | arm64::D_BRANCH
            | arm64::D_SHIFT
            | arm64::D_EXTREG
            | arm64::D_ROFF
            | arm64::D_SPR => a.offset = i64::from(self.u32(start)? as i32),
            arm64::D_DCONST => {
                let low = self.u32(start)?;
                let high = self.u32(start)?;
                a.offset = (i64::from(high as i32) << 32) | i64::from(low);
                a.ty = arm64::D_CONST;
            }
            arm64::D_SCONST => a.string = self.string(start)?,
            arm64::D_FCONST => a.float = self.float(start)?,
            ty => return Err(Plan9Error::Operand { ty }),
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_special_opcodes_are_at_their_index() {
        let six = [
            (amd64::ADATA, "ADATA"),
            (amd64::AGLOBL, "AGLOBL"),
            (amd64::AHISTORY, "AHISTORY"),
            (amd64::ANAME, "ANAME"),
            (amd64::ATEXT, "ATEXT"),
            (amd64::AEND, "AEND"),
            (amd64::ASIGNAME, "ASIGNAME"),
        ];
        for (op, name) in six {
            assert_eq!(Machine::Amd64.opcode_name(op), Some(name));
        }
        let seven = [
            (arm64::ATEXT, "ATEXT"),
            (arm64::ADATA, "ADATA"),
            (arm64::AGLOBL, "AGLOBL"),
            (arm64::AHISTORY, "AHISTORY"),
            (arm64::ANAME, "ANAME"),
            (arm64::ASIGNAME, "ASIGNAME"),
            (arm64::AEND, "AEND"),
        ];
        for (op, name) in seven {
            assert_eq!(Machine::Arm64.opcode_name(op), Some(name));
        }
        assert_eq!(amd64::NAMES.len(), 680);
        assert_eq!(arm64::NAMES.len(), 377);
    }

    /// `TEXT main(SB), $16`, `MOVQ $1, AX`, `RET` and `AEND`, written as `6c` writes them.
    #[test]
    fn a_small_amd64_function_has_the_bytes_of_6c() {
        let mut text = Addr6::symbol(amd64::D_EXTERN, "main", 0);
        text.scale = 0;
        let ret = amd64::NAMES.iter().position(|&n| n == "ARET").expect("RET");
        let movq = amd64::NAMES.iter().position(|&n| n == "AMOVQ").expect("MOVQ");
        let progs = vec![
            Prog6::new(amd64::ATEXT, 3, text, Addr6::constant(16)),
            Prog6::new(movq as u16, 4, Addr6::constant(1), Addr6::of(amd64::D_AX)),
            Prog6::new(ret as u16, 5, Addr6::default(), Addr6::default()),
            Prog6::new(amd64::AEND, 0, Addr6::default(), Addr6::default()),
        ];
        let object = Object::new(Progs::Amd64(progs.clone()));
        let bytes = write_plan9(&object).expect("the records can be written");
        let mut want = Vec::new();
        // ANAME, D_EXTERN, slot 1, "main".
        want.extend_from_slice(&amd64::ANAME.to_le_bytes());
        want.extend_from_slice(&[amd64::D_EXTERN, 1, b'm', b'a', b'i', b'n', 0]);
        // TEXT: from has a type and a symbol, to has a type and an offset.
        want.extend_from_slice(&amd64::ATEXT.to_le_bytes());
        want.extend_from_slice(&3u32.to_le_bytes());
        want.extend_from_slice(&[amd64::T_TYPE | amd64::T_SYM, 1, amd64::D_EXTERN]);
        want.extend_from_slice(&[amd64::T_TYPE | amd64::T_OFFSET, 16, 0, 0, 0, amd64::D_CONST]);
        want.extend_from_slice(&(movq as u16).to_le_bytes());
        want.extend_from_slice(&4u32.to_le_bytes());
        want.extend_from_slice(&[amd64::T_TYPE | amd64::T_OFFSET, 1, 0, 0, 0, amd64::D_CONST]);
        want.extend_from_slice(&[amd64::T_TYPE, amd64::D_AX]);
        want.extend_from_slice(&(ret as u16).to_le_bytes());
        want.extend_from_slice(&5u32.to_le_bytes());
        want.extend_from_slice(&[0, 0]);
        want.extend_from_slice(&amd64::AEND.to_le_bytes());
        want.extend_from_slice(&0u32.to_le_bytes());
        want.extend_from_slice(&[0, 0]);
        assert_eq!(bytes, want);

        let records = read_plan9(Machine::Amd64, &bytes).expect("the records can be read");
        let read: Vec<Prog6> = records
            .into_iter()
            .filter_map(|r| if let Record::Amd64(p) = r { Some(p) } else { None })
            .collect();
        assert_eq!(read, progs);
    }

    #[test]
    fn a_large_offset_and_constants_round_trip() {
        let progs = vec![
            Prog6::new(
                amd64::ADATA,
                1,
                Addr6 { scale: 8, ..Addr6::symbol(amd64::D_STATIC, "tab", 8) },
                Addr6::constant(0x1234_5678_9abc),
            ),
            Prog6::new(
                amd64::ADATA,
                1,
                Addr6 { scale: 8, ..Addr6::symbol(amd64::D_STATIC, "tab", 16) },
                Addr6 { ty: amd64::D_SCONST, string: *b"hello\0\0\0", ..Addr6::default() },
            ),
            Prog6::new(
                amd64::ADATA,
                1,
                Addr6 { scale: 8, ..Addr6::symbol(amd64::D_STATIC, "tab", 24) },
                Addr6 { ty: amd64::D_FCONST, float: 1.5, ..Addr6::default() },
            ),
            Prog6::new(
                amd64::ADATA,
                1,
                Addr6 { scale: 8, ..Addr6::symbol(amd64::D_STATIC, "tab", 32) },
                Addr6 { index: amd64::D_EXTERN, ..Addr6::symbol(amd64::D_ADDR, "main", 0) },
            ),
            Prog6::new(
                amd64::AGLOBL,
                1,
                Addr6::symbol(amd64::D_STATIC, "tab", 0),
                Addr6::constant(-8),
            ),
            Prog6::new(amd64::AEND, 0, Addr6::default(), Addr6::default()),
        ];
        let bytes = write_plan9(&Object::new(Progs::Amd64(progs.clone()))).expect("written");
        let records = read_plan9(Machine::Amd64, &bytes).expect("read");
        let names: Vec<&str> = records
            .iter()
            .filter_map(
                |r| if let Record::Name { name, .. } = r { Some(name.as_str()) } else { None },
            )
            .collect();
        // `main` is named once with the type of the thing it addresses.
        assert_eq!(names, ["tab", "main"]);
        let read: Vec<Prog6> = records
            .into_iter()
            .filter_map(|r| if let Record::Amd64(p) = r { Some(p) } else { None })
            .collect();
        assert_eq!(read, progs);
    }

    #[test]
    fn the_cache_wraps_after_the_last_slot_and_names_a_symbol_again() {
        let mut progs = Vec::new();
        for i in 0..60 {
            let sym = format!("s{i}");
            progs.push(Prog6::new(
                amd64::AGLOBL,
                1,
                Addr6::symbol(amd64::D_EXTERN, &sym, 0),
                Addr6::constant(8),
            ));
        }
        // `s0` was in slot 1, which `s49` has now.
        progs.push(Prog6::new(
            amd64::AGLOBL,
            1,
            Addr6::symbol(amd64::D_EXTERN, "s0", 0),
            Addr6::constant(8),
        ));
        // `s59` is still in its slot, so it is not named again.
        progs.push(Prog6::new(
            amd64::AGLOBL,
            1,
            Addr6::symbol(amd64::D_EXTERN, "s59", 0),
            Addr6::constant(8),
        ));
        let bytes = write_plan9(&Object::new(Progs::Amd64(progs.clone()))).expect("written");
        let records = read_plan9(Machine::Amd64, &bytes).expect("read");
        let names: Vec<(String, u8)> = records
            .iter()
            .filter_map(|r| match r {
                Record::Name { name, slot, .. } => Some((name.clone(), *slot)),
                _ => None,
            })
            .collect();
        assert_eq!(names.len(), 61);
        assert_eq!(names[48], ("s48".to_owned(), 49));
        assert_eq!(names[49], ("s49".to_owned(), 1));
        assert_eq!(names[59], ("s59".to_owned(), 11));
        assert_eq!(names[60], ("s0".to_owned(), 12));
        let read: Vec<Prog6> = records
            .into_iter()
            .filter_map(|r| if let Record::Amd64(p) = r { Some(p) } else { None })
            .collect();
        assert_eq!(read, progs);
    }

    #[test]
    fn a_symbol_with_two_types_has_two_slots() {
        // `MOVQ $x(SB), x(SB)` names `x` as the address of a static and as an external. The
        // slot holds a symbol with one type only, so the two go in two slots.
        let movq = amd64::NAMES.iter().position(|&n| n == "AMOVQ").expect("MOVQ") as u16;
        let addr = Addr6 { index: amd64::D_STATIC, ..Addr6::symbol(amd64::D_ADDR, "x", 0) };
        let progs = vec![Prog6::new(movq, 1, addr, Addr6::symbol(amd64::D_EXTERN, "x", 0))];
        let bytes = write_plan9(&Object::new(Progs::Amd64(progs))).expect("written");
        let records = read_plan9(Machine::Amd64, &bytes).expect("read");
        let slots: Vec<(u8, u8)> = records
            .iter()
            .filter_map(|r| match r {
                Record::Name { ty, slot, .. } => Some((*ty, *slot)),
                _ => None,
            })
            .collect();
        assert_eq!(slots, [(amd64::D_STATIC, 1), (amd64::D_EXTERN, 2)]);
    }

    #[test]
    fn the_first_external_name_carries_its_signature() {
        let progs = vec![
            Prog6::new(
                amd64::AGLOBL,
                1,
                Addr6::symbol(amd64::D_EXTERN, "x", 0),
                Addr6::constant(4),
            ),
            Prog6::new(
                amd64::AGLOBL,
                1,
                Addr6::symbol(amd64::D_STATIC, "y", 0),
                Addr6::constant(4),
            ),
        ];
        let mut object = Object::new(Progs::Amd64(progs));
        object.signed = true;
        object.signatures.insert("x".to_owned(), 0x4151_d5bd);
        let bytes = write_plan9(&object).expect("written");
        let records = read_plan9(Machine::Amd64, &bytes).expect("read");
        let signatures: Vec<Option<u32>> = records
            .iter()
            .filter_map(|r| match r {
                Record::Name { signature, .. } => Some(*signature),
                _ => None,
            })
            .collect();
        assert_eq!(signatures, [Some(0x4151_d5bd), None]);
    }

    #[test]
    fn the_history_splits_a_path_and_follows_the_directory() {
        let mut object = Object::new(Progs::Amd64(vec![]));
        object.directory = Some("/sys/src/cmd".to_owned());
        object.history = vec![
            History::push("cat.c", 1),
            History::push("/sys/include/libc.h", 2),
            History::pop(900),
            History::library("libc.a", 3),
        ];
        let bytes = write_plan9(&object).expect("written");
        let records = read_plan9(Machine::Amd64, &bytes).expect("read");
        let mut files = Vec::new();
        let mut entries = Vec::new();
        for record in records {
            match record {
                Record::Name { ty, slot, name, .. } => {
                    assert_eq!((ty, slot), (amd64::D_FILE, 1));
                    files.push(name);
                }
                Record::Amd64(p) => {
                    assert_eq!(p.op, amd64::AHISTORY);
                    entries.push((p.line, p.to.ty, p.to.offset));
                }
                Record::Arm64(_) => unreachable!(),
            }
        }
        let want = [
            "</", "<sys", "<src", "<cmd", "<cat.c", "</", "<sys", "<include", "<libc.h", "<libc.a",
        ];
        assert_eq!(files, want);
        assert_eq!(
            entries,
            [
                (1, amd64::D_NONE, 0),
                (2, amd64::D_NONE, 0),
                (900, amd64::D_NONE, 0),
                (3, amd64::D_CONST, -1),
            ]
        );
        // A pop is the record and nothing else: 2 bytes of opcode, 4 of line and two empty
        // operands.
        let pop: [u8; 8] = [
            amd64::AHISTORY.to_le_bytes()[0],
            amd64::AHISTORY.to_le_bytes()[1],
            0x84,
            0x03,
            0,
            0,
            0,
            0,
        ];
        assert!(bytes.windows(8).any(|w| w == pop));
    }

    #[test]
    fn an_arm64_record_has_a_middle_register_and_a_third_operand() {
        let add = arm64::NAMES.iter().position(|&n| n == "AADD").expect("ADD") as u16;
        let madd = arm64::NAMES.iter().position(|&n| n == "AMADD").expect("MADD") as u16;
        let mut three =
            Prog7::new(madd, 7, Addr7::register(arm64::D_REG, 1), Addr7::register(arm64::D_REG, 0));
        three.reg = 2;
        three.from3 = Addr7::register(arm64::D_REG, 3);
        let mut text = Prog7::new(arm64::ATEXT, 1, Addr7::default(), Addr7::constant(0));
        text.from.ty = arm64::D_OREG;
        text.from.name = arm64::D_EXTERN;
        text.from.sym = Some("f".to_owned());
        let progs = vec![
            text,
            Prog7::new(add, 2, Addr7::constant(1 << 40), Addr7::register(arm64::D_REG, 0)),
            three,
            Prog7::new(arm64::AEND, 0, Addr7::default(), Addr7::default()),
        ];
        let bytes = write_plan9(&Object::new(Progs::Arm64(progs.clone()))).expect("written");
        // The 64-bit constant is written as `D_DCONST`.
        assert!(bytes.windows(4).any(|w| w == [arm64::D_DCONST, arm64::NREG, 0, arm64::D_NONE]));
        // The third operand is marked in the register byte.
        assert!(bytes.windows(3).any(|w| w == [madd as u8, (madd >> 8) as u8, 0x42]));
        let records = read_plan9(Machine::Arm64, &bytes).expect("read");
        let read: Vec<Prog7> = records
            .into_iter()
            .filter_map(|r| if let Record::Arm64(p) = r { Some(p) } else { None })
            .collect();
        assert_eq!(read, progs);
    }

    #[test]
    fn a_register_pair_is_refused_on_arm64() {
        let progs = vec![Prog7::new(1, 1, Addr7::of(arm64::D_PAIR), Addr7::default())];
        let err = write_plan9(&Object::new(Progs::Arm64(progs))).expect_err("refused");
        assert_eq!(err, Plan9Error::Operand { ty: arm64::D_PAIR });
    }
}
