//! The unwind table Windows reads on AArch64, which is the same two sections as on x86-64 with
//! other things in them.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4, and Microsoft's "ARM64 exception handling"
//! for the encoding, which is the platform's and not a choice.
//!
//! # What a record is here
//!
//! A row of `.pdata` is two words rather than three: where the function starts, and either where its
//! description is in `.xdata` or, with the low two bits set, the whole description packed into the
//! word itself. The end of the function is in the description, as a count of instructions.
//!
//! A description is a header, a list of epilogues, and a string of unwind codes. Each code is one
//! instruction of the prologue or of an epilogue, and the unwinder runs them as it finds them, so
//! the prologue's are written last instruction first, the order they are undone in, and an
//! epilogue's are written in the order its instructions run, which is the same order. That is why
//! an epilogue that mirrors the prologue needs no codes of its own: it names the index its codes
//! start at, and that can be the prologue's codes or the tail of them.
//!
//! Every code stands for exactly one instruction. An unwinder stopped part of the way through a
//! prologue works out how many of its instructions have run by counting codes back from the end of
//! the prologue, so a prologue whose codes and instructions do not pair off is a record that is
//! wrong at every address in it. The instructions nothing needs undoing for are `nop` codes. A file
//! whose directives do not pair off is refused rather than written.
//!
//! # What this does not write
//!
//! No exception handler, the same as the x86-64 half, and no record for a function longer than the
//! eighteen bits of instruction count a header holds, which Windows would split into fragments.
//!
//! # Packed rows
//!
//! A function whose prologue is the one shape Windows can rebuild from a few counts gets no
//! description at all: the counts go in the row. Which functions those are is decided here the way
//! clang decides it, and clang's short forms of a few codes are used the way it uses them, so a file
//! of assembly clang wrote gets from this the table clang would have given it.

use std::fmt;

use rucc_object::{Extent, Marker, Reference, Reloc, Unwind};

use crate::Error;

/// One unwind code, as the directive clang and gas spell it and as the instruction it stands for.
///
/// The offsets are bytes, as the directives give them, and every one of them is a multiple of eight
/// because the codes count in eights. Registers are by number: `x19` is nineteen and `d8` is eight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Code {
    /// `.seh_stackalloc`: `sub sp, sp, #size`, a multiple of sixteen.
    Alloc(u32),
    /// `.seh_save_r19r20_x`: `stp x19, x20, [sp, #-offset]!`.
    R19R20X(u32),
    /// `.seh_save_fplr`: `stp x29, x30, [sp, #offset]`.
    Fplr(u32),
    /// `.seh_save_fplr_x`: `stp x29, x30, [sp, #-offset]!`.
    FplrX(u32),
    /// `.seh_save_regp`: `stp xN, xN+1, [sp, #offset]`.
    Regp(u8, u32),
    /// `.seh_save_regp_x`: `stp xN, xN+1, [sp, #-offset]!`.
    RegpX(u8, u32),
    /// `.seh_save_reg`: `str xN, [sp, #offset]`.
    Reg(u8, u32),
    /// `.seh_save_reg_x`: `str xN, [sp, #-offset]!`.
    RegX(u8, u32),
    /// `.seh_save_lrpair`: `stp xN, lr, [sp, #offset]`.
    Lrpair(u8, u32),
    /// `.seh_save_fregp`: `stp dN, dN+1, [sp, #offset]`.
    Fregp(u8, u32),
    /// `.seh_save_fregp_x`: `stp dN, dN+1, [sp, #-offset]!`.
    FregpX(u8, u32),
    /// `.seh_save_freg`: `str dN, [sp, #offset]`.
    Freg(u8, u32),
    /// `.seh_save_freg_x`: `str dN, [sp, #-offset]!`.
    FregX(u8, u32),
    /// `.seh_set_fp`: `mov x29, sp`.
    SetFp,
    /// `.seh_add_fp`: `add x29, sp, #offset`.
    AddFp(u32),
    /// `.seh_nop`: an instruction that did nothing the unwinder has to undo.
    Nop,
    /// `.seh_save_next`: the next pair after the one the code before it saved, eight bytes higher.
    SaveNext,
    /// `.seh_pac_sign_lr`: `pacibsp`.
    PacSignLr,
}

