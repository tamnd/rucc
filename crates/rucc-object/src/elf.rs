//! What ELF answers, where the two formats answer differently.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3 and `spec/cross-compile/07-object-formats.md`
//! section 7.2. How a file is laid out is in [`crate::file`], which is one piece of code for both
//! formats because the layout is the same question twice. This is the other half: the places where
//! the answer is a number or a name that belongs to one format, which is the relocation set, the
//! field a visibility goes in, the note saying what the file was built to have checked, and the
//! marker whose absence makes the stack executable.
//!
//! # The marker that has to be there
//!
//! `.note.GNU-stack`. A linker that does not find it in every input marks the stack executable,
//! which section 11.3 calls out as a real and recurring security bug rather than a missing nicety.
//! It is an empty section and nothing reads its contents, and leaving it out is the kind of mistake
//! that produces a working program with a weakness in it, so it is written here and a test says so.

use object::write::{Object as Writer, SymbolId};
use object::{SectionFlags, SectionKind, SymbolFlags, elf};

use crate::section::{Array, Binding, Property, Reference, Tls, Visibility};

/// Which relocation of this machine one reference is, and nothing for one this machine has none of.
///
/// The first three are the distance from the end of an instruction to something, and they differ in
/// what the linker is allowed to do about it. A call may go through a stub, which is what lets a
/// call reach a symbol further away than four bytes can say and what makes a call to a shared
/// library work at all. A load may not, because there is nowhere to put a stub that a load would
/// read, so a load of something another object may define reads a table slot the linker fills in
/// instead, and the relaxing form of the relocation lets the linker undo that when it turns out
/// nobody else defines it. The linker only knows how to undo it in a few instructions and has to
/// know whether there is a REX prefix, so the load comes in three kinds that say which. After them
/// is a table slot as well, which holds an offset into a thread's own block rather than an
/// address, because a thread-local variable has a copy per thread and no address at all. Then the
/// distance written as data, at four bytes and at eight, and last the address itself, at the four
/// widths gas writes one at. A byte or two of an address is rare and is what `.byte sym` asks for.
///
/// Nothing for how far something is from the front of the image. This format has no relocation of
/// that kind because nothing it writes wants one, and a file asking for one here is a file whose
/// unwind table was built for the other platform.
pub(crate) fn r_type(reference: Reference) -> Option<elf::RelocationType> {
    Some(match reference {
        Reference::Call => elf::R_X86_64_PLT32,
        Reference::Data | Reference::Away => elf::R_X86_64_PC32,
        Reference::Got => elf::R_X86_64_REX_GOTPCRELX,
        Reference::GotBare => elf::R_X86_64_GOTPCRELX,
        Reference::GotKept => elf::R_X86_64_GOTPCREL,
        Reference::Thread => elf::R_X86_64_GOTTPOFF,
        Reference::AwayWide => elf::R_X86_64_PC64,
        Reference::Address { bytes: 8 } => elf::R_X86_64_64,
        Reference::Address { bytes: 4 } => elf::R_X86_64_32,
        Reference::Address { bytes: 2 } => elf::R_X86_64_16,
        Reference::Address { bytes: 1 } => elf::R_X86_64_8,
        Reference::Short => elf::R_X86_64_PC16,
        Reference::Tiny => elf::R_X86_64_PC8,
        Reference::Signed => elf::R_X86_64_32S,
        Reference::Address { .. } | Reference::Image | Reference::Section | Reference::Field(_) => {
            return None;
        }
        // The i386 ways of reaching the global offset table, counted from a register holding it,
        // and its thread-local storage. This machine counts from the instruction pointer instead
        // and has kinds of its own above.
        Reference::GotOffset
        | Reference::GotFront
        | Reference::Slot
        | Reference::SlotKept
        | Reference::Tls(_) => {
            return None;
        }
    })
}

