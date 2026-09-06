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
//! Thread-local storage. Reaching a thread-local variable is a different instruction sequence per
//! model and the back end writes none of them, so a module carrying one is refused before it
//! reaches here rather than written as an ordinary variable in the wrong section.

use object::write::{Object as Writer, Relocation, StandardSection, Symbol, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, RelocationFlags, SectionKind, SymbolFlags, SymbolKind,
    SymbolScope, elf,
};
use rucc_target::{Arch, Os, TargetInfo};

use crate::section::{Alias, Binding, Data, Object, Place, Reference, Reloc, Text};

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

/// One text section and the variables beside it, as a relocatable ELF object.
///
/// # Errors
///
/// [`Error::Format`] for a machine or a platform this does not write, and [`Error::Refused`] for
/// anything the writer underneath objected to, which would be a bug here. An alias whose target
/// this file does not define is refused the same way, since the front end is what reports that as
/// a program's mistake and one reaching here means it did not. See [`Error`].
pub fn write(
    text: &Text,
    data: &Data,
    aliases: &[Alias],
    target: &TargetInfo,
) -> Result<Vec<u8>, Error> {
    if target.triple.arch != Arch::X86_64 || target.triple.os == Os::Darwin {
        return Err(Error::Format { triple: target.triple.to_string() });
    }
    let mut obj = Writer::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let section = obj.section_id(StandardSection::Text);
    obj.append_section_data(section, &text.bytes, u64::from(text.align));

    // Every function defined here, then every variable, then every name either of them wanted that
    // is not. A name is looked up rather than added twice, because two symbols with one name is
    // not a file a linker accepts.
    let mut symbols = std::collections::BTreeMap::new();
    for func in &text.funcs {
        let id = obj.add_symbol(Symbol {
            name: func.name.clone().into_bytes(),
            value: func.start as u64,
            size: func.len as u64,
            kind: SymbolKind::Text,
            scope: scope_of(func.binding),
            weak: func.binding == Binding::Weak,
            section: SymbolSection::Section(section),
            flags: SymbolFlags::None,
        });
        symbols.insert(func.name.clone(), id);
    }

    // Where each variable's image landed in the section it went into, kept because a relocation in
    // an image counts from the start of the image and one in a file counts from the start of the
    // section. A variable that is not in a section has no entry, since nothing in a merged one can
    // hold a relocation: the linker is being asked for zeroed space rather than for an image.
    let mut placed = Vec::with_capacity(data.objects.len());
    for object in &data.objects {
        let (section, offset) = put(&mut obj, object);
        let id = obj.add_symbol(Symbol {
            name: object.name.clone().into_bytes(),
            // A common symbol says what it wants rather than where it is, and what it wants is
            // recorded where an ordinary symbol records its address.
            value: if object.place == Place::Merged { object.align } else { offset },
            size: object.size,
            kind: SymbolKind::Data,
            scope: scope_of(object.binding),
            weak: object.binding == Binding::Weak,
            section,
            flags: SymbolFlags::None,
        });
        symbols.insert(object.name.clone(), id);
        placed.push((section.id(), offset));
    }

    // A second name for something already added, which is where the alias's own binding is the
    // only thing it does not take from what it points at: the target of one may be a `static` and
    // the alias of it may not be. Before the loop below rather than after it, because a reference
    // to the new name is a reference to something this file defines and would otherwise be added
    // as a name this file wants from somewhere else.
    for alias in aliases {
        let Some(&id) = symbols.get(&alias.target) else {
            let why =
                format!("'{}' is aliased to '{}', which is not here", alias.name, alias.target);
            return Err(Error::Refused { why });
        };
        let (value, size) = (obj.symbol(id).value, obj.symbol(id).size);
        let (kind, section) = (obj.symbol(id).kind, obj.symbol(id).section);
        let id = obj.add_symbol(Symbol {
            name: alias.name.clone().into_bytes(),
            value,
            size,
            kind,
            scope: scope_of(alias.binding),
            weak: alias.binding == Binding::Weak,
            section,
            flags: SymbolFlags::None,
        });
        symbols.insert(alias.name.clone(), id);
    }

    let wanted = text.relocs.iter().chain(data.objects.iter().flat_map(|object| &object.relocs));
    for reloc in wanted {
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
        add(&mut obj, section, 0, reloc, &symbols)?;
    }
    for (object, &(section, offset)) in data.objects.iter().zip(&placed) {
        let Some(section) = section else { continue };
        for reloc in &object.relocs {
            add(&mut obj, section, offset, reloc, &symbols)?;
        }
    }

    // Written as an empty note rather than left out, because a linker that does not find it in
    // every input marks the stack executable.
    obj.add_section(Vec::new(), b".note.GNU-stack".to_vec(), SectionKind::Metadata);

    obj.write().map_err(|why| Error::Refused { why: why.to_string() })
}

