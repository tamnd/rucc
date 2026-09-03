//! Relocatable ELF objects.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3, which says the three formats are written
//! through the [`object`] crate's writer with our own layer above it for the parts it does not
//! model. This is that layer for ELF, and what it holds is the part `object` cannot decide: which
//! relocation an instruction wants, what a symbol's binding and type are, and the sections a
//! linker expects to find whether or not anything was put in them.
//!
//! # The marker that has to be there
//!
//! `.note.GNU-stack`. A linker that does not find it in every input marks the stack executable,
//! which section 11.3 calls out as a real and recurring security bug rather than a missing
//! nicety. It is an empty section and nothing reads its contents, and leaving it out is the kind
//! of mistake that produces a working program with a weakness in it, so it is written here and a
//! test says so.
//!
//! # What is not here
//!
//! Mach-O and COFF. The formats disagree about more than their headers: an Apple symbol carries
//! an underscore in front of the C name, Mach-O has no way to say how long a function is and
//! wants `.subsections_via_symbols` instead, and COFF wants storage classes and `.pdata`. Each is
//! its own piece of work and each is written when the target that needs it is.
//!
//! Sections other than the text and the two the writer makes on its own. Data, read-only data and
//! the zero filled section arrive with the global variables that go in them.

use object::write::{Object, Relocation, StandardSection, Symbol, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, RelocationFlags, SectionKind, SymbolFlags, SymbolKind,
    SymbolScope, elf,
};
use rucc_target::{Arch, Os, TargetInfo};

use crate::section::{Reference, Text};

/// What a function is aligned to, which is what the assembler already padded to.
const ALIGN: u64 = 16;