/// The same for i386.
///
/// A call is `R_386_PLT32`, which lets the linker send it through a stub, and a distance from the
/// four bytes to something else is `R_386_PC32`. gas writes the second for a plain `call foo` and
/// the first only for `call foo@PLT`, and a linker treats the two the same way for a function it
/// finds in the program, so the kind the assembler chose says which is meant.
///
/// Position independent code has no instruction pointer to count from on this machine, so it
/// counts from a register it has loaded with the address of the global offset table. The table is
/// found with `R_386_GOTPC`, something this file defines is reached as an offset from the table with
/// `R_386_GOTOFF`, and something another object may define is read out of a slot, with
/// `R_386_GOT32X` where the linker may turn the load back into the address it would have been and
/// `R_386_GOT32` where it may not.
///
/// An address is written in four bytes, or in two or one where the source asked for that. An
/// address an x86-64 instruction would have sign extended is the same four bytes here, since there
/// is nothing wider to extend it into.
///
/// Thread-local storage is reached through `%gs` on this machine with a relocation for each step of
/// each model, which [`Reference::Tls`] names. [`Reference::Thread`], which is the x86-64 table
/// slot, is refused rather than written as whichever of them looks closest. Nothing either for
/// anything eight bytes wide, or for the kinds that belong to another machine or another format.
pub(crate) fn r_type_i386(reference: Reference) -> Option<elf::RelocationType> {
    Some(match reference {
        Reference::Call => elf::R_386_PLT32,
        Reference::Data | Reference::Away => elf::R_386_PC32,
        Reference::GotOffset => elf::R_386_GOTOFF,
        Reference::GotFront => elf::R_386_GOTPC,
        Reference::Slot => elf::R_386_GOT32X,
        Reference::SlotKept => elf::R_386_GOT32,
        Reference::Tls(Tls::General) => elf::R_386_TLS_GD,
        Reference::Tls(Tls::Module) => elf::R_386_TLS_LDM,
        Reference::Tls(Tls::InModule) => elf::R_386_TLS_LDO_32,
        Reference::Tls(Tls::Slot) => elf::R_386_TLS_GOTIE,
        Reference::Tls(Tls::SlotAddress) => elf::R_386_TLS_IE,
        Reference::Tls(Tls::SlotNegated) => elf::R_386_TLS_IE_32,
        Reference::Tls(Tls::Offset) => elf::R_386_TLS_LE,
        Reference::Tls(Tls::Negated) => elf::R_386_TLS_LE_32,
        Reference::Address { bytes: 4 } | Reference::Signed => elf::R_386_32,
        Reference::Address { bytes: 2 } => elf::R_386_16,
        Reference::Address { bytes: 1 } => elf::R_386_8,
        Reference::Short => elf::R_386_PC16,
        Reference::Tiny => elf::R_386_PC8,
        Reference::Got
        | Reference::GotBare
        | Reference::GotKept
        | Reference::Thread
        | Reference::AwayWide
        | Reference::Address { .. }
        | Reference::Image
        | Reference::Section
        | Reference::Field(_) => return None,
    })
}

/// How many bytes of the section an i386 relocation writes over, which is where its addend goes.
///
/// A file for this machine keeps no addend in the relocation itself. What is added to the symbol is
/// whatever the bytes held before the linker got there, so the writer has to put it there, and it
/// needs to know how many bytes are the relocation's to do that. The writer underneath knows for
/// some of the types and not for `R_386_GOT32X`, so the answer is given here for every type
/// [`r_type_i386`] gives.
pub(crate) fn width_i386(r_type: elf::RelocationType) -> Option<usize> {
    match r_type {
        elf::R_386_8 | elf::R_386_PC8 => Some(1),
        elf::R_386_16 | elf::R_386_PC16 => Some(2),
        elf::R_386_32
        | elf::R_386_PC32
        | elf::R_386_PLT32
        | elf::R_386_GOTOFF
        | elf::R_386_GOTPC
        | elf::R_386_GOT32
        | elf::R_386_GOT32X
        | elf::R_386_TLS_GD
        | elf::R_386_TLS_LDM
        | elf::R_386_TLS_LDO_32
        | elf::R_386_TLS_GOTIE
        | elf::R_386_TLS_IE
        | elf::R_386_TLS_IE_32
        | elf::R_386_TLS_LE
        | elf::R_386_TLS_LE_32 => Some(4),
        _ => None,
    }
}

