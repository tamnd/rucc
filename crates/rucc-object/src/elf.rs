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

use crate::section::{Array, Binding, Property, Reference, Visibility};

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
/// address, because a thread-local variable has a copy per thread and no address at all. Last is
/// the address itself, at the two widths this machine writes one at.
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
        Reference::Address { bytes: 8 } => elf::R_X86_64_64,
        Reference::Address { bytes: 4 } => elf::R_X86_64_32,
        Reference::Address { .. } | Reference::Image => return None,
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
    let word = |bytes: &[u8], at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let short = |bytes: &[u8], at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
    let long = |bytes: &[u8], at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    // Where the section headers are, how far apart they are and how many of them there are. A
    // file with more than there is room to say puts the count in the first header instead, which
    // this never writes: it would take sixty five thousand sections, and a section here is a
    // function.
    let headers = word(bytes, 0x28) as usize;
    let step = short(bytes, 0x3a) as usize;
    let count = short(bytes, 0x3c) as usize;
    let strings = word(bytes, headers + short(bytes, 0x3e) as usize * step + 24) as usize;
    let name = |bytes: &[u8], header: usize| {
        let at = strings + long(bytes, header) as usize;
        let end = bytes[at..].iter().position(|byte| *byte == 0).map_or(at, |len| at + len);
        String::from_utf8_lossy(&bytes[at..end]).into_owned()
    };
    let names: Vec<String> = (0..count).map(|i| name(bytes, headers + i * step)).collect();
    let mut wanted = ordered.iter();
    for (i, section) in names.iter().enumerate() {
        if section != PATCHABLE {
            continue;
        }
        let Some(target) = wanted.next() else { break };
        let Some(at) = names.iter().position(|name| name == target) else { continue };
        let at = u32::try_from(at).expect("a file with this many sections in it");
        let sh_link = headers + i * step + 40;
        bytes[sh_link..sh_link + 4].copy_from_slice(&at.to_le_bytes());
    }
    debug_assert!(wanted.next().is_none(), "a record whose header nothing found");
}

/// The note that says what the file was built to have checked.
///
/// A note is a name, a description and a number saying what kind it is, and this kind is the one
/// whose description is a list of properties. Each property is a key, a length and that many bytes,
/// and the one written here is the feature word.
///
/// Everything is padded to eight rather than to four, which is what a note in a sixty four bit
/// object is aligned to and what makes the reader's walk over the list a walk over aligned words.
/// The two lengths in the header count the padding after what they measure, which is why the
/// description is sixteen bytes for a property of twelve.
pub(crate) fn record(property: Property) -> Vec<u8> {
    // How long the name is, how long the description is, and which kind of note this is. Then the
    // name, and then the description, which is the one property and the four bytes that pad it.
    let head = [4, 16, elf::NT_GNU_PROPERTY_TYPE_0.0];
    let desc = [Property::X86_FEATURES, 4, property.features, 0];
    let mut out = Vec::with_capacity(32);
    for word in head {
        out.extend_from_slice(&word.to_le_bytes());
    }
    // Twelve bytes in and already a multiple of eight, so the description begins straight after the
    // name with no padding between them.
    out.extend_from_slice(b"GNU\0");
    for word in desc {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}
