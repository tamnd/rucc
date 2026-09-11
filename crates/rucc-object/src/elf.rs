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

use object::write::{
    Object as Writer, Relocation, StandardSection, Symbol, SymbolId, SymbolSection,
};
use object::{
    Architecture, BinaryFormat, Endianness, RelocationFlags, SectionFlags, SectionKind,
    SymbolFlags, SymbolKind, SymbolScope, elf,
};
use rucc_target::{ObjectFormat, TargetInfo};
use rucc_tuple::Arch;

use crate::section::{
    Alias, Binding, Data, Object, Output, Place, Property, Reference, Reloc, Sections, Text,
    Visibility,
};

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
    output: Output,
) -> Result<Vec<u8>, Error> {
    let Output { sections, property } = output;
    if target.tuple.arch() != Arch::X86_64 || target.object_format != ObjectFormat::Elf {
        return Err(Error::Format { triple: target.tuple.to_string() });
    }
    let mut obj = Writer::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    // The one that holds every function when they are not being split up. Asked for even when it
    // will stay empty, because it is the section the writer underneath starts a file with anyway
    // and gcc writes an empty `.text` under `-ffunction-sections` too.
    let whole = obj.section_id(StandardSection::Text);
    if !sections.functions {
        obj.append_section_data(whole, &text.bytes, u64::from(text.align));
    }

    // Every function defined here, then every variable, then every name either of them wanted that
    // is not. A name is looked up rather than added twice, because two symbols with one name is
    // not a file a linker accepts.
    let mut symbols = std::collections::BTreeMap::new();
    // Where each function ended up, in the order they were written, so that a relocation inside
    // one goes into the section that one is in and one that points at the start of one can be
    // written against that section. The same list as `text.funcs` and in the same order, so the
    // two are walked together below.
    let mut split: Vec<(object::write::SectionId, u64)> = Vec::with_capacity(text.funcs.len());
    // Which text section each record of where a patcher's room is belongs to, in the order the
    // records were added, which is the order their headers come out in. See `link`.
    let mut ordered: Vec<String> = Vec::new();
    for func in &text.funcs {
        // A section of its own, holding this function's bytes and nothing else, so the linker can
        // drop it when nothing reaches it. The name is what gcc writes, and the leading `.text.`
        // is not decoration: `--gc-sections` and the linker scripts that place code both match on
        // it, and a section called something else would be placed by the catch all rule.
        //
        // The room a patcher was promised in front of the label goes in it too. Those bytes are
        // the function's, they are just not under its name: the symbol is where the label was and
        // the room is what came before, so a section holding one without the other would be a
        // section a linker could place with the room missing.
        let ahead = func.patch.map_or(0, |patch| patch.before);
        let (section, at) = if sections.functions {
            let name = format!(".text.{}", func.name).into_bytes();
            let id = obj.add_section(Vec::new(), name, SectionKind::Text);
            let bytes = &text.bytes[func.start - ahead..func.start + func.len];
            obj.append_section_data(id, bytes, u64::from(func.align.max(1)));
            (id, ahead as u64)
        } else {
            (whole, func.start as u64)
        };
        // Where the room is, in a section of its own that says nothing else. What reads it is a
        // tracer patching every function in an image at once, and what it needs is every address
        // in one place: a stripped kernel has no symbol table to walk instead, which is the whole
        // reason the list is written rather than worked out later.
        //
        // The address is a relocation rather than a number, because a function is at a fixed
        // offset in its own section and where that section lands is the linker's answer. It is
        // written against the section rather than against the function's own name so that it still
        // points at the room when the room is in front of the name.
        //
        // One section per function even when they all point at the same text, which is what gas
        // produces and what lets a linker throw the record away with the function. `SHF_LINK_ORDER`
        // is what ties the two together and it needs a section index the writer underneath does not
        // set, so `link` fills it in afterwards. See `link`.
        if let Some(patch) = func.patch {
            let base = if sections.functions { func.start - ahead } else { 0 };
            let name = PATCHABLE.as_bytes().to_vec();
            let id = obj.add_section(Vec::new(), name, SectionKind::Data);
            obj.section_mut(id).flags = SectionFlags::Elf {
                sh_type: elf::SHT_PROGBITS,
                sh_flags: elf::SHF_ALLOC | elf::SHF_WRITE | elf::SHF_LINK_ORDER,
            };
            obj.append_section_data(id, &[0; 8], 8);
            let symbol = obj.section_symbol(section);
            obj.add_relocation(
                id,
                Relocation {
                    offset: 0,
                    symbol,
                    addend: (patch.at - base) as i64,
                    flags: RelocationFlags::Elf { r_type: elf::R_X86_64_64 },
                },
            )
            .map_err(|why| Error::Refused { why: why.to_string() })?;
            ordered.push(if sections.functions {
                format!(".text.{}", func.name)
            } else {
                ".text".to_owned()
            });
        }
        let id = obj.add_symbol(Symbol {
            name: func.name.clone().into_bytes(),
            value: at,
            size: func.len as u64,
            kind: SymbolKind::Text,
            scope: scope_of(func.binding),
            weak: func.binding == Binding::Weak,
            section: SymbolSection::Section(section),
            flags: SymbolFlags::None,
        });
        see(&mut obj, id, func.binding, func.visibility);
        symbols.insert(func.name.clone(), id);
        split.push((section, at));
    }

    // Where each variable's image landed in the section it went into, kept because a relocation in
    // an image counts from the start of the image and one in a file counts from the start of the
    // section. A variable that is not in a section has no entry, since nothing in a merged one can
    // hold a relocation: the linker is being asked for zeroed space rather than for an image.
    let mut placed = Vec::with_capacity(data.objects.len());
    // The one section the writer has no name of its own for, remembered so that every variable that
    // wants it lands in the same one. The rest come back from `section_id`, which already answers
    // with the section it made the first time it was asked.
    let mut local = None;
    for object in &data.objects {
        let (section, offset) = put(&mut obj, object, &mut local, sections);
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
        see(&mut obj, id, object.binding, object.visibility);
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
        see(&mut obj, id, alias.binding, alias.visibility);
        symbols.insert(alias.name.clone(), id);
    }

    // Not the unwind table's, which name functions this file defines and are written against the
    // section rather than against the name. A record for anything else is refused below, so a name
    // added here for one would be a name nothing goes on to use.
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
        // Which function's bytes this one is in, which is the question only the split path has to
        // ask: when there is one text section every offset in it is already the offset in it.
        // Every relocation is inside some function, since the padding between two of them is
        // instructions that do nothing and holds nothing a linker fills in.
        let (section, at) = if sections.functions {
            let after = text.funcs.partition_point(|func| func.start <= reloc.at);
            let Some(func) = after.checked_sub(1).map(|i| &text.funcs[i]) else {
                let why = format!("a relocation at {} is in front of every function", reloc.at);
                return Err(Error::Refused { why });
            };
            // From the start of the section rather than from the symbol, and the two are not the
            // same byte in a function with room in front of its label.
            let base = func.start - func.patch.map_or(0, |patch| patch.before);
            (split[after - 1].0, (reloc.at - base) as u64)
        } else {
            (whole, reloc.at as u64)
        };
        add(&mut obj, section, at, reloc, &symbols)?;
    }

    // The unwind table, if there is one. Its own section rather than part of the text, because it
    // is read rather than run: the loader maps it and the linker gathers every input's into one
    // table and builds the index the unwinder binary searches. Eight, because a record is looked
    // up by address at a point where the program is usually already crashing and an unaligned read
    // there is a second fault on top of the first.
    if !text.unwind.bytes.is_empty() {
        let frames = obj.add_section(Vec::new(), b".eh_frame".to_vec(), SectionKind::ReadOnlyData);
        obj.append_section_data(frames, &text.unwind.bytes, 8);
        for reloc in &text.unwind.relocs {
            // Against the section the function is in rather than against the function's own name,
            // which is the same reason the record of a patcher's room is written that way and one
            // more besides. The section is the only one of the two that is settled here: a global
            // name is answered at load time by whichever object defines it first, so a distance
            // measured to one is not a distance the linker can work out, and it says so and stops.
            // The effect was that nothing this compiler wrote could go into a shared library at
            // all, because every function has a record and every record pointed at a name.
            //
            // A function defined elsewhere has no record here, so the lookup failing means the
            // record is for something that is not a function in this file, and that is a bug
            // rather than a shape to handle: the writer says what it was given rather than
            // guessing.
            let found = text.funcs.iter().position(|func| func.name == reloc.symbol);
            let Some((section, at)) = found.map(|i| split[i]) else {
                let why =
                    format!("'{}' has an unwind record and is not a function here", reloc.symbol);
                return Err(Error::Refused { why });
            };
            let symbol = obj.section_symbol(section);
            let r_type = r_type(reloc.kind).ok_or_else(|| Error::Refused {
                why: format!("no relocation is {:?}", reloc.kind),
            })?;
            let record = Relocation {
                offset: reloc.at as u64,
                symbol,
                // Where the function starts inside its section, since the section symbol is where
                // the section starts and the two are the same byte only for the first function in
                // one.
                addend: reloc.addend + at as i64,
                flags: RelocationFlags::Elf { r_type },
            };
            obj.add_relocation(frames, record)
                .map_err(|why| Error::Refused { why: why.to_string() })?;
        }
    }
    for (object, &(section, offset)) in data.objects.iter().zip(&placed) {
        let Some(section) = section else { continue };
        for reloc in &object.relocs {
            add(&mut obj, section, offset + reloc.at as u64, reloc, &symbols)?;
        }
    }

    // What the file was built to have checked, when it was built to have anything checked. Left
    // out otherwise rather than written as a zero, because a linker treats a missing note and a
    // note with no bits in it the same way and gcc writes nothing.
    if property.any() {
        let note = obj.section_id(StandardSection::GnuProperty);
        obj.append_section_data(note, &record(property), 8);
    }

    // Written as an empty note rather than left out, because a linker that does not find it in
    // every input marks the stack executable.
    obj.add_section(Vec::new(), b".note.GNU-stack".to_vec(), SectionKind::Metadata);

    let mut bytes = obj.write().map_err(|why| Error::Refused { why: why.to_string() })?;
    link(&mut bytes, &ordered);
    Ok(bytes)
}