/// One variable's image into the section it belongs in, and where in that section it landed.
///
/// A zero filled variable takes as many bytes of the file as it is long on the way in and none on
/// the way out, which is the whole point of the section it goes in. A merged one goes in no section
/// at all: the linker is being asked for that much zeroed space under that name, and where it ends
/// up is the linker's answer rather than this file's.
fn put(obj: &mut Writer<'_>, object: &Object) -> (SymbolSection, u64) {
    let section = match &object.place {
        Place::Written => obj.section_id(StandardSection::Data),
        Place::ReadOnly => obj.section_id(StandardSection::ReadOnlyData),
        Place::Zero => obj.section_id(StandardSection::UninitializedData),
        Place::Merged => return (SymbolSection::Common, 0),
        // A named section is the program's word for where this goes, and a program that names one
        // wants what it named rather than what would have been chosen. It is written as ordinary
        // data because nothing in the IR says otherwise.
        Place::Named(name) => {
            obj.add_section(Vec::new(), name.clone().into_bytes(), SectionKind::Data)
        }
    };
    let offset = if object.place == Place::Zero {
        obj.append_section_bss(section, object.size, object.align)
    } else {
        obj.append_section_data(section, &object.bytes, object.align)
    };
    (SymbolSection::Section(section), offset)
}

/// One relocation, at `offset` bytes into the section its image landed at.
fn add(
    obj: &mut Writer<'_>,
    section: object::write::SectionId,
    offset: u64,
    reloc: &Reloc,
    symbols: &std::collections::BTreeMap<String, object::write::SymbolId>,
) -> Result<(), Error> {
    let r_type = r_type(reloc.kind)
        .ok_or_else(|| Error::Refused { why: format!("no relocation is {:?}", reloc.kind) })?;
    obj.add_relocation(
        section,
        Relocation {
            offset: offset + reloc.at as u64,
            symbol: symbols[&reloc.symbol],
            addend: reloc.addend,
            flags: RelocationFlags::Elf { r_type },
        },
    )
    .map_err(|why| Error::Refused { why: why.to_string() })
}

/// How far a name reaches, which is the one thing about a symbol ELF calls its binding.
fn scope_of(binding: Binding) -> SymbolScope {
    match binding {
        Binding::Local => SymbolScope::Compilation,
        // Linkage rather than Dynamic, because whether a name goes in the dynamic symbol table is
        // its visibility and the IR keeps that separately. Nothing sets it to anything but the
        // default yet, and when something does it belongs here rather than folded into this.
        Binding::Global | Binding::Weak => SymbolScope::Linkage,
    }
}

