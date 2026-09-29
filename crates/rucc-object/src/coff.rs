//! What COFF answers, where the two formats answer differently.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3 and `spec/cross-compile/07-object-formats.md`
//! section 7.4. The sibling of [`crate::elf`], and the shape of a file is [`crate::file`]'s for both.
//!
//! # The relocation that counts from the other end
//!
//! Both formats write the distance from an instruction to something into four bytes, and they
//! disagree about where that distance is measured from. ELF counts from where the four bytes start
//! and lets the addend make up whatever else is wanted, so one relocation type covers every
//! instruction. COFF counts from where the instruction ends, which is not a number it can be told,
//! so it is in the relocation type instead: `IMAGE_REL_AMD64_REL32` is an instruction that ends at
//! the hole and `REL32_1` through `REL32_5` are one whose last one to five bytes come after it. That
//! is why [`crate::Reloc`] carries the count as well as the addend it is already inside.
//!
//! What is not here is anything about the addend, because the writer underneath does that part: it
//! reads the type, works out the same one to five, adds it to the addend it was handed and writes
//! the sum into the bytes, since a COFF relocation has no field to keep an addend in.
//!
//! # What this format has no answer for
//!
//! Three things, and each is refused by name rather than written as something close. A visibility is
//! the one that is not refused: `hidden` and `protected` are facts about a dynamic symbol table and
//! a COFF symbol has nowhere to put either, so a file built with `-fvisibility=hidden` for Windows
//! is a file where that flag changed nothing, which is what gcc does there too.

use object::RelocationFlags;
use object::pe;
use object::write::Object as Writer;
use rucc_target::aarch64::Fixup;

use crate::section::Reference;

/// Which relocation of this machine one reference is, given how many bytes of the instruction come
/// after the four the linker writes over.
///
/// The first two are the distance to something and are the same relocation here, because a call to
/// a name in another image is answered by an import stub the linker makes whether or not the
/// relocation asked for one, which is the difference ELF spends a second type on. The last two are
/// the address itself at the two widths this machine writes one at.
///
/// Nothing for the two table slots. A global offset table is not how this platform reaches a symbol
/// it does not define, and a thread-local variable is reached through its offset in `.tls` instead,
/// so a file wanting either is a file this cannot write and says so.
///
/// The last is the one relocation here that ELF has nothing to match: four bytes holding how far
/// something is from the front of the image, which is what every field of an unwind table is.
pub(crate) fn typ(reference: Reference, after: u8) -> Option<pe::RelocationType> {
    Some(match reference {
        Reference::Call | Reference::Data if after <= 5 => {
            pe::RelocationType(pe::IMAGE_REL_AMD64_REL32.0 + u16::from(after))
        }
        Reference::Address { bytes: 8 } => pe::IMAGE_REL_AMD64_ADDR64,
        Reference::Address { bytes: 4 } => pe::IMAGE_REL_AMD64_ADDR32,
        Reference::Image => pe::IMAGE_REL_AMD64_ADDR32NB,
        Reference::Section => pe::IMAGE_REL_AMD64_SECREL,
        // Nothing for a distance written into an image. The relocation this format has for four
        // bytes of distance counts from the byte after them, which is the answer an instruction
        // wants and is four more than the answer an image wants, and there is no addend field to
        // put the difference in because this format keeps the addend in the bytes themselves.
        Reference::Call | Reference::Data | Reference::Got | Reference::Thread => return None,
        Reference::GotBare | Reference::GotKept | Reference::Field(_) => return None,
        Reference::Away => return None,
        Reference::Address { .. } => return None,
    })
}

/// The same, as the writer underneath wants it.
pub(crate) fn reloc(reference: Reference, after: u8) -> Option<RelocationFlags> {
    typ(reference, after).map(|typ| RelocationFlags::Coff { typ })
}

/// Which relocation of an AArch64 file one reference is.
///
/// A field of an instruction is the same field ELF names, less the table slots, which are not how
/// this platform reaches anything, and the offsets from the thread pointer, which this platform
/// reaches through the offset from the start of `.tls` instead. The literal load has nothing either:
/// the format has no nineteen bit distance to data, only to code, and gas and clang never ask for
/// one. The six loads and stores are one relocation here, since the linker reads how far to shift
/// the low bits from the instruction rather than from the type.
///
/// The distance written as data is `REL32`, which counts from the byte after the four it fills,
/// and the writer underneath adds the four back to the addend the way it does for the x86 types,
/// so the linker's answer is the distance from the hole that ELF gives.
pub(crate) fn arm64(reference: Reference) -> Option<pe::RelocationType> {
    Some(match reference {
        Reference::Field(Fixup::Call26 | Fixup::Jump26) => pe::IMAGE_REL_ARM64_BRANCH26,
        Reference::Field(Fixup::CondBr19) => pe::IMAGE_REL_ARM64_BRANCH19,
        Reference::Field(Fixup::TestBr14) => pe::IMAGE_REL_ARM64_BRANCH14,
        Reference::Field(Fixup::AdrLo21) => pe::IMAGE_REL_ARM64_REL21,
        Reference::Field(Fixup::AdrPage21) => pe::IMAGE_REL_ARM64_PAGEBASE_REL21,
        Reference::Field(Fixup::AddLo12) => pe::IMAGE_REL_ARM64_PAGEOFFSET_12A,
        Reference::Field(
            Fixup::Ldst8Lo12
            | Fixup::Ldst16Lo12
            | Fixup::Ldst32Lo12
            | Fixup::Ldst64Lo12
            | Fixup::Ldst128Lo12,
        ) => pe::IMAGE_REL_ARM64_PAGEOFFSET_12L,
        Reference::Field(Fixup::SecrelHigh12A) => pe::IMAGE_REL_ARM64_SECREL_HIGH12A,
        Reference::Field(Fixup::SecrelLow12A) => pe::IMAGE_REL_ARM64_SECREL_LOW12A,
        Reference::Field(Fixup::SecrelLow12L) => pe::IMAGE_REL_ARM64_SECREL_LOW12L,
        Reference::Field(
            Fixup::Literal19
            | Fixup::GotPage21
            | Fixup::GotLo12
            | Fixup::GotTprelPage21
            | Fixup::GotTprelLo12Nc
            | Fixup::TprelHi12
            | Fixup::TprelLo12Nc,
        ) => return None,
        Reference::Address { bytes: 8 } => pe::IMAGE_REL_ARM64_ADDR64,
        Reference::Address { bytes: 4 } => pe::IMAGE_REL_ARM64_ADDR32,
        Reference::Image => pe::IMAGE_REL_ARM64_ADDR32NB,
        Reference::Section => pe::IMAGE_REL_ARM64_SECREL,
        Reference::Data | Reference::Away => pe::IMAGE_REL_ARM64_REL32,
        Reference::Call | Reference::Got | Reference::GotBare | Reference::GotKept => return None,
        Reference::Thread | Reference::Address { .. } => return None,
    })
}