impl fmt::Display for Code {
    /// The directive an assembler reads this code from.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Code::Alloc(size) => write!(f, "\t.seh_stackalloc\t{size}"),
            Code::R19R20X(offset) => write!(f, "\t.seh_save_r19r20_x\t{offset}"),
            Code::Fplr(offset) => write!(f, "\t.seh_save_fplr\t{offset}"),
            Code::FplrX(offset) => write!(f, "\t.seh_save_fplr_x\t{offset}"),
            Code::Regp(reg, offset) => write!(f, "\t.seh_save_regp\tx{reg}, {offset}"),
            Code::RegpX(reg, offset) => write!(f, "\t.seh_save_regp_x\tx{reg}, {offset}"),
            Code::Reg(reg, offset) => write!(f, "\t.seh_save_reg\tx{reg}, {offset}"),
            Code::RegX(reg, offset) => write!(f, "\t.seh_save_reg_x\tx{reg}, {offset}"),
            Code::Lrpair(reg, offset) => write!(f, "\t.seh_save_lrpair\tx{reg}, {offset}"),
            Code::Fregp(reg, offset) => write!(f, "\t.seh_save_fregp\td{reg}, {offset}"),
            Code::FregpX(reg, offset) => write!(f, "\t.seh_save_fregp_x\td{reg}, {offset}"),
            Code::Freg(reg, offset) => write!(f, "\t.seh_save_freg\td{reg}, {offset}"),
            Code::FregX(reg, offset) => write!(f, "\t.seh_save_freg_x\td{reg}, {offset}"),
            Code::SetFp => write!(f, "\t.seh_set_fp"),
            Code::AddFp(offset) => write!(f, "\t.seh_add_fp\t{offset}"),
            Code::Nop => write!(f, "\t.seh_nop"),
            Code::SaveNext => write!(f, "\t.seh_save_next"),
            Code::PacSignLr => write!(f, "\t.seh_pac_sign_lr"),
        }
    }
}