/// Which relocation of this machine one reference is, and nothing for one this machine has none of.
///
/// The first two are the distance from the end of an instruction to something, and they differ in
/// what the linker is allowed to do about it. A call may go through a stub, which is what lets a
/// call reach a symbol further away than four bytes can say and what makes a call to a shared
/// library work at all. A load may not, because there is nowhere to put a stub that a load would
/// read. The third is the address itself, at the two widths this machine writes one at.
fn r_type(reference: Reference) -> Option<elf::RelocationType> {
    Some(match reference {
        Reference::Call => elf::R_X86_64_PLT32,
        Reference::Data => elf::R_X86_64_PC32,
        Reference::Address { bytes: 8 } => elf::R_X86_64_64,
        Reference::Address { bytes: 4 } => elf::R_X86_64_32,
        Reference::Address { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use object::read::elf::Sym as _;
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
            funcs: vec![Extent {
                name: "f".to_owned(),
                start: 0,
                len: 6,
                binding: Binding::Global,
            }],
            relocs: vec![Reloc {
                at: 1,
                symbol: name.to_owned(),
                kind: Reference::Call,
                addend: -4,
            }],
            ..Text::default()
        }
    }

    #[test]
    fn the_bytes_come_back_out_of_the_section_they_went_into() {
        let text = calling("puts");
        let bytes = write(&text, &Data::default(), &[], &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".text").expect("a text section");
        assert_eq!(section.data().expect("the bytes"), &text.bytes[..]);
    }

    #[test]
    fn a_function_is_a_symbol_that_says_where_it_is_and_how_long_it_is() {
        let mut text = calling("puts");
        text.funcs.push(Extent {
            name: "g".to_owned(),
            start: 16,
            len: 1,
            binding: Binding::Global,
        });
        text.bytes.resize(17, 0x90);
        let bytes = write(&text, &Data::default(), &[], &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let g = file.symbols().find(|s| s.name() == Ok("g")).expect("the second function");
        assert_eq!(g.address(), 16);
        assert_eq!(g.size(), 1);
        assert_eq!(g.kind(), SymbolKind::Text);
        assert!(g.is_global(), "nothing said otherwise about this one");
    }

    #[test]
    fn a_function_no_other_file_can_see_is_a_local_symbol() {
        let mut text = calling("puts");
        text.funcs.push(Extent {
            name: "hidden".to_owned(),
            start: 16,
            len: 1,
            binding: Binding::Local,
        });
        text.funcs.push(Extent {
            name: "shared".to_owned(),
            start: 32,
            len: 1,
            binding: Binding::Weak,
        });
        text.bytes.resize(33, 0x90);
        let bytes = write(&text, &Data::default(), &[], &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let hidden = file.symbols().find(|s| s.name() == Ok("hidden")).expect("the static one");
        // A symbol the linker keeps and does not let another file reach, which is the whole of
        // what `static` on a function means and what two files each defining their own need.
        assert!(hidden.is_local(), "a static function must not be offered to the linker");
        assert!(!hidden.is_weak());
        let shared = file.symbols().find(|s| s.name() == Ok("shared")).expect("the weak one");
        assert!(shared.is_weak(), "a weak function has to be able to lose");
        assert!(shared.is_global());
    }

    #[test]
    fn a_name_this_file_does_not_define_is_left_for_the_linker_to_find() {
        let bytes = write(&calling("puts"), &Data::default(), &[], &target()).expect("an object");
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
            let bytes = write(&text, &Data::default(), &[], &target()).expect("an object");
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
        let bytes = write(&text, &Data::default(), &[], &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert_eq!(file.symbols().filter(|s| s.name() == Ok("puts")).count(), 1);
    }

    #[test]
    fn a_function_that_is_also_called_is_not_a_second_symbol() {
        let text = calling("f");
        let bytes = write(&text, &Data::default(), &[], &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let mut found = file.symbols().filter(|s| s.name() == Ok("f"));
        let f = found.next().expect("the function");
        assert!(!f.is_undefined(), "the file defines it");
        assert!(found.next().is_none(), "and defines it once");
    }

    #[test]
    fn the_marker_that_says_the_stack_is_not_executable_is_written() {
        let bytes = write(&calling("puts"), &Data::default(), &[], &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let note = file.section_by_name(".note.GNU-stack").expect("the marker");
        assert!(note.data().expect("no bytes").is_empty());
    }

    /// One variable of four bytes, in whichever section its own answer puts it.
    fn variable(name: &str, place: Place) -> Object {
        Object {
            name: name.to_owned(),
            bytes: if place == Place::Zero { Vec::new() } else { vec![1, 0, 0, 0] },
            size: 4,
            align: 4,
            place,
            binding: Binding::Global,
            relocs: Vec::new(),
        }
    }

    /// A file of that one variable and nothing else.
    fn holding(object: Object) -> Vec<u8> {
        let data = Data { objects: vec![object] };
        write(&Text::default(), &data, &[], &target()).expect("an object")
    }

    #[test]
    fn what_a_variable_is_decides_which_section_it_goes_in() {
        for (place, wanted) in [
            (Place::Written, ".data"),
            (Place::ReadOnly, ".rodata"),
            (Place::Zero, ".bss"),
            (Place::Named(".init_array".to_owned()), ".init_array"),
        ] {
            let bytes = holding(variable("x", place.clone()));
            let file = object::File::parse(&bytes[..]).expect("a readable object");
            let section = file.section_by_name(wanted).unwrap_or_else(|| panic!("{place:?}"));
            assert_eq!(section.size(), 4, "{place:?}");
            // The zero filled one is as long as it says and carries none of it, which is the
            // whole reason the section exists.
            let carried = section.data().expect("the bytes").len();
            assert_eq!(carried, if place == Place::Zero { 0 } else { 4 }, "{place:?}");
        }
    }

    #[test]
    fn a_variable_is_a_symbol_that_says_where_it_is_and_how_long_it_is() {
        let mut data = Data { objects: vec![variable("first", Place::Written)] };
        data.objects.push(Object { align: 16, ..variable("second", Place::Written) });
        let bytes = write(&Text::default(), &data, &[], &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let second = file.symbols().find(|s| s.name() == Ok("second")).expect("the second one");
        assert_eq!(second.kind(), SymbolKind::Data);
        assert_eq!(second.size(), 4);
        // Sixteen rather than four, because the second one asked for sixteen and the first one
        // had already used four. Getting this wrong is a variable at an address it said it would
        // never be at, which nothing downstream would notice until an aligned load faulted.
        assert_eq!(second.address(), 16);
    }

    #[test]
    fn the_linkage_a_variable_had_is_the_binding_the_symbol_gets() {
        for (binding, global, weak) in [
            (Binding::Global, true, false),
            (Binding::Local, false, false),
            (Binding::Weak, true, true),
        ] {
            let bytes = holding(Object { binding, ..variable("x", Place::Written) });
            let file = object::File::parse(&bytes[..]).expect("a readable object");
            let x = file.symbols().find(|s| s.name() == Ok("x")).expect("the variable");
            assert_eq!(x.is_global(), global, "{binding:?}");
            assert_eq!(x.is_weak(), weak, "{binding:?}");
        }
    }

    #[test]
    fn a_tentative_definition_asks_the_linker_for_space_rather_than_naming_any() {
        let bytes = holding(Object { align: 8, ..variable("x", Place::Merged) });
        let file = object::read::elf::ElfFile64::<Endianness>::parse(&bytes[..]).expect("readable");
        let x = file.symbols().find(|s| s.name() == Ok("x")).expect("the variable");
        assert!(x.is_common(), "the linker merges every definition of this name into one");
        assert_eq!(x.size(), 4);
        // What a common symbol records where an ordinary one records its address is what it wants
        // to be aligned to, because it has no address yet. The reader deliberately answers nothing
        // when asked for the address of one, so this is the field itself.
        assert_eq!(x.address(), 0);
        assert_eq!(x.elf_symbol().st_value(Endianness::Little), 8);
    }

    #[test]
    fn an_address_in_an_image_is_the_address_and_not_a_distance_to_it() {
        let object = Object {
            bytes: vec![0; 8],
            size: 8,
            align: 8,
            relocs: vec![Reloc {
                at: 0,
                symbol: "y".to_owned(),
                kind: Reference::Address { bytes: 8 },
                addend: 16,
            }],
            ..variable("p", Place::Written)
        };
        let bytes = holding(object);
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".data").expect("a data section");
        let (offset, reloc) = section.relocations().next().expect("one relocation");
        assert_eq!(offset, 0);
        assert_eq!(reloc.addend(), 16);
        assert_eq!(reloc.flags(), RelocationFlags::Elf { r_type: elf::R_X86_64_64 });
        let y = file.symbols().find(|s| s.name() == Ok("y")).expect("what it points at");
        assert!(y.is_undefined(), "nothing here defines it and the linker is being asked for it");
    }

    /// Not a rewording of the case above: what is checked is the arithmetic between the two.
    #[test]
    fn a_relocation_counts_from_the_start_of_the_section_and_not_of_the_image_it_is_in() {
        let mut data = Data { objects: vec![variable("first", Place::Written)] };
        data.objects.push(Object {
            bytes: vec![0; 16],
            size: 16,
            align: 8,
            relocs: vec![Reloc {
                at: 8,
                symbol: "y".to_owned(),
                kind: Reference::Address { bytes: 8 },
                addend: 0,
            }],
            ..variable("second", Place::Written)
        });
        let bytes = write(&Text::default(), &data, &[], &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".data").expect("a data section");
        let (offset, _) = section.relocations().next().expect("one relocation");
        // Eight into the second image, which starts eight in because the first one is four long
        // and the second is eight aligned.
        assert_eq!(offset, 16);
    }

    #[test]
    fn a_second_name_is_a_second_symbol_at_the_first_one_s_address_and_no_second_image() {
        let data = Data {
            objects: vec![Object { binding: Binding::Local, ..variable("a", Place::Written) }],
        };
        let aliases =
            [Alias { name: "b".to_owned(), target: "a".to_owned(), binding: Binding::Global }];
        let bytes = write(&Text::default(), &data, &aliases, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let a = file.symbols().find(|s| s.name() == Ok("a")).expect("the variable");
        let b = file.symbols().find(|s| s.name() == Ok("b")).expect("the second name");
        assert_eq!(b.address(), a.address(), "the same place");
        assert_eq!(b.size(), a.size());
        assert_eq!(b.section_index(), a.section_index());
        // The binding is the one thing the second name does not take from the first, which is
        // what `extern int b __attribute__((alias("a")))` on a `static a` asks for.
        assert!(a.is_local(), "the target was written `static`");
        assert!(b.is_global(), "and the name given to it was not");
        // Four bytes of image and not eight, since an alias is a name and not a copy.
        assert_eq!(file.section_by_name(".data").expect("a data section").size(), 4);
    }

    #[test]
    fn a_function_can_be_given_a_second_name_the_same_way_a_variable_can() {
        let text = calling("puts");
        let aliases =
            [Alias { name: "g".to_owned(), target: "f".to_owned(), binding: Binding::Weak }];
        let bytes = write(&text, &Data::default(), &aliases, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let f = file.symbols().find(|s| s.name() == Ok("f")).expect("the function");
        let g = file.symbols().find(|s| s.name() == Ok("g")).expect("the second name");
        assert_eq!(g.address(), f.address());
        assert_eq!(g.size(), f.size());
        assert_eq!(g.kind(), f.kind(), "a second name for a function is a function");
        assert!(g.is_weak(), "so that a program may define the name itself instead");
    }

    /// The front end is what reports this as a program's mistake, so one arriving here is a bug
    /// in this compiler and is said so rather than written as an undefined symbol.
    #[test]
    fn a_second_name_for_something_this_file_does_not_define_is_refused() {
        let aliases =
            [Alias { name: "b".to_owned(), target: "a".to_owned(), binding: Binding::Global }];
        let error = write(&Text::default(), &Data::default(), &aliases, &target())
            .expect_err("nothing to point at");
        assert!(matches!(error, Error::Refused { .. }), "{error:?}");
    }

    #[test]
    fn a_platform_this_does_not_write_is_said_so_rather_than_written_as_elf() {
        let text = calling("puts");
        for triple in [
            Triple::new(Arch::Aarch64, Os::Linux, Env::Gnu),
            Triple::new(Arch::X86_64, Os::Darwin, Env::Gnu),
        ] {
            let error = write(&text, &Data::default(), &[], &TargetInfo::new(triple))
                .expect_err("no writer");
            assert!(matches!(error, Error::Format { .. }), "{error:?}");
        }
    }
}