/// The same for AArch64.
///
/// A field of an instruction is the relocation its fixup names. What a table of data holds is the
/// same question it is on the other machine with different numbers: an address at eight bytes or
/// at four, and a distance from the four bytes themselves, which is what an unwind record says
/// about its function. Nothing for the kinds that are about an x86-64 instruction, since an
/// instruction here asks through a field.
pub(crate) fn r_type_aarch64(reference: Reference) -> Option<elf::RelocationType> {
    Some(match reference {
        Reference::Field(fixup) => elf::RelocationType(fixup.elf()?),
        Reference::Address { bytes: 8 } => elf::R_AARCH64_ABS64,
        Reference::Address { bytes: 4 } => elf::R_AARCH64_ABS32,
        Reference::Data | Reference::Away => elf::R_AARCH64_PREL32,
        Reference::AwayWide => elf::R_AARCH64_PREL64,
        _ => return None,
    })
}

/// Say what `st_other` is for a symbol that has just been added, rather than leave it to be
/// inferred from the scope.
///
/// The writer underneath fills `st_info` in from the kind, the binding and whether the symbol is
/// defined, and there is nothing to add to that. `st_other` is the field this compiler has an
/// opinion about and the field the `SymbolScope` mapping got wrong, so it is written here in the
/// two bits ELF puts the visibility in and the rest of the byte is left as it was found.
///
/// A local symbol is left alone. Its visibility means nothing, since a name the static link has
/// already finished with cannot be in a dynamic symbol table whatever `st_other` says, and gcc
/// writes `STV_DEFAULT` for one, which is what the writer underneath produces on its own.
pub(crate) fn see(obj: &mut Writer<'_>, id: SymbolId, binding: Binding, visibility: Visibility) {
    if binding == Binding::Local {
        return;
    }
    let wanted = match visibility {
        Visibility::Default => elf::STV_DEFAULT,
        Visibility::Hidden => elf::STV_HIDDEN,
        Visibility::Protected => elf::STV_PROTECTED,
    };
    if let SymbolFlags::Elf { st_other, .. } = obj.symbol_flags_mut(id) {
        *st_other = st_other.with_visibility(wanted);
    }
}

/// Make a symbol that has just been added an indirect function, `STT_GNU_IFUNC`, keeping the
/// binding the writer underneath worked out for it.
///
/// The writer has no kind of its own for one, so the symbol is added as text and the type field
/// of `st_info` is written over here. The binding is the other half of the same byte and is left
/// as it was found, which is local for a `static` resolver's name and global or weak otherwise.
///
/// The type is a GNU extension, and a file holding one says so in its header: gas writes
/// `ELFOSABI_GNU` there rather than `ELFOSABI_NONE` once a symbol of the type is in it, and
/// `readelf` only names the type `IFUNC` in a file that does. The flags beside it are kept.
pub(crate) fn indirect(obj: &mut Writer<'_>, id: SymbolId) {
    if let SymbolFlags::Elf { st_info, .. } = obj.symbol_flags_mut(id) {
        *st_info = st_info.st_bind() | elf::STT_GNU_IFUNC;
    }
    let (abi_version, e_flags) = match obj.flags {
        object::FileFlags::Elf { abi_version, e_flags, .. } => (abi_version, e_flags),
        _ => (0, elf::FileFlags(0)),
    };
    obj.flags = object::FileFlags::Elf { os_abi: elf::ELFOSABI_GNU, abi_version, e_flags };
}