impl Code {
    /// The code read back from a directive, given its name without the dot and what came after it,
    /// or `None` for a name that is not one of these. A register or a number out of what the code
    /// can say is an error rather than `None`.
    pub(crate) fn read(word: &str, args: &[String]) -> Option<Result<Code, String>> {
        let args: Vec<&str> = args.iter().map(|arg| arg.trim()).collect();
        let number = |text: &str| -> Result<u32, String> {
            let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                Some(hex) => u32::from_str_radix(hex, 16),
                None => text.strip_prefix('#').unwrap_or(text).parse(),
            };
            parsed.map_err(|_| format!("'{text}' is not a number '.{word}' takes"))
        };
        let one = || match args.as_slice() {
            [offset] => number(offset),
            _ => Err(format!("'.{word}' takes one number")),
        };
        let two = |bank: char| -> Result<(u8, u32), String> {
            let [reg, offset] = args.as_slice() else {
                return Err(format!("'.{word}' takes a register and a number"));
            };
            let lower = reg.to_ascii_lowercase();
            let named = match lower.as_str() {
                "fp" if bank == 'x' => Some(29),
                "lr" if bank == 'x' => Some(30),
                _ => lower.strip_prefix(bank).and_then(|n| n.parse::<u8>().ok()),
            };
            let reg = named.ok_or_else(|| format!("'{reg}' is not a register '.{word}' names"))?;
            Ok((reg, number(offset)?))
        };
        let none = || {
            if args.iter().all(|arg| arg.is_empty()) {
                Ok(())
            } else {
                Err(format!("'.{word}' takes nothing after it"))
            }
        };
        let code = match word {
            "seh_stackalloc" => one().map(Code::Alloc),
            "seh_save_r19r20_x" => one().map(Code::R19R20X),
            "seh_save_fplr" => one().map(Code::Fplr),
            "seh_save_fplr_x" => one().map(Code::FplrX),
            "seh_save_regp" => two('x').map(|(reg, offset)| Code::Regp(reg, offset)),
            "seh_save_regp_x" => two('x').map(|(reg, offset)| Code::RegpX(reg, offset)),
            "seh_save_reg" => two('x').map(|(reg, offset)| Code::Reg(reg, offset)),
            "seh_save_reg_x" => two('x').map(|(reg, offset)| Code::RegX(reg, offset)),
            "seh_save_lrpair" => two('x').map(|(reg, offset)| Code::Lrpair(reg, offset)),
            "seh_save_fregp" => two('d').map(|(reg, offset)| Code::Fregp(reg, offset)),
            "seh_save_fregp_x" => two('d').map(|(reg, offset)| Code::FregpX(reg, offset)),
            "seh_save_freg" => two('d').map(|(reg, offset)| Code::Freg(reg, offset)),
            "seh_save_freg_x" => two('d').map(|(reg, offset)| Code::FregX(reg, offset)),
            "seh_set_fp" => none().map(|()| Code::SetFp),
            "seh_add_fp" => one().map(Code::AddFp),
            "seh_nop" => none().map(|()| Code::Nop),
            "seh_save_next" => none().map(|()| Code::SaveNext),
            "seh_pac_sign_lr" => none().map(|()| Code::PacSignLr),
            _ => return None,
        };
        // Encoded once here, so a code that cannot be said is refused on the line that said it
        // rather than when the table is written.
        Some(code.and_then(|code| code.bytes().map(|_| code)))
    }

    /// The bytes of this code, first byte first, which is how the unwinder reads them.
    ///
    /// Each code has a few bits for the register, counted from the first one it can name, and a
    /// few for the offset, counted in eights and one less than it for the forms that also move the
    /// stack pointer, since those never move it by nothing.
    pub(crate) fn bytes(self) -> Result<Vec<u8>, String> {
        let eights = |offset: u32, bits: u32, less: u32| -> Result<u32, String> {
            let scaled = (offset / 8).checked_sub(less).filter(|&n| n < 1 << bits);
            match scaled {
                Some(n) if offset % 8 == 0 => Ok(n),
                _ => {
                    Err(format!("{offset} is not an offset '{}' can say", self.to_string().trim()))
                }
            }
        };
        let reg = |reg: u8, first: u8, last: u8| -> Result<u32, String> {
            if (first..=last).contains(&reg) {
                Ok(u32::from(reg - first))
            } else {
                Err(format!("register {reg} is not one '{}' can say", self.to_string().trim()))
            }
        };
        let two = |word: u32| vec![(word >> 8) as u8, word as u8];
        Ok(match self {
            Code::Alloc(size) if size % 16 != 0 => {
                return Err(format!("a frame of {size} bytes, which is not a multiple of sixteen"));
            }
            Code::Alloc(size) if size / 16 < 1 << 5 => vec![(size / 16) as u8],
            Code::Alloc(size) if size / 16 < 1 << 11 => two(0xc000 | (size / 16)),
            Code::Alloc(size) if size / 16 < 1 << 24 => {
                let n = size / 16;
                vec![0xe0, (n >> 16) as u8, (n >> 8) as u8, n as u8]
            }
            Code::Alloc(size) => return Err(format!("a frame of {size} bytes")),
            Code::R19R20X(offset) => vec![0x20 | eights(offset, 5, 0)? as u8],
            Code::Fplr(offset) => vec![0x40 | eights(offset, 6, 0)? as u8],
            Code::FplrX(offset) => vec![0x80 | eights(offset, 6, 1)? as u8],
            Code::Regp(r, offset) => two(0xc800 | reg(r, 19, 28)? << 6 | eights(offset, 6, 0)?),
            Code::RegpX(r, offset) => two(0xcc00 | reg(r, 19, 28)? << 6 | eights(offset, 6, 1)?),
            Code::Reg(r, offset) => two(0xd000 | reg(r, 19, 30)? << 6 | eights(offset, 6, 0)?),
            Code::RegX(r, offset) => two(0xd400 | reg(r, 19, 30)? << 5 | eights(offset, 5, 1)?),
            Code::Lrpair(r, offset) => {
                if (r - 19.min(r)) % 2 != 0 {
                    return Err(format!("x{r} is not one of the pairs '.seh_save_lrpair' says"));
                }
                two(0xd600 | (reg(r, 19, 27)? / 2) << 6 | eights(offset, 6, 0)?)
            }
            Code::Fregp(r, offset) => two(0xd800 | reg(r, 8, 14)? << 6 | eights(offset, 6, 0)?),
            Code::FregpX(r, offset) => two(0xda00 | reg(r, 8, 14)? << 6 | eights(offset, 6, 1)?),
            Code::Freg(r, offset) => two(0xdc00 | reg(r, 8, 15)? << 6 | eights(offset, 6, 0)?),
            Code::FregX(r, offset) => two(0xde00 | reg(r, 8, 15)? << 5 | eights(offset, 5, 1)?),
            Code::SetFp => vec![0xe1],
            Code::AddFp(offset) => vec![0xe2, eights(offset, 8, 0)? as u8],
            Code::Nop => vec![NOP],
            Code::SaveNext => vec![0xe6],
            Code::PacSignLr => vec![0xfc],
        })
    }
}