/// What a record of where a patcher's room is is called.
const PATCHABLE: &str = "__patchable_function_entries";

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
fn link(bytes: &mut [u8], ordered: &[String]) {
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
fn record(property: Property) -> Vec<u8> {
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

/// One variable's image into the section it belongs in, and where in that section it landed.
///
/// A zero filled variable takes as many bytes of the file as it is long on the way in and none on
/// the way out, which is the whole point of the section it goes in. A merged one goes in no section
/// at all: the linker is being asked for that much zeroed space under that name, and where it ends
/// up is the linker's answer rather than this file's.
fn put(
    obj: &mut Writer<'_>,
    object: &Object,
    local: &mut Option<object::write::SectionId>,
    sections: Sections,
) -> (SymbolSection, u64) {
    // A section of its own, named after the variable and after the section it would have gone in,
    // which is what `-fdata-sections` asks for. A merged variable has no section to split and a
    // named one was named by the program, so both are left where they are: the first is a request
    // to the linker rather than an image, and the second would otherwise have the flag silently
    // overrule what the source said.
    if sections.data {
        if let Some(name) = object.place.split(&object.name) {
            let section = obj.add_section(Vec::new(), name.into_bytes(), kind_of(&object.place));
            let offset = if object.place == Place::Zero {
                obj.append_section_bss(section, object.size, object.align)
            } else {
                obj.append_section_data(section, &object.bytes, object.align)
            };
            return (SymbolSection::Section(section), offset);
        }
    }
    let section = match &object.place {
        Place::Written => obj.section_id(StandardSection::Data),
        Place::ReadOnly => obj.section_id(StandardSection::ReadOnlyData),
        // Read only after the loader has written it, which the writer knows as the relocatable
        // read only data section and which is `.data.rel.ro` on ELF. The `.local` half is a layout
        // hint the writer has no name for, so it is added by hand and remembered: asking again
        // would make a second section with the same name, and a file with one of those per variable
        // is a file whose section headers outweigh what they describe.
        Place::RelocReadOnly { local: false } => {
            obj.section_id(StandardSection::ReadOnlyDataWithRel)
        }
        Place::RelocReadOnly { local: true } => *local.get_or_insert_with(|| {
            obj.add_section(
                Vec::new(),
                b".data.rel.ro.local".to_vec(),
                SectionKind::ReadOnlyDataWithRel,
            )
        }),
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

/// What a section split off for one variable is, which is what the section it was split off from
/// was.
///
/// Splitting changes the name and nothing else. A variable that was going to be in a page the
/// loader maps read only is still in one, and a zero filled variable still costs the file nothing,
/// so the flags a linker reads off the section header have to come out the same as they would
/// have. The two kinds with no section of their own never reach here, and `Data` for them is a
/// value that is never used rather than a claim about either.
fn kind_of(place: &Place) -> SectionKind {
    match place {
        Place::ReadOnly => SectionKind::ReadOnlyData,
        Place::RelocReadOnly { .. } => SectionKind::ReadOnlyDataWithRel,
        Place::Zero => SectionKind::UninitializedData,
        Place::Written | Place::Merged | Place::Named(_) => SectionKind::Data,
    }
}

/// One relocation, `at` bytes into the section it ended up in.
///
/// The offset is worked out by the caller rather than here, because the two callers count from
/// different places: a relocation in an image counts from the start of that image and a relocation
/// in a function counts from the start of that function, and neither of those is where the section
/// begins once something else is in front of it.
fn add(
    obj: &mut Writer<'_>,
    section: object::write::SectionId,
    at: u64,
    reloc: &Reloc,
    symbols: &std::collections::BTreeMap<String, SymbolId>,
) -> Result<(), Error> {
    let r_type = r_type(reloc.kind)
        .ok_or_else(|| Error::Refused { why: format!("no relocation is {:?}", reloc.kind) })?;
    obj.add_relocation(
        section,
        Relocation {
            offset: at,
            symbol: symbols[&reloc.symbol],
            addend: reloc.addend,
            flags: RelocationFlags::Elf { r_type },
        },
    )
    .map_err(|why| Error::Refused { why: why.to_string() })
}

/// How far a name reaches, which is the one thing about a symbol ELF calls its binding.
///
/// `SymbolScope` is two facts in one word, and the trap is that the middle one is not the neutral
/// answer it reads as. The writer turns `Compilation` into a local symbol, and it turns the choice
/// between `Linkage` and `Dynamic` into `st_other`: `Linkage` is `STV_HIDDEN` and `Dynamic` is
/// `STV_DEFAULT`. So there is no way to say global and decline to say anything about visibility,
/// and picking the one whose name sounds like the smaller claim is picking hidden. That is what
/// tamnd/rucc#733 was.
///
/// `Dynamic` is what every global asks for here, and the visibility is said afterwards by
/// [`see`] rather than through this, so that nothing about `st_other` depends on reading one of
/// these four names the way its author meant it.
fn scope_of(binding: Binding) -> SymbolScope {
    match binding {
        Binding::Local => SymbolScope::Compilation,
        Binding::Global | Binding::Weak => SymbolScope::Dynamic,
    }
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
fn see(obj: &mut Writer<'_>, id: SymbolId, binding: Binding, visibility: Visibility) {
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

/// Which relocation of this machine one reference is, and nothing for one this machine has none of.
///
/// The first three are the distance from the end of an instruction to something, and they differ in
/// what the linker is allowed to do about it. A call may go through a stub, which is what lets a
/// call reach a symbol further away than four bytes can say and what makes a call to a shared
/// library work at all. A load may not, because there is nowhere to put a stub that a load would
/// read, so a load of something another object may define reads a table slot the linker fills in
/// instead, and the relaxing form of the relocation lets the linker undo that when it turns out
/// nobody else defines it. The fourth is the address itself, at the two widths this machine writes
/// one at.
fn r_type(reference: Reference) -> Option<elf::RelocationType> {
    Some(match reference {
        Reference::Call => elf::R_X86_64_PLT32,
        Reference::Data => elf::R_X86_64_PC32,
        Reference::Got => elf::R_X86_64_REX_GOTPCRELX,
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
    use rucc_target::{Arch, Env, Os, Triple};

    use crate::section::{Extent, Patch, Reloc};

    /// A linux x86-64 target, which is the only one this writes.
    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// One function of that name, at that offset, that many bytes long, and visible that far.
    ///
    /// Visibility is the field these cases mostly have no opinion about, so it is the one the
    /// helper fills in and the two that do have an opinion write for themselves.
    fn extent(name: String, start: usize, len: usize, binding: Binding) -> Extent {
        Extent {
            name,
            start,
            len,
            align: crate::FUNC_ALIGN,
            binding,
            visibility: Visibility::Default,
            patch: None,
        }
    }

    /// A call to something outside the file, which is the shape every case here starts from.
    fn calling(name: &str) -> Text {
        Text {
            bytes: vec![0xe8, 0, 0, 0, 0, 0xc3],
            funcs: vec![extent("f".to_owned(), 0, 6, Binding::Global)],
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
        let bytes =
            write(&text, &Data::default(), &[], &target(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".text").expect("a text section");
        assert_eq!(section.data().expect("the bytes"), &text.bytes[..]);
    }

    #[test]
    fn a_function_is_a_symbol_that_says_where_it_is_and_how_long_it_is() {
        let mut text = calling("puts");
        text.funcs.push(extent("g".to_owned(), 16, 1, Binding::Global));
        text.bytes.resize(17, 0x90);
        let bytes =
            write(&text, &Data::default(), &[], &target(), Output::default()).expect("an object");
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
        text.funcs.push(extent("hidden".to_owned(), 16, 1, Binding::Local));
        text.funcs.push(extent("shared".to_owned(), 32, 1, Binding::Weak));
        text.bytes.resize(33, 0x90);
        let bytes =
            write(&text, &Data::default(), &[], &target(), Output::default()).expect("an object");
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

    /// A global is `STV_DEFAULT`, so a shared library built from these objects exports something.
    ///
    /// The bug in tamnd/rucc#733. Every global came out `STV_HIDDEN`, which a static link does not
    /// look at, so nothing here noticed and SQLite linked and ran and the whole test suite passed.
    /// What it costs is the dynamic symbol table: `gcc -shared` over one of these objects produced
    /// a library with an empty one, and `dlsym` could not find a function the file plainly defines.
    ///
    /// Written against `st_other` itself rather than against the reader's `scope`, because `scope`
    /// is the word that was misread in the first place and a test that asks it the same question
    /// would agree with whatever the writer did.
    /// The record of where a patcher's room is, and what it says about it.
    ///
    /// Four things have to be right at once for a linker to take it: the flags, the alignment, the
    /// relocation and the section it says it is ordered after. The last of those is the one the
    /// writer underneath cannot say, so a zero there would be a file `ld` refuses and a test that
    /// only looked at the bytes would not see it.
    #[test]
    fn where_a_patcher_may_write_is_recorded_in_a_section_tied_to_the_code_it_is_about() {
        let mut text = calling("puts");
        text.bytes.splice(0..0, [0x90, 0x90, 0x90]);
        text.funcs[0].start = 3;
        text.funcs[0].patch = Some(Patch { at: 0, before: 3 });
        text.relocs[0].at = 4;
        let bytes =
            write(&text, &Data::default(), &[], &target(), Output::default()).expect("an object");
        let file = object::read::elf::ElfFile64::<Endianness>::parse(&bytes[..]).expect("readable");
        let section = file.section_by_name(PATCHABLE).expect("a record of the room");
        assert_eq!(section.size(), 8, "one address, and this file defines one function");
        assert_eq!(section.align(), 8);
        let header = section.elf_section_header();
        assert_eq!(
            header.sh_flags.get(Endianness::Little),
            elf::SHF_ALLOC | elf::SHF_WRITE | elf::SHF_LINK_ORDER
        );
        // Which is the whole point of the fixup: the index has to be the text section's own, and
        // the writer underneath had written a zero there.
        let index = file.section_by_name(".text").expect("a text section").index().0;
        assert_eq!(header.sh_link.get(Endianness::Little) as usize, index);
        assert_ne!(index, 0);

        // And the address, which is the front of the room rather than the function's own symbol.
        let [(at, reloc)] = &section.relocations().collect::<Vec<_>>()[..] else {
            panic!("one address in the record")
        };
        assert_eq!(*at, 0);
        assert_eq!(reloc.addend(), 0);
        assert_eq!(reloc.flags(), RelocationFlags::Elf { r_type: elf::R_X86_64_64 });
    }

    /// And a file that asked for none has no such section, which is nearly every file.
    #[test]
    fn a_file_that_promised_a_patcher_nothing_records_nothing() {
        let text = calling("puts");
        let bytes =
            write(&text, &Data::default(), &[], &target(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert!(file.section_by_name(PATCHABLE).is_none());
    }

    /// The same when each function is a section of its own, which is what a kernel builds with.
    ///
    /// Each record then points at a different section, which is what makes the pairing worth
    /// asserting: getting it backwards would still produce a file every tool reads and every
    /// address in it would be about the wrong function.
    #[test]
    fn each_record_is_tied_to_its_own_function_when_they_are_split_up() {
        let mut text = calling("puts");
        text.funcs[0].patch = Some(Patch { at: 0, before: 0 });
        text.funcs.push(extent("g".to_owned(), 16, 1, Binding::Global));
        text.funcs[1].patch = Some(Patch { at: 16, before: 0 });
        text.bytes.resize(17, 0x90);
        let output =
            Output { sections: Sections { functions: true, data: false }, ..Output::default() };
        let bytes = write(&text, &Data::default(), &[], &target(), output).expect("an object");
        let file = object::read::elf::ElfFile64::<Endianness>::parse(&bytes[..]).expect("readable");
        let links: Vec<usize> = file
            .sections()
            .filter(|section| section.name() == Ok(PATCHABLE))
            .map(|section| section.elf_section_header().sh_link.get(Endianness::Little) as usize)
            .collect();
        let index = |name: &str| file.section_by_name(name).expect("a text section").index().0;
        assert_eq!(links, [index(".text.f"), index(".text.g")]);
    }

    #[test]
    fn a_global_is_visible_to_the_dynamic_linker_and_a_static_one_is_not_a_symbol_at_all() {
        let mut text = calling("puts");
        text.funcs.push(extent("g".to_owned(), 16, 1, Binding::Global));
        text.funcs.push(extent("w".to_owned(), 32, 1, Binding::Weak));
        text.funcs.push(extent("s".to_owned(), 48, 1, Binding::Local));
        text.bytes.resize(49, 0x90);
        let bytes =
            write(&text, &Data::default(), &[], &target(), Output::default()).expect("an object");
        let file = object::read::elf::ElfFile64::<Endianness>::parse(&bytes[..]).expect("readable");
        let visibility = |name: &str| {
            file.symbols()
                .find(|s| s.name() == Ok(name))
                .expect("the function")
                .elf_symbol()
                .st_visibility()
        };
        // Nothing said hidden about either of these, so neither is.
        assert_eq!(visibility("g"), elf::STV_DEFAULT);
        assert_eq!(visibility("w"), elf::STV_DEFAULT, "a weak one is still a name others may use");
        // The `static` one is local, and a local symbol's visibility means nothing either way,
        // which is why the binding is what this asks about.
        assert_eq!(visibility("s"), elf::STV_DEFAULT);
    }

    /// And the other direction: a name that did ask to be hidden is hidden, and a protected one is
    /// protected.
    ///
    /// The half of tamnd/rucc#733 that the fix above left open. Saying `STV_DEFAULT` for everything
    /// is right for everything nobody marked and wrong the moment something is marked, so the two
    /// tests together are what says the field carries an answer rather than a constant.
    ///
    /// Both are asked of a function and of a variable, because they are added by two different
    /// loops in `write` and a field one of them fills in is not a field the other one does.
    #[test]
    fn a_name_that_asked_to_be_hidden_is_hidden_and_a_protected_one_is_protected() {
        let mut text = calling("puts");
        for (index, (name, seen)) in
            [("h", Visibility::Hidden), ("p", Visibility::Protected)].into_iter().enumerate()
        {
            let mut func = extent(name.to_owned(), 16 + index * 16, 1, Binding::Global);
            func.visibility = seen;
            text.funcs.push(func);
        }
        text.bytes.resize(49, 0x90);
        let mut data = Data::default();
        for (name, seen) in [("vh", Visibility::Hidden), ("vp", Visibility::Protected)] {
            let mut object = variable(name, Place::Written);
            object.visibility = seen;
            data.objects.push(object);
        }
        let bytes = write(&text, &data, &[], &target(), Output::default()).expect("an object");
        let file = object::read::elf::ElfFile64::<Endianness>::parse(&bytes[..]).expect("readable");
        let visibility = |name: &str| {
            file.symbols()
                .find(|s| s.name() == Ok(name))
                .expect("the symbol")
                .elf_symbol()
                .st_visibility()
        };
        assert_eq!(visibility("h"), elf::STV_HIDDEN);
        assert_eq!(visibility("p"), elf::STV_PROTECTED);
        assert_eq!(visibility("vh"), elf::STV_HIDDEN, "a variable goes through a second loop");
        assert_eq!(visibility("vp"), elf::STV_PROTECTED);
        // The one thing a visibility must not disturb, since `st_info` and `st_other` are written
        // in one go and the second was set after the first.
        let h = file.symbols().find(|s| s.name() == Ok("h")).expect("the function");
        assert!(h.is_global(), "hidden is about the dynamic linker and not about the binding");
        assert_eq!(h.size(), 1, "and it is still a function of the length it was");
    }

    #[test]
    fn a_name_this_file_does_not_define_is_left_for_the_linker_to_find() {
        let bytes = write(&calling("puts"), &Data::default(), &[], &target(), Output::default())
            .expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let puts = file.symbols().find(|s| s.name() == Ok("puts")).expect("the callee");
        assert!(puts.is_undefined(), "the file does not define it and must not claim to");
    }

    #[test]
    fn a_call_asks_for_the_relocation_a_stub_may_answer_and_a_load_asks_for_the_one_that_may_not() {
        for (reference, wanted) in [
            (Reference::Call, elf::R_X86_64_PLT32),
            (Reference::Data, elf::R_X86_64_PC32),
            (Reference::Got, elf::R_X86_64_REX_GOTPCRELX),
        ] {
            let mut text = calling("puts");
            text.relocs[0].kind = reference;
            let bytes = write(&text, &Data::default(), &[], &target(), Output::default())
                .expect("an object");
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
        let bytes =
            write(&text, &Data::default(), &[], &target(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert_eq!(file.symbols().filter(|s| s.name() == Ok("puts")).count(), 1);
    }

    #[test]
    fn a_function_that_is_also_called_is_not_a_second_symbol() {
        let text = calling("f");
        let bytes =
            write(&text, &Data::default(), &[], &target(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let mut found = file.symbols().filter(|s| s.name() == Ok("f"));
        let f = found.next().expect("the function");
        assert!(!f.is_undefined(), "the file defines it");
        assert!(found.next().is_none(), "and defines it once");
    }

    #[test]
    fn the_marker_that_says_the_stack_is_not_executable_is_written() {
        let bytes = write(&calling("puts"), &Data::default(), &[], &target(), Output::default())
            .expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let note = file.section_by_name(".note.GNU-stack").expect("the marker");
        assert!(note.data().expect("no bytes").is_empty());
    }

    /// What the file says it was built to have checked, byte for byte.
    ///
    /// Written against the bytes rather than against a reader, because the two lengths in the
    /// header count the padding after what they measure and a note whose lengths are one word out
    /// is one a linker drops without saying anything. What comes of that is a program the loader
    /// leaves the check turned off for, which is a build that looks like it worked.
    #[test]
    fn the_note_that_says_what_the_file_was_built_to_have_checked_is_written() {
        let property = Property { features: Property::IBT | Property::SHSTK };
        let output = Output { property, ..Output::default() };
        let bytes =
            write(&calling("puts"), &Data::default(), &[], &target(), output).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let note = file.section_by_name(".note.gnu.property").expect("the note");
        assert_eq!(note.align(), 8, "a note in a sixty four bit object is read a word at a time");
        let want: Vec<u8> = [
            4u32,
            16,
            5,
            u32::from_le_bytes(*b"GNU\0"),
            Property::X86_FEATURES,
            4,
            Property::IBT | Property::SHSTK,
            0,
        ]
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect();
        assert_eq!(note.data().expect("the bytes"), &want[..]);
    }

    /// And nothing at all when the file was built to have nothing checked.
    ///
    /// A note with an empty feature word and no note are the same thing to a linker, which drops
    /// the whole property when any input lacks it. gcc writes nothing, so a section header that
    /// describes nothing would be the one difference between the two compilers' objects.
    #[test]
    fn a_file_built_to_have_nothing_checked_says_nothing() {
        let bytes = write(&calling("puts"), &Data::default(), &[], &target(), Output::default())
            .expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert!(file.section_by_name(".note.gnu.property").is_none());
    }

    /// Every unwind record names the function it is about, and each name goes where it is in the
    /// table rather than at the start of it.
    ///
    /// Written because working the offset out is the caller's job here, which is what the two text
    /// paths differ about, and a third caller that let it default to nothing would put every record
    /// in the table on the same function. Nothing else would notice: the section is the right
    /// length, the symbols are right, the link succeeds, and what comes of it is an unwinder that
    /// walks out of the wrong frame the first time something throws or a backtrace is taken.
    #[test]
    fn an_unwind_record_names_the_function_it_is_about_and_not_the_first_one() {
        let mut text = calling("puts");
        text.funcs.push(extent("g".to_owned(), 16, 1, Binding::Global));
        text.bytes.resize(17, 0x90);
        // A shared header and two records, whose contents nothing here reads: what is being asked
        // is where in them each name landed.
        text.unwind.bytes = vec![0; 64];
        for (at, name) in [(32usize, "f"), (48usize, "g")] {
            text.unwind.relocs.push(Reloc {
                at,
                symbol: name.to_owned(),
                kind: Reference::Address { bytes: 8 },
                addend: 0,
            });
        }
        let bytes =
            write(&text, &Data::default(), &[], &target(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let mut found = points_at(&file);
        found.sort_unstable();
        assert_eq!(found, [(32, ".text".to_owned(), 0), (48, ".text".to_owned(), 16)]);
    }

    /// What each record in the unwind table points at: where it is, the section it reaches, and
    /// how far into that section the function it is about begins.
    fn points_at(file: &object::File<'_>) -> Vec<(u64, String, i64)> {
        let frames = file.section_by_name(".eh_frame").expect("the table");
        frames
            .relocations()
            .map(|(offset, reloc)| {
                let object::RelocationTarget::Symbol(index) = reloc.target() else {
                    panic!("a record points at something that is not a symbol");
                };
                let symbol = file.symbol_by_index(index).expect("a symbol that is in the table");
                assert_eq!(symbol.kind(), SymbolKind::Section, "a record names a section");
                let section = symbol.section_index().expect("a section symbol is in one");
                let name = file.section_by_index(section).expect("a readable section");
                (offset, name.name().expect("a named section").to_owned(), reloc.addend())
            })
            .collect()
    }

    /// A record points at the section its function is in rather than at the function's name.
    ///
    /// Written for tamnd/rucc#1004, which was that nothing this compiler wrote could go into a
    /// shared library. A global name is answered at load time by whichever object defines it
    /// first, so the distance from a record to one of them is not a distance a static linker can
    /// work out, and `ld` says so and stops with advice to recompile with the flag that was
    /// already on the command line. A section is settled by then, which is why gcc measures to a
    /// local label and why this measures to the section.
    ///
    /// Both ways of splitting the text, because the offset is the part that differs: one section
    /// holding everything makes it the function's place in the whole text, and a section per
    /// function makes it whatever room a patcher was promised in front of the label.
    #[test]
    fn a_record_reaches_its_function_through_the_section_it_is_in() {
        let mut text = two();
        text.unwind.bytes = vec![0; 64];
        for (at, name) in [(32usize, "f"), (48usize, "g")] {
            text.unwind.relocs.push(Reloc {
                at,
                symbol: name.to_owned(),
                kind: Reference::Data,
                addend: 0,
            });
        }
        let bytes =
            write(&text, &Data::default(), &[], &target(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let mut whole = points_at(&file);
        whole.sort_unstable();
        assert_eq!(whole, [(32, ".text".to_owned(), 0), (48, ".text".to_owned(), 16)]);

        let sections =
            Output { sections: Sections { functions: true, data: false }, ..Output::default() };
        let bytes = write(&text, &Data::default(), &[], &target(), sections).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let mut split = points_at(&file);
        split.sort_unstable();
        assert_eq!(split, [(32, ".text.f".to_owned(), 0), (48, ".text.g".to_owned(), 0)]);
    }

    /// A record about a name this file does not define is refused rather than written.
    ///
    /// There is no such file today: the table is built beside the text out of the functions that
    /// were just compiled. It is refused rather than left to the linker because the alternative is
    /// the shape that was just fixed, a record measured to a name, and the writer saying what it
    /// was given is how that stays fixed.
    #[test]
    fn a_record_about_something_this_file_does_not_define_is_refused() {
        let mut text = calling("puts");
        text.unwind.bytes = vec![0; 64];
        text.unwind.relocs.push(Reloc {
            at: 32,
            symbol: "puts".to_owned(),
            kind: Reference::Data,
            addend: 0,
        });
        let why = write(&text, &Data::default(), &[], &target(), Output::default())
            .expect_err("a record about a name from somewhere else");
        assert!(why.to_string().contains("puts"), "{why}");
    }

    /// The name of the section that symbol is defined in.
    fn lives_in<'a>(file: &'a object::File<'a>, name: &str) -> String {
        let symbol = file.symbols().find(|s| s.name() == Ok(name)).expect("the symbol");
        let index = symbol.section_index().expect("a section to be defined in");
        let section = file.section_by_index(index).expect("a readable section");
        section.name().expect("a named section").to_owned()
    }

    /// Two functions, the second of them sixteen bytes in and calling something outside the file.
    fn two() -> Text {
        let mut text = calling("puts");
        // Padded to where the second one is aligned to, with the instruction that does nothing,
        // because the space in front of a function is reached by falling off the end of one.
        text.bytes.resize(16, 0x90);
        text.bytes.extend_from_slice(&[0xe8, 0, 0, 0, 0, 0xc3]);
        text.funcs.push(extent("g".to_owned(), 16, 6, Binding::Global));
        text.relocs.push(Reloc {
            at: 17,
            symbol: "puts".to_owned(),
            kind: Reference::Call,
            addend: -4,
        });
        text
    }

    /// What `-ffunction-sections` comes down to in an object file, which is the flag that makes
    /// `--gc-sections` able to drop anything: a linker can leave out a section nothing reaches and
    /// cannot leave out half of one.
    ///
    /// The empty `.text` stays, because it is the section the writer underneath opens a file with
    /// and gcc 16 leaves an empty one behind under the flag too.
    #[test]
    fn every_function_gets_a_section_of_its_own_when_that_is_what_was_asked_for() {
        let sections =
            Output { sections: Sections { functions: true, data: false }, ..Output::default() };
        let bytes = write(&two(), &Data::default(), &[], &target(), sections).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert_eq!(lives_in(&file, "f"), ".text.f");
        assert_eq!(lives_in(&file, "g"), ".text.g");
        assert!(file.section_by_name(".text").expect("the empty one").size() == 0);
        // Each one at nothing into its own section, and as long as it was: a function alone in a
        // section starts where the section does, whatever it started at when they shared one.
        for name in ["f", "g"] {
            let symbol = file.symbols().find(|s| s.name() == Ok(name)).expect("the function");
            assert_eq!(symbol.address(), 0, "{name}");
            assert_eq!(symbol.size(), 6, "{name}");
        }
        let section = file.section_by_name(".text.g").expect("the second function");
        assert_eq!(section.data().expect("the bytes"), &[0xe8, 0, 0, 0, 0, 0xc3]);
        // The padding between the two is gone with them, since it was there to align the second
        // one inside a section they shared and each section is aligned by the linker now.
        assert_eq!(section.align(), u64::from(crate::FUNC_ALIGN));
    }

    /// A relocation counts from the start of whichever section its function ended up in, which is
    /// the arithmetic the split path has to do and the unsplit one never does.
    ///
    /// Getting it wrong is a call patched over the wrong bytes, which assembles, links, and jumps
    /// into the middle of an instruction at run time.
    #[test]
    fn a_relocation_moves_with_the_function_whose_bytes_it_is_in() {
        let sections =
            Output { sections: Sections { functions: true, data: false }, ..Output::default() };
        let bytes = write(&two(), &Data::default(), &[], &target(), sections).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        for name in [".text.f", ".text.g"] {
            let section = file.section_by_name(name).expect("a function");
            let (offset, _) = section.relocations().next().expect("the call in it");
            // One byte in either way, because the call is the first instruction of both and the
            // opcode is one byte in front of the address the linker fills in.
            assert_eq!(offset, 1, "{name}");
            assert_eq!(section.relocations().count(), 1, "{name}");
        }
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
            visibility: Visibility::Default,
            relocs: Vec::new(),
        }
    }

    /// A file of that one variable and nothing else.
    fn holding(object: Object) -> Vec<u8> {
        let data = Data { objects: vec![object] };
        write(&Text::default(), &data, &[], &target(), Output::default()).expect("an object")
    }

    #[test]
    fn what_a_variable_is_decides_which_section_it_goes_in() {
        for (place, wanted) in [
            (Place::Written, ".data"),
            (Place::ReadOnly, ".rodata"),
            (Place::RelocReadOnly { local: false }, ".data.rel.ro"),
            (Place::RelocReadOnly { local: true }, ".data.rel.ro.local"),
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

    /// What `-fdata-sections` comes down to in an object file: the section a variable would have
    /// shared, with its own name after it. The names are gcc 16's, checked against it on a Linux
    /// host, and the part in front of the dot is what a linker script and `--gc-sections` match on.
    #[test]
    fn every_variable_gets_a_section_of_its_own_when_that_is_what_was_asked_for() {
        let sections =
            Output { sections: Sections { functions: false, data: true }, ..Output::default() };
        for (place, wanted) in [
            (Place::Written, ".data.x"),
            (Place::ReadOnly, ".rodata.x"),
            (Place::RelocReadOnly { local: false }, ".data.rel.ro.x"),
            (Place::RelocReadOnly { local: true }, ".data.rel.ro.local.x"),
            (Place::Zero, ".bss.x"),
        ] {
            let data = Data { objects: vec![variable("x", place.clone())] };
            let bytes = write(&Text::default(), &data, &[], &target(), sections).expect("object");
            let file = object::File::parse(&bytes[..]).expect("a readable object");
            assert_eq!(lives_in(&file, "x"), wanted, "{place:?}");
            let section = file.section_by_name(wanted).expect("the section it named");
            assert_eq!(section.size(), 4, "{place:?}");
            // Which page it lands in is what the section it came out of decided, and splitting
            // must not quietly change it: the zero filled one still carries none of its bytes.
            let carried = section.data().expect("the bytes").len();
            assert_eq!(carried, if place == Place::Zero { 0 } else { 4 }, "{place:?}");
        }
    }

    /// The two kinds of variable the flag leaves alone. A tentative definition is a request to the
    /// linker for that much zeroed space rather than an image, so there is no section to split off,
    /// and one the program named has the answer the source gave, which a flag must not overrule.
    #[test]
    fn a_variable_that_has_no_section_of_its_own_to_be_given_is_left_where_it_was() {
        let sections =
            Output { sections: Sections { functions: false, data: true }, ..Output::default() };
        let named = Place::Named(".init_array".to_owned());
        let objects = vec![variable("m", Place::Merged), variable("n", named)];
        let bytes =
            write(&Text::default(), &Data { objects }, &[], &target(), sections).expect("object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let m = file.symbols().find(|s| s.name() == Ok("m")).expect("the tentative one");
        assert!(m.is_common(), "still the linker's to merge and not in a section at all");
        assert_eq!(lives_in(&file, "n"), ".init_array");
        assert!(file.section_by_name(".init_array.n").is_none(), "the source already answered");
    }

    /// A relocation in a variable's image counts from the start of the section it ended up in, the
    /// same question the split text has to answer and a shorter answer: a variable alone in a
    /// section starts where the section does.
    #[test]
    fn a_relocation_in_an_image_moves_with_the_variable_whose_image_it_is_in() {
        let sections =
            Output { sections: Sections { functions: false, data: true }, ..Output::default() };
        let pointer = Object {
            bytes: vec![0; 8],
            size: 8,
            align: 8,
            relocs: vec![Reloc {
                at: 0,
                symbol: "y".to_owned(),
                kind: Reference::Address { bytes: 8 },
                addend: 0,
            }],
            ..variable("p", Place::Written)
        };
        let objects = vec![variable("first", Place::Written), pointer];
        let bytes =
            write(&Text::default(), &Data { objects }, &[], &target(), sections).expect("object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".data.p").expect("the pointer's own section");
        let (offset, reloc) = section.relocations().next().expect("one relocation");
        // Nothing rather than the eight it would be if the variable in front of it were still
        // counted, which is what a section of its own means.
        assert_eq!(offset, 0);
        assert_eq!(reloc.flags(), RelocationFlags::Elf { r_type: elf::R_X86_64_64 });
    }

    /// Two variables that want `.data.rel.ro.local` end up in one section, not two of one name.
    ///
    /// The writer has no name of its own for that section, so it is added by hand, and asking for
    /// it again makes a second section rather than handing back the first. SQLite has enough const
    /// tables of function pointers in it to turn that into eighty odd sections in one object, each
    /// with its own relocation section beside it, which is a pile of section headers describing
    /// eight bytes apiece.
    #[test]
    fn every_variable_that_wants_the_local_relocated_section_shares_one() {
        let place = Place::RelocReadOnly { local: true };
        let data =
            Data { objects: vec![variable("first", place.clone()), variable("second", place)] };
        let bytes =
            write(&Text::default(), &data, &[], &target(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let named = file.sections().filter(|s| s.name() == Ok(".data.rel.ro.local")).count();
        assert_eq!(named, 1, "one section holding both, not one each");
    }

    #[test]
    fn a_variable_is_a_symbol_that_says_where_it_is_and_how_long_it_is() {
        let mut data = Data { objects: vec![variable("first", Place::Written)] };
        data.objects.push(Object { align: 16, ..variable("second", Place::Written) });
        let bytes =
            write(&Text::default(), &data, &[], &target(), Output::default()).expect("an object");
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
        let bytes =
            write(&Text::default(), &data, &[], &target(), Output::default()).expect("an object");
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
        let aliases = [Alias {
            name: "b".to_owned(),
            target: "a".to_owned(),
            binding: Binding::Global,
            visibility: Visibility::Default,
        }];
        let bytes = write(&Text::default(), &data, &aliases, &target(), Output::default())
            .expect("an object");
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
        let aliases = [Alias {
            name: "g".to_owned(),
            target: "f".to_owned(),
            binding: Binding::Weak,
            visibility: Visibility::Default,
        }];
        let bytes = write(&text, &Data::default(), &aliases, &target(), Output::default())
            .expect("an object");
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
        let aliases = [Alias {
            name: "b".to_owned(),
            target: "a".to_owned(),
            binding: Binding::Global,
            visibility: Visibility::Default,
        }];
        let error =
            write(&Text::default(), &Data::default(), &aliases, &target(), Output::default())
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
            let error =
                write(&text, &Data::default(), &[], &TargetInfo::new(triple), Output::default())
                    .expect_err("no writer");
            assert!(matches!(error, Error::Format { .. }), "{error:?}");
        }
    }
}