/// The type and flags a section of function addresses the startup code calls has.
///
/// A section of the ordinary type under one of those names is gathered by the linker in the same
/// run and called by nobody, so the type is written out rather than left to the default.
pub(crate) fn gathered(array: Array) -> SectionFlags {
    SectionFlags::Elf {
        sh_type: match array {
            Array::Init => elf::SHT_INIT_ARRAY,
            Array::Fini => elf::SHT_FINI_ARRAY,
            Array::Preinit => elf::SHT_PREINIT_ARRAY,
        },
        sh_flags: elf::SHF_ALLOC | elf::SHF_WRITE,
    }
}

/// The flags a record of where a patcher's room is has, which say it belongs to a text section.
///
/// `SHF_LINK_ORDER` is what ties the two together, and which section is `sh_link`, which the writer
/// underneath does not set and [`link`] fills in afterwards.
pub(crate) fn ordered() -> SectionFlags {
    SectionFlags::Elf {
        sh_type: elf::SHT_PROGBITS,
        sh_flags: elf::SHF_ALLOC | elf::SHF_WRITE | elf::SHF_LINK_ORDER,
    }
}

/// What a record of where a patcher's room is is called.
pub(crate) const PATCHABLE: &str = "__patchable_function_entries";

/// What the unwind table is called here, and what it is aligned to.
///
/// One section rather than two: a record carries the codes for its own function, so there is nothing
/// for a second section to hold. Eight, because a record is looked up by address at a point where
/// the program is usually already crashing, and an unaligned read there is a second fault on top of
/// the first.
pub(crate) const FRAMES: (&str, u64) = (".eh_frame", 8);

/// The section a variable the loader writes into before anything reads it goes in, when the program
/// asked for the half of it the linker keeps apart from the rest.
///
/// It is a layout hint rather than anything the loader reads: the two halves have the same flags,
/// and putting the ones whose fixups never leave the image together is what lets the linker give
/// their pages back their protection in one go. The writer underneath has a name for the section
/// and none for this half of it, so it is added by hand.
pub(crate) const REL_RO_LOCAL: Option<&str> = Some(".data.rel.ro.local");

/// The empty section whose absence makes the stack executable.
pub(crate) fn marker(obj: &mut Writer<'_>) {
    obj.add_section(Vec::new(), b".note.GNU-stack".to_vec(), SectionKind::Metadata);
}

/// Ties each record of where a patcher's room is to the text it is a record of.
///
/// `SHF_LINK_ORDER` says a section belongs to another one, and which one is `sh_link`, a section
/// index. The writer underneath has no way to say it: it writes a zero into every ordinary
/// section's `sh_link` and offers nothing that would change one. A zero there is not harmless,
/// since a linker reads a section that claims to be ordered after nothing as an error, so the
/// number is written into the finished bytes here.
///
/// Ordinary sections come out in the order they were added, so the records are found by name in
/// header order and paired with the text sections they were added beside, in the same order.
/// `ordered` is that list, and the target is looked up by name because a text section's name is
/// unique in a file even though a record's is not.
///
/// A file with no records is left alone, which is nearly every file.
pub(crate) fn link(bytes: &mut [u8], ordered: &[String]) {
    if ordered.is_empty() {
        return;
    }
    let headers = Headers::read(bytes);
    let mut wanted = ordered.iter();
    for header in &headers.list {
        if header.name != PATCHABLE {
            continue;
        }
        let Some(target) = wanted.next() else { break };
        let Some(at) = headers.list.iter().position(|other| other.name == *target) else {
            continue;
        };
        let at = u32::try_from(at).expect("a file with this many sections in it");
        let sh_link = header.at + headers.link();
        bytes[sh_link..sh_link + 4].copy_from_slice(&at.to_le_bytes());
    }
    debug_assert!(wanted.next().is_none(), "a record whose header nothing found");
}

/// The section headers of a finished little endian file, read back so that a field the writer
/// underneath has no way to set can be written into the bytes afterwards.
///
/// Both classes, because the fields are in different places in each: a 32 bit file has four byte
/// addresses and sizes where a 64 bit one has eight, so every field after the first two moves. The
/// class is the fifth byte of the file, and the rest is where that says it is.
pub(crate) struct Headers {
    /// Whether the file is a 64 bit one.
    wide: bool,
    /// Every header, in the order they are in the file.
    pub(crate) list: Vec<Header>,
}