/// The code that ends a list, which stands for the `ret` after an epilogue.
const END: u8 = 0xe4;

/// The code for an instruction with nothing to undo, which is also what pads the codes out to a
/// whole word, as clang pads them.
const NOP: u8 = 0xe3;

/// One epilogue: where its first instruction is, counted in bytes from the front of the function,
/// and its codes in the order its instructions run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Epilogue {
    pub(crate) start: usize,
    pub(crate) codes: Vec<Code>,
}

/// One function's prologue and epilogues as codes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Proc {
    /// The prologue's codes, in the order its instructions run, which is one to an instruction from
    /// the front of the function.
    pub(crate) prologue: Vec<Code>,
    pub(crate) epilogues: Vec<Epilogue>,
}

/// The largest function a header can count, in instructions.
const LONGEST: usize = (1 << 18) - 1;

/// The largest function a packed row can count, in instructions.
const PACKED_LONGEST: usize = (1 << 11) - 1;

/// What a row of `.pdata` says about its function after where it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Record {
    /// The whole description, in the row itself.
    Packed(u32),
    /// Where the description is: these bytes, in `.xdata`.
    Full(Vec<u8>),
}

/// The table: a description in `.xdata` for each function and a row for it in `.pdata`.
///
/// The row is where the function starts and either the description packed into one word or where
/// the description is, both as a distance from the front of the image. A description is found by a
/// local name for the same reason as on x86-64: it is in another section.
pub(crate) fn table(funcs: &[Extent], procs: &[Proc]) -> Result<Unwind, Error> {
    debug_assert_eq!(funcs.len(), procs.len(), "a record per function");
    let mut out = Unwind::default();
    for (func, proc) in funcs.iter().zip(procs) {
        let record = describe(func.len, proc)
            .map_err(|why| Error::Frame { func: func.name.clone(), why })?;
        let image = |symbol: String, at: usize| Reloc {
            at,
            symbol,
            kind: Reference::Image,
            addend: 0,
            after: 0,
        };
        out.relocs.push(image(func.name.clone(), out.bytes.len()));
        out.bytes.extend_from_slice(&0u32.to_le_bytes());
        match record {
            Record::Packed(word) => out.bytes.extend_from_slice(&word.to_le_bytes()),
            Record::Full(bytes) => {
                let name = format!("$unwind${}", func.name);
                out.labels.push(Marker { name: name.clone(), at: out.info.len() });
                out.info.extend_from_slice(&bytes);
                out.relocs.push(image(name, out.bytes.len()));
                out.bytes.extend_from_slice(&0u32.to_le_bytes());
            }
        }
    }
    Ok(out)
}

/// Codes swapped for the shorter ones that say the same thing, which is what clang does to the
/// directives it reads, so that a file of assembly gets the record clang would write for it.
///
/// A pair saved at `x29` is the frame record and has a code of its own, as does the first pair of
/// callee saved registers pushed together and a frame pointer set with no offset. A pair saved just
/// above the pair before it, two registers on, is `save_next`, which is one byte where the other is
/// two. Not for the vector registers, which clang leaves alone because Windows has read that wrong.
///
/// The pairs are looked at in the order they are saved, which is the order the prologue runs and
/// the reverse of the order an epilogue does, so `reverse` is for an epilogue.
fn simplify(codes: &mut [Code], reverse: bool) {
    let mut previous: Option<(u8, u32)> = None;
    let mut visit = |code: &mut Code| {
        *code = match *code {
            Code::Regp(29, offset) => Code::Fplr(offset),
            Code::RegpX(29, offset) => Code::FplrX(offset),
            Code::RegpX(19, offset) if offset <= 248 => Code::R19R20X(offset),
            Code::AddFp(0) => Code::SetFp,
            Code::Regp(reg, offset)
                if previous == Some((reg.wrapping_sub(2), offset.wrapping_sub(16))) =>
            {
                Code::SaveNext
            }
            other => other,
        };
        previous = match *code {
            Code::R19R20X(_) => Some((19, 0)),
            Code::RegpX(reg, _) => Some((reg, 0)),
            Code::Regp(reg, offset) => Some((reg, offset)),
            Code::SaveNext => previous.map(|(reg, offset)| (reg + 2, offset + 16)),
            _ => None,
        };
    };
    if reverse {
        codes.iter_mut().rev().for_each(&mut visit);
    } else {
        codes.iter_mut().for_each(&mut visit);
    }
}

