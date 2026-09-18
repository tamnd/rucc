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
/// it does not define, and a thread-local variable is reached through a different mechanism again,
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
        Reference::Call | Reference::Data | Reference::Got | Reference::Thread => return None,
        Reference::Address { .. } => return None,
    })
}

/// The same, as the writer underneath wants it.
pub(crate) fn reloc(reference: Reference, after: u8) -> Option<RelocationFlags> {
    typ(reference, after).map(|typ| RelocationFlags::Coff { typ })
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