/// One section header, as much of it as anything here reads.
pub(crate) struct Header {
    /// Where the header starts in the file.
    pub(crate) at: usize,
    /// What the section is called.
    pub(crate) name: String,
    /// `sh_type`.
    pub(crate) sh_type: u32,
    /// `sh_offset`, which is where the section's contents start in the file.
    pub(crate) offset: usize,
}

impl Headers {
    /// Every section header of `bytes`.
    ///
    /// A file with more sections than there is room to say puts the count in the first header
    /// instead, which this never writes: it would take sixty five thousand sections, and a section
    /// here is a function.
    pub(crate) fn read(bytes: &[u8]) -> Headers {
        let wide = bytes[4] == elf::ELFCLASS64.0;
        let word = |at: usize, width: usize| {
            bytes[at..at + width].iter().rev().fold(0u64, |sum, &byte| sum << 8 | u64::from(byte))
                as usize
        };
        // Where the headers are, how far apart, how many, and which one names the rest.
        let (table, each, count, names) = if wide {
            (word(0x28, 8), 0x3a, 0x3c, 0x3e)
        } else {
            (word(0x20, 4), 0x2e, 0x30, 0x32)
        };
        let (each, count, names) = (word(each, 2), word(count, 2), word(names, 2));
        let offset = |header: usize| if wide { word(header + 24, 8) } else { word(header + 16, 4) };
        let strings = offset(table + names * each);
        let list = (0..count)
            .map(|nth| {
                let at = table + nth * each;
                let from = strings + word(at, 4);
                let end =
                    bytes[from..].iter().position(|byte| *byte == 0).map_or(from, |len| from + len);
                Header {
                    at,
                    name: String::from_utf8_lossy(&bytes[from..end]).into_owned(),
                    sh_type: word(at + 4, 4) as u32,
                    offset: offset(at),
                }
            })
            .collect();
        Headers { wide, list }
    }

    /// Where `sh_link` is in a header.
    pub(crate) fn link(&self) -> usize {
        if self.wide { 40 } else { 24 }
    }

    /// Where `sh_entsize` is in a header, and how wide it is.
    pub(crate) fn entry_size(&self) -> (usize, usize) {
        if self.wide { (56, 8) } else { (36, 4) }
    }
}

/// The notes that say what the file was built to have checked and what it needs of the link.
///
/// A note is a name, a description and a number saying what kind it is, and this kind is the one
/// whose description is a list of properties. Each property is a key, a length and that many bytes,
/// and each written here is one word. One note each, which is what gcc writes, and what the linker
/// reads the same as one note holding both.
///
/// Everything is padded to the width of an address, which is eight in a 64 bit object and four in
/// a 32 bit one, and is what makes the reader's walk over the list a walk over aligned words. The
/// two lengths in the header count the padding after what they measure, which is why the
/// description is sixteen bytes for a property of twelve in a 64 bit file and twelve in a 32 bit
/// one, where there is nothing to pad.
pub(crate) fn record(property: Property, align: u32) -> Vec<u8> {
    let size = 12u32.next_multiple_of(align);
    let mut out = Vec::new();
    for (key, word) in property.each() {
        // How long the name is, how long the description is, and which kind of note this is.
        // Then the name, and then the description, which is the one property and whatever pads it.
        let start = out.len();
        let head = [4, size, elf::NT_GNU_PROPERTY_TYPE_0.0];
        for word in head {
            out.extend_from_slice(&word.to_le_bytes());
        }
        // Sixteen bytes in once the name is there, which is a multiple of either width, so the
        // description begins straight after the name with no padding between them.
        out.extend_from_slice(b"GNU\0");
        for word in [key, 4, word] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.resize(start + 16 + size as usize, 0);
    }
    out
}