/// Why an object file could not be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A machine or a platform this does not write objects for.
    Format {
        /// The triple that was asked for.
        triple: String,
    },
    /// The writer refused something it was given, which is a bug here rather than in a program.
    Refused {
        /// What it said, already formatted.
        why: String,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Format { triple } => {
                write!(f, "there is no object writer for {triple} in this compiler yet")
            }
            Error::Refused { why } => {
                write!(f, "the object writer refused what it was given: {why}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// One text section as a relocatable ELF object.
///
/// # Errors
///
/// [`Error::Format`] for a machine or a platform this does not write, and [`Error::Refused`] for
/// anything the writer underneath objected to, which would be a bug here. See [`Error`].
pub fn write(text: &Text, target: &TargetInfo) -> Result<Vec<u8>, Error> {
    if target.triple.arch != Arch::X86_64 || target.triple.os == Os::Darwin {
        return Err(Error::Format { triple: target.triple.to_string() });
    }
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let section = obj.section_id(StandardSection::Text);
    obj.append_section_data(section, &text.bytes, ALIGN);

    // Every function defined here, then every name it wanted that is not. A name is looked up
    // rather than added twice, because two symbols with one name is not a file a linker accepts.
    let mut symbols = std::collections::BTreeMap::new();
    for func in &text.funcs {
        let id = obj.add_symbol(Symbol {
            name: func.name.clone().into_bytes(),
            value: func.start as u64,
            size: func.len as u64,
            kind: SymbolKind::Text,
            // Every function is written global, because a machine function does not carry the
            // linkage the C had and nothing below the driver could ask. It is wrong for a static
            // function and it is the same thing the assembly path does, so the two go on agreeing
            // and both stop being wrong on the day the machine IR has somewhere to keep linkage.
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Section(section),
            flags: SymbolFlags::None,
        });
        symbols.insert(func.name.clone(), id);
    }
    for reloc in &text.relocs {
        if symbols.contains_key(&reloc.symbol) {
            continue;
        }
        let id = obj.add_symbol(Symbol {
            name: reloc.symbol.clone().into_bytes(),
            value: 0,
            size: 0,
            // What kind of thing an undefined name is is not known here and does not have to be:
            // a linker resolves an undefined symbol by its name, and the type of one that is not
            // defined anywhere in this file is nothing this file can say.
            kind: SymbolKind::Unknown,
            scope: SymbolScope::Dynamic,
            weak: false,
            section: SymbolSection::Undefined,
            flags: SymbolFlags::None,
        });
        symbols.insert(reloc.symbol.clone(), id);
    }

    for reloc in &text.relocs {
        let symbol = symbols[&reloc.symbol];
        obj.add_relocation(
            section,
            Relocation {
                offset: reloc.at as u64,
                symbol,
                addend: reloc.addend,
                flags: RelocationFlags::Elf { r_type: r_type(reloc.kind) },
            },
        )
        .map_err(|why| Error::Refused { why: why.to_string() })?;
    }

    // Written as an empty note rather than left out, because a linker that does not find it in
    // every input marks the stack executable.
    obj.add_section(Vec::new(), b".note.GNU-stack".to_vec(), SectionKind::Metadata);

    obj.write().map_err(|why| Error::Refused { why: why.to_string() })
}

/// Which relocation of this machine one reference is.
///
/// Both are the distance from the end of an instruction to something, and they differ in what the
/// linker is allowed to do about it. A call may go through a stub, which is what lets a call reach
/// a symbol further away than four bytes can say and what makes a call to a shared library work at
/// all. A load may not, because there is nowhere to put a stub that a load would read.
fn r_type(reference: Reference) -> elf::RelocationType {
    match reference {
        Reference::Call => elf::R_X86_64_PLT32,
        Reference::Data => elf::R_X86_64_PC32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use object::read::{Object as _, ObjectSection as _, ObjectSymbol as _};
    use rucc_target::{Env, Triple};

    use crate::section::{Extent, Reloc};

    /// A linux x86-64 target, which is the only one this writes.
    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// A call to something outside the file, which is the shape every case here starts from.
    fn calling(name: &str) -> Text {
        Text {
            bytes: vec![0xe8, 0, 0, 0, 0, 0xc3],
            funcs: vec![Extent { name: "f".to_owned(), start: 0, len: 6 }],
            relocs: vec![Reloc {
                at: 1,
                symbol: name.to_owned(),
                kind: Reference::Call,
                addend: -4,
            }],
        }
    }

    #[test]
    fn the_bytes_come_back_out_of_the_section_they_went_into() {
        let text = calling("puts");
        let bytes = write(&text, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".text").expect("a text section");
        assert_eq!(section.data().expect("the bytes"), &text.bytes[..]);
    }

    #[test]
    fn a_function_is_a_symbol_that_says_where_it_is_and_how_long_it_is() {
        let mut text = calling("puts");
        text.funcs.push(Extent { name: "g".to_owned(), start: 16, len: 1 });
        text.bytes.resize(17, 0x90);
        let bytes = write(&text, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let g = file.symbols().find(|s| s.name() == Ok("g")).expect("the second function");
        assert_eq!(g.address(), 16);
        assert_eq!(g.size(), 1);
        assert_eq!(g.kind(), SymbolKind::Text);
        assert!(g.is_global(), "a function is global until the machine IR can say otherwise");
    }

    #[test]
    fn a_name_this_file_does_not_define_is_left_for_the_linker_to_find() {
        let bytes = write(&calling("puts"), &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let puts = file.symbols().find(|s| s.name() == Ok("puts")).expect("the callee");
        assert!(puts.is_undefined(), "the file does not define it and must not claim to");
    }

    #[test]
    fn a_call_asks_for_the_relocation_a_stub_may_answer_and_a_load_asks_for_the_one_that_may_not() {
        for (reference, wanted) in
            [(Reference::Call, elf::R_X86_64_PLT32), (Reference::Data, elf::R_X86_64_PC32)]
        {
            let mut text = calling("puts");
            text.relocs[0].kind = reference;
            let bytes = write(&text, &target()).expect("an object");
            let file = object::File::parse(&bytes[..]).expect("a readable object");
            let section = file.section_by_name(".text").expect("a text section");
            let (offset, reloc) = section.relocations().next().expect("one relocation");
            assert_eq!(offset, 1);
            assert_eq!(reloc.addend(), -4);
            assert_eq!(reloc.flags(), RelocationFlags::Elf { r_type: wanted });
        }
    }

    #[test]
    fn a_name_wanted_twice_is_one_symbol_rather_than_two() {
        let mut text = calling("puts");
        text.relocs.push(Reloc {
            at: 1,
            symbol: "puts".to_owned(),
            kind: Reference::Call,
            addend: -4,
        });
        let bytes = write(&text, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert_eq!(file.symbols().filter(|s| s.name() == Ok("puts")).count(), 1);
    }

    #[test]
    fn a_function_that_is_also_called_is_not_a_second_symbol() {
        let text = calling("f");
        let bytes = write(&text, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let mut found = file.symbols().filter(|s| s.name() == Ok("f"));
        let f = found.next().expect("the function");
        assert!(!f.is_undefined(), "the file defines it");
        assert!(found.next().is_none(), "and defines it once");
    }

    #[test]
    fn the_marker_that_says_the_stack_is_not_executable_is_written() {
        let bytes = write(&calling("puts"), &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let note = file.section_by_name(".note.GNU-stack").expect("the marker");
        assert!(note.data().expect("no bytes").is_empty());
    }

    #[test]
    fn a_platform_this_does_not_write_is_said_so_rather_than_written_as_elf() {
        let text = calling("puts");
        for triple in [
            Triple::new(Arch::Aarch64, Os::Linux, Env::Gnu),
            Triple::new(Arch::X86_64, Os::Darwin, Env::Gnu),
        ] {
            let error = write(&text, &TargetInfo::new(triple)).expect_err("no writer");
            assert!(matches!(error, Error::Format { .. }), "{error:?}");
        }
    }
}