/// How many bytes a list of codes is.
fn size(codes: &[Code]) -> Result<usize, String> {
    codes.iter().try_fold(0, |sum, code| Ok(sum + code.bytes()?.len()))
}

/// Where an epilogue's codes are in the prologue's, if it undoes the first of the prologue's
/// instructions in the reverse order they ran, which is the tail of the prologue's codes as they
/// are written, `end` and all. The index is past the codes of the prologue it leaves alone.
fn in_prologue(prologue: &[Code], epilogue: &[Code]) -> Result<Option<usize>, String> {
    if epilogue.len() > prologue.len() {
        return Ok(None);
    }
    if !epilogue.iter().rev().eq(&prologue[..epilogue.len()]) {
        return Ok(None);
    }
    size(&prologue[epilogue.len()..]).map(Some)
}

/// One function's description, packed into the row when it fits and whole otherwise, the choice
/// clang makes and made the way it makes it.
///
/// A whole one is the header word first: how many instructions the function is, then two flags,
/// then how many epilogues there are and how many words of codes. When there is one epilogue and it
/// is the last thing in the function, its scope is left out and the field for the count holds the
/// index its codes start at instead, which is the `E` bit. Counts too big for the five bits the
/// header has for each go in a second word, marked by both fields being zero.
///
/// Then a word per epilogue saying where it starts, in instructions, and the index its codes start
/// at. Then the codes: the prologue's, last instruction first, and `end`, then whatever epilogue
/// codes are not already there, each list with its own `end`. An epilogue that undoes the first of
/// the prologue's instructions points into the prologue's codes, and one whose list another epilogue
/// already wrote points at that.
fn describe(len: usize, proc: &Proc) -> Result<Record, String> {
    if len % 4 != 0 {
        return Err(format!("a function {len} bytes long, which is not a whole number of words"));
    }
    if len / 4 > LONGEST {
        return Err(format!("a function of {} instructions, more than a record counts", len / 4));
    }
    let mut prologue = proc.prologue.clone();
    simplify(&mut prologue, false);
    let mut epilogues = proc.epilogues.clone();
    for epilogue in &mut epilogues {
        if epilogue.start % 4 != 0 || epilogue.start >= len {
            let at = epilogue.start;
            return Err(format!("an epilogue at byte {at}, which is not an instruction"));
        }
        simplify(&mut epilogue.codes, true);
    }
    let prologue_bytes = size(&prologue)? + 1;

    // One epilogue, the last instructions of the function, its codes and the `ret` after them, has
    // no scope of its own. Its index is in the header, and is either into the prologue's codes or
    // just after them.
    let mut shared = false;
    let packed = match epilogues.as_slice() {
        [only] if (len - only.start) / 4 == only.codes.len() + 1 => {
            let own = size(&only.codes)? + 1;
            let after =
                (prologue_bytes <= 31 && prologue_bytes + own <= 124).then_some(prologue_bytes);
            match in_prologue(&prologue, &only.codes)? {
                Some(index) if index <= 31 && prologue_bytes <= 124 => {
                    shared = true;
                    Some(index)
                }
                _ => after,
            }
        }
        _ => None,
    };
    let word = packed
        .filter(|&index| index < prologue_bytes && len / 4 <= PACKED_LONGEST)
        .and_then(|index| pack(len, &prologue, index));
    if let Some(word) = word {
        return Ok(Record::Packed(word));
    }

    let mut codes = Vec::new();
    for code in prologue.iter().rev() {
        codes.extend(code.bytes()?);
    }
    codes.push(END);
    let mut scopes = Vec::with_capacity(epilogues.len());
    let mut written: Vec<(&[Code], usize)> = Vec::new();
    for epilogue in &epilogues {
        if shared {
            break;
        }
        let index = match written.iter().find(|(codes, _)| *codes == epilogue.codes.as_slice()) {
            Some(&(_, index)) => index,
            None => match in_prologue(&prologue, &epilogue.codes)? {
                Some(index) => index,
                None => {
                    let index = codes.len();
                    for code in &epilogue.codes {
                        codes.extend(code.bytes()?);
                    }
                    codes.push(END);
                    written.push((&epilogue.codes, index));
                    index
                }
            },
        };
        scopes.push((epilogue.start / 4, index));
    }
    let bytes = codes.len();
    while codes.len() % 4 != 0 {
        codes.push(NOP);
    }
    let words = codes.len() / 4;
    let (e, count) = match packed {
        Some(index) => (1, index),
        None => (0, scopes.len()),
    };
    let long = count >= 1 << 5 || bytes > 124;
    let too_many = |what: &str, n: usize, bits: u32| {
        (n >= 1 << bits).then(|| format!("{n} {what}, more than a record holds"))
    };
    if let Some(why) =
        too_many("epilogues", count, 16).or_else(|| too_many("words of unwind codes", words, 8))
    {
        return Err(why);
    }
    let mut out = Vec::new();
    let mut header = (len / 4) as u32 | (e as u32) << 21;
    if !long {
        header |= (count as u32) << 22 | (words as u32) << 27;
    }
    out.extend_from_slice(&header.to_le_bytes());
    if long {
        out.extend_from_slice(&(count as u32 | (words as u32) << 16).to_le_bytes());
    }
    if packed.is_none() {
        for (start, index) in scopes {
            if index >= 1 << 10 {
                return Err(format!("an epilogue whose codes start at byte {index} of them"));
            }
            out.extend_from_slice(&(start as u32 | (index as u32) << 22).to_le_bytes());
        }
    }
    out.extend_from_slice(&codes);
    Ok(Record::Full(out))
}