/// An instruction with the addend of its relocation written into the field the linker fills, or
/// why it cannot be.
///
/// A COFF relocation has no addend, so the number added to the name goes where the linker will
/// find it, which for data is the bytes being relocated and for an instruction is the field. The
/// writer underneath does the first and refuses the second, so this is the second. What each field
/// holds is what lld and link.exe read back out of it. The page `adrp` names is counted from the
/// name plus the whole addend, so its field is the addend in bytes, all twenty one bits of it. The
/// low twelve bits are added to the low twelve bits of the name, which gives the low twelve bits of
/// the sum whatever the addend is, carried or not, since what is carried out of them is the page's
/// business. A load or store keeps its twelve bits shifted by the size of the access, so the addend
/// has to be a multiple of that size, which it is for any field of a variable the access is of.
///
/// A branch is refused. Its field is combined with the distance in a way that has changed between
/// linkers, and a branch to a name plus a number is not something a compiler writes. The two halves
/// of an offset into `.tls` are refused too, because the high half is worked out from the name
/// alone and a carry out of the low half, which the addend can cause, would be lost between them.
pub(crate) fn carry(fixup: Fixup, word: u32, addend: i64) -> Result<u32, String> {
    if addend == 0 {
        return Ok(word);
    }
    let low = u32::try_from(addend & 0xfff).expect("twelve bits");
    let refused = || format!("{} cannot carry {addend} added to its name", fixup.name());
    match fixup {
        Fixup::AdrPage21 | Fixup::AdrLo21 => {
            let bits = u32::try_from(addend + (1 << 20)).ok().filter(|&bits| bits < 1 << 21);
            let bits = bits.ok_or_else(refused)? ^ (1 << 20);
            Ok(word & !(3 << 29 | 0x7ffff << 5) | (bits & 3) << 29 | (bits >> 2) << 5)
        }
        Fixup::AddLo12 => Ok(word & !(0xfff << 10) | low << 10),
        Fixup::Ldst8Lo12
        | Fixup::Ldst16Lo12
        | Fixup::Ldst32Lo12
        | Fixup::Ldst64Lo12
        | Fixup::Ldst128Lo12 => {
            let scale = match fixup {
                Fixup::Ldst8Lo12 => 0,
                Fixup::Ldst16Lo12 => 1,
                Fixup::Ldst32Lo12 => 2,
                Fixup::Ldst64Lo12 => 3,
                _ => 4,
            };
            if low & ((1 << scale) - 1) != 0 {
                return Err(refused());
            }
            Ok(word & !(0xfff << 10) | (low >> scale) << 10)
        }
        _ => Err(refused()),
    }
}

/// Nothing, which is what this format has for a variable the loader writes into before anything
/// reads it and the linker was asked to keep apart from the rest.
///
/// `.data.rel.ro` and the `.local` half of it are an ELF answer to a problem this format solves
/// elsewhere. A Windows image has its fixups applied before the pages are given the protection the
/// section headers asked for, so a pointer that needs one lives in ordinary read only data and is
/// written to anyway, which is where the linker and every other compiler on the platform put it.
pub(crate) const REL_RO_LOCAL: Option<&str> = None;

/// What the unwind table is called here, and what it is aligned to.
///
/// One fixed row per function, which the linker sorts by address so that the runtime can find the
/// row for a return address by binary search. Four, because a row is three four byte fields.
///
/// The name is the platform's and the linker matches on it, so it is not a choice: the directory
/// entry telling the runtime where the table is is built out of whatever landed in `.pdata`.
pub(crate) const FUNCTIONS: (&str, u64) = (".pdata", 4);

/// What the second half of the unwind table is called, and what it is aligned to.
///
/// What the rows point at, which is the description of each prologue. A second section rather than
/// more fields, because a row is a fixed size and a description is as long as the prologue it is
/// about.
pub(crate) const CODES: (&str, u64) = (".xdata", 4);

/// Nothing, which is what this format says about the stack being executable.
///
/// A PE image says whether its stack may be run from in the header of the image rather than in a
/// note in every input, so there is no marker for an object to carry and no linker looking for one.
pub(crate) fn marker(_obj: &mut Writer<'_>) {}