/// Where the packed form's canonical prologue has got to, as its codes are read in the order its
/// instructions run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Start,
    Signed,
    Ints,
    Floats,
    Homed,
    Adjust,
    Record,
    End,
}

/// The one word a row can hold instead of a description, when the prologue is the one the packed
/// form rebuilds from a handful of counts, or `None` when it is not.
///
/// The packed form describes one prologue: the integer registers from `x19` up saved in pairs, then
/// the vector ones from `d8`, then the frame, then the frame record with the frame pointer set to
/// it. The epilogue it rebuilds is the same one backwards, which is why only an epilogue that
/// mirrors the prologue can be packed, less the setting of the frame pointer. Each code has to be
/// in its place and at the offset that shape puts it, since an unwinder stopped part of the way
/// through works out where it is from the shape and not from the codes, and anything else is a
/// whole description. The checks are clang's, so the functions it packs are the ones packed here.
fn pack(len: usize, prologue: &[Code], index: usize) -> Option<u32> {
    match index {
        0 => {}
        1 if prologue.last() == Some(&Code::SetFp) => {}
        _ => return None,
    }
    let (mut ints, mut floats, mut lone_lr, mut record, mut signed) =
        (0u32, 0u32, false, false, false);
    let (mut predecrement, mut adjust, mut nops) = (0u32, 0u32, 0);
    let mut stage = Stage::Start;
    let first = |stage: Stage| matches!(stage, Stage::Start | Stage::Signed);
    for &code in prologue {
        match code {
            Code::PacSignLr if stage == Stage::Start => {
                signed = true;
                stage = Stage::Signed;
            }
            Code::R19R20X(offset) if first(stage) => {
                predecrement = offset;
                ints = 2;
                stage = Stage::Ints;
            }
            Code::RegX(reg, offset) if first(stage) && (reg == 19 || reg == 30) => {
                predecrement = offset;
                if reg == 19 {
                    ints += 1;
                } else {
                    lone_lr = true;
                }
                stage = Stage::Floats;
            }
            Code::Regp(reg, offset)
                if stage == Stage::Ints && offset == 8 * ints && u32::from(reg) == 19 + ints =>
            {
                ints += 2;
            }
            Code::Reg(reg, offset) if stage == Stage::Ints && offset == 8 * ints => {
                if u32::from(reg) == 19 + ints {
                    ints += 1;
                } else if reg == 30 {
                    lone_lr = true;
                } else {
                    return None;
                }
                stage = Stage::Floats;
            }
            Code::Lrpair(reg, offset)
                if stage == Stage::Ints && offset == 8 * ints && u32::from(reg) == 19 + ints =>
            {
                ints += 1;
                lone_lr = true;
                stage = Stage::Floats;
            }
            Code::Freg(reg, offset)
                if stage == Stage::Floats
                    && floats != 0
                    && u32::from(reg) == 8 + floats
                    && offset == 8 * (ints + u32::from(lone_lr) + floats) =>
            {
                floats += 1;
                stage = Stage::Homed;
            }
            Code::FregpX(8, offset) if first(stage) => {
                predecrement = offset;
                floats = 2;
                stage = Stage::Floats;
            }
            Code::Fregp(reg, offset)
                if matches!(stage, Stage::Ints | Stage::Floats)
                    && u32::from(reg) == 8 + floats
                    && offset == 8 * (ints + u32::from(lone_lr) + floats) =>
            {
                floats += 2;
                stage = Stage::Floats;
            }
            Code::SaveNext if stage == Stage::Ints => ints += 2,
            Code::SaveNext if stage == Stage::Floats => floats += 2,
            Code::Nop if matches!(stage, Stage::Ints | Stage::Floats | Stage::Homed) => {
                nops += 1;
                stage = Stage::Homed;
            }
            Code::Alloc(size)
                if size / 16 < 1 << 11
                    && matches!(
                        stage,
                        Stage::Start
                            | Stage::Signed
                            | Stage::Ints
                            | Stage::Floats
                            | Stage::Homed
                            | Stage::Adjust
                    ) =>
            {
                // One allocation, or two when the first is the most one instruction can take.
                adjust = match adjust {
                    0 => size,
                    4080 => 4080 + size,
                    _ => return None,
                };
                stage = Stage::Adjust;
            }
            Code::FplrX(offset)
                if matches!(
                    stage,
                    Stage::Start | Stage::Signed | Stage::Ints | Stage::Floats | Stage::Homed
                ) =>
            {
                adjust = offset;
                record = true;
                stage = Stage::Record;
            }
            Code::Fplr(0) if stage == Stage::Adjust => {
                record = true;
                stage = Stage::Record;
            }
            Code::SetFp if stage == Stage::Record => stage = Stage::End,
            _ => return None,
        }
    }
    // Homed parameters are four `nop`s, and Windows and its documentation disagree about whether
    // the epilogue has them, so like clang this never packs them.
    if ints > 10 || floats > 8 || (lone_lr && record) || (record && stage != Stage::End) {
        return None;
    }
    if nops != 0 || (signed && !record) {
        return None;
    }
    let saved = (8 * ints + 8 * u32::from(lone_lr) + 8 * floats + 15) & !15;
    if predecrement != saved || (record && adjust < 16) || adjust % 16 != 0 {
        return None;
    }
    let frame = (adjust + saved) / 16;
    if frame >= 1 << 9 {
        return None;
    }
    let chained = match (signed, record, lone_lr) {
        (true, ..) => 2,
        (false, true, _) => 3,
        (false, false, true) => 1,
        (false, false, false) => 0,
    };
    let floats = floats.saturating_sub(1);
    Some(1 | (len as u32 / 4) << 2 | floats << 13 | ints << 16 | chained << 21 | frame << 23)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extent(name: &str, len: usize) -> Extent {
        Extent {
            name: name.to_owned(),
            start: 0,
            len,
            align: 4,
            binding: rucc_object::Binding::Global,
            visibility: rucc_object::Visibility::Default,
            patch: None,
            hooked: 0,
            landings: Vec::new(),
        }
    }

    #[test]
    fn each_code_is_the_bytes_clang_writes_for_it() {
        // From llvm-readobj --unwind on clang's output for aarch64-w64-mingw32.
        for (code, bytes) in [
            (Code::Fplr(40), vec![0x45]),
            (Code::FplrX(32), vec![0x83]),
            (Code::Reg(23, 32), vec![0xd1, 0x04]),
            (Code::RegX(19, 32), vec![0xd4, 0x03]),
            (Code::Reg(28, 16), vec![0xd2, 0x42]),
            (Code::Fregp(8, 16), vec![0xd8, 0x02]),
            (Code::R19R20X(64), vec![0x28]),
            (Code::AddFp(40), vec![0xe2, 0x05]),
            (Code::SetFp, vec![0xe1]),
            (Code::Alloc(8192), vec![0xc2, 0x00]),
            (Code::Alloc(1808), vec![0xc0, 0x71]),
            (Code::Alloc(32), vec![0x02]),
            (Code::Alloc(1 << 20), vec![0xe0, 0x01, 0x00, 0x00]),
            (Code::RegpX(19, 64), vec![0xcc, 0x07]),
            (Code::Regp(21, 16), vec![0xc8, 0x82]),
        ] {
            assert_eq!(code.bytes(), Ok(bytes), "{code:?}");
        }
        assert!(Code::Alloc(24).bytes().is_err());
        assert!(Code::Reg(18, 8).bytes().is_err());
        assert!(Code::FplrX(0).bytes().is_err());
        assert!(Code::Fplr(512).bytes().is_err());
    }

    #[test]
    fn an_epilogue_at_the_end_that_mirrors_the_prologue_shares_its_codes() {
        // clang's `vla`: the prologue is `str x19, [sp, #-32]!`, `stp x29, x30, [sp, #8]` and
        // `add x29, sp, #8`, and the one epilogue undoes all three and is followed by `ret` at the
        // end of the function, so it is packed into the header and points at index nought.
        let proc = Proc {
            prologue: vec![Code::RegX(19, 32), Code::Fplr(8), Code::AddFp(8)],
            epilogues: vec![Epilogue {
                start: 48,
                codes: vec![Code::AddFp(8), Code::Fplr(8), Code::RegX(19, 32)],
            }],
        };
        let table = table(&[extent("vla", 64)], &[proc]).expect("a table");
        assert_eq!(
            table.info,
            [0x10, 0x00, 0x20, 0x10, 0xe2, 0x01, 0x41, 0xd4, 0x03, 0xe4, 0xe3, 0xe3]
        );
        assert_eq!(table.bytes.len(), 8);
        assert_eq!(table.relocs.len(), 2);
    }

    #[test]
    fn two_epilogues_in_the_middle_point_into_the_tail_of_the_prologue() {
        // clang's `twoexit`, which returns from two places and whose epilogues set no frame
        // pointer, so their codes start two bytes into the prologue's.
        let epilogue = |start| Epilogue { start, codes: vec![Code::Fplr(8), Code::RegX(19, 32)] };
        let proc = Proc {
            prologue: vec![Code::RegX(19, 32), Code::Fplr(8), Code::AddFp(8)],
            epilogues: vec![epilogue(16), epilogue(52)],
        };
        let table = table(&[extent("twoexit", 64)], &[proc]).expect("a table");
        assert_eq!(
            table.info,
            [
                0x10, 0x00, 0x80, 0x10, 0x04, 0x00, 0x80, 0x00, 0x0d, 0x00, 0x80, 0x00, 0xe2, 0x01,
                0x41, 0xd4, 0x03, 0xe4, 0xe3, 0xe3
            ]
        );
    }

    #[test]
    fn an_epilogue_unlike_the_prologue_has_codes_of_its_own() {
        // clang's `big`, whose epilogue gives the frame back before the registers.
        let proc = Proc {
            prologue: vec![Code::RegpX(19, 48), Code::Reg(28, 16), Code::Fplr(24), Code::AddFp(24)],
            epilogues: vec![Epilogue {
                start: 48,
                codes: vec![
                    Code::Alloc(8192),
                    Code::Alloc(1808),
                    Code::Fplr(24),
                    Code::Reg(28, 16),
                    Code::RegpX(19, 48),
                ],
            }],
        };
        let table = table(&[extent("big", 72)], &[proc]).expect("a table");
        let header = u32::from_le_bytes(table.info[..4].try_into().unwrap());
        assert_eq!(header & 0x3ffff, 18, "eighteen instructions");
        assert_eq!(header >> 21 & 1, 1, "one epilogue at the end");
        assert_eq!(header >> 22 & 31, 7, "whose codes start after the prologue's");
        assert_eq!(header >> 27, 4, "four words of codes");
    }

    #[test]
    fn a_function_that_never_returns_has_no_epilogue() {
        let proc = Proc { prologue: vec![Code::FplrX(16), Code::SetFp], epilogues: Vec::new() };
        let table = table(&[extent("f", 12)], &[proc]).expect("a table");
        assert_eq!(table.info, [0x03, 0x00, 0x00, 0x08, 0xe1, 0x81, 0xe4, 0xe3]);
    }
}
