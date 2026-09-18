//! Relocatable objects, in whichever of the formats the target wants.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3, which says the three formats are written
//! through the [`object`] crate's writer with our own layer above it for the parts it does not
//! model. This is that layer, and what it holds is the part `object` cannot decide: which
//! relocation an instruction wants, what a symbol's binding and type are, and the sections a
//! linker expects to find whether or not anything was put in them.
//!
//! # One layout and two sets of answers
//!
//! Which sections a file has, what goes in each of them, which symbol says where each thing is and
//! what each relocation is against are the same questions for ELF and for COFF, and they have the
//! same answers, so they are asked once here. What differs is a short list: the number a relocation
//! is, the field a visibility goes in, the note saying what the file was built to have checked, and
//! the marker whose absence makes the stack executable. [`Flavour`] is that list, and the answers
//! are in [`crate::elf`] and [`crate::coff`] beside each other where they can be read against one
//! another.
//!
//! The alternative was two writers, and the reason against it is what a second copy of a layout
//! decays into: a fix to one of them is a fix to one platform, and which platform got it is
//! whichever the person who found the bug was building for.
//!
//! # What is not here
//!
//! Mach-O. The formats disagree about more than their headers: an Apple symbol carries an
//! underscore in front of the C name and Mach-O has no way to say how long a function is, wanting
//! `.subsections_via_symbols` instead. It is written when the target that needs it is.
//!
//! `.pdata` and `.xdata`, which is how Windows unwinds. They are a table describing the shape of a
//! prologue rather than the instruction stream ELF's `.eh_frame` is, so they are their own piece of
//! work, and a file whose functions have unwind records is refused for COFF rather than written
//! without them.
//!
//! Thread-local storage. Reaching a thread-local variable is a different instruction sequence per
//! model and the back end writes none of them, so a module carrying one is refused before it
//! reaches here rather than written as an ordinary variable in the wrong section.

use std::collections::HashMap;

use object::write::{
    Object as Writer, Relocation, StandardSection, Symbol, SymbolId, SymbolSection,
};
use object::{
    Architecture, BinaryFormat, Endianness, RelocationFlags, SectionFlags, SectionKind,
    SymbolFlags, SymbolKind, SymbolScope,
};
use rucc_target::{ObjectFormat, TargetInfo};
use rucc_tuple::Arch;

use crate::section::{
    Alias, Array, Binding, Data, Object, Output, Place, Property, Reference, Reloc, Sections, Text,
    Visibility,
};
use crate::{coff, elf};

/// Which of the two formats is being written, and therefore which set of answers the questions this
/// module cannot decide get.
///
/// A short list rather than a trait, because the list is short and closed: everything a format has
/// an opinion about is a call to one of the methods below, so adding Mach-O is adding a third arm to
/// each of them and the compiler names every one that was forgotten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavour {
    /// Linux, the BSDs and the freestanding targets.
    Elf,
    /// Windows, under either of its two runtimes.
    Coff,
}

impl Flavour {
    /// Which one a target wants, and nothing for the two formats that are not written.
    fn of(target: &TargetInfo) -> Option<Flavour> {
        match target.object_format {
            ObjectFormat::Elf => Some(Flavour::Elf),
            ObjectFormat::Coff => Some(Flavour::Coff),
            ObjectFormat::MachO | ObjectFormat::Wasm => None,
        }
    }

    /// The format the writer underneath is asked for.
    fn binary(self) -> BinaryFormat {
        match self {
            Flavour::Elf => BinaryFormat::Elf,
            Flavour::Coff => BinaryFormat::Coff,
        }
    }

    /// Which relocation this reference is, or `None` for one this format has none of.
    ///
    /// `after` is how many bytes of the instruction come after the four the linker writes over,
    /// which ELF has already folded into the addend and COFF wants told apart. See [`crate::Reloc`].
    fn reloc(self, reference: Reference, after: u8) -> Option<RelocationFlags> {
        match self {
            Flavour::Elf => elf::r_type(reference).map(|r_type| RelocationFlags::Elf { r_type }),
            Flavour::Coff => coff::reloc(reference, after),
        }
    }

    /// Say how far a name reaches beyond what its scope already said.
    ///
    /// Nothing on COFF, where a symbol has nowhere to keep it. A file built with
    /// `-fvisibility=hidden` for Windows is a file where that flag changed nothing, which is what
    /// gcc does there as well.
    fn see(self, obj: &mut Writer<'_>, id: SymbolId, binding: Binding, visibility: Visibility) {
        match self {
            Flavour::Elf => elf::see(obj, id, binding, visibility),
            Flavour::Coff => {}
        }
    }

    /// The section a variable the loader writes into before anything reads it goes in, when the
    /// program asked for the half of it the linker keeps apart, or nothing for a format that has no
    /// such half and puts one in ordinary read only data with the rest.
    fn rel_ro_local(self) -> Option<&'static str> {
        match self {
            Flavour::Elf => elf::REL_RO_LOCAL,
            Flavour::Coff => coff::REL_RO_LOCAL,
        }
    }

    /// The type and flags a section of function addresses the startup code calls has, where the
    /// format has something to say about it.
    ///
    /// Nothing on COFF, where such a section is refused by [`beyond`] before it reaches here rather
    /// than written under a name nothing on that platform gathers.
    fn gathered(self, array: Array) -> Option<SectionFlags> {
        match self {
            Flavour::Elf => Some(elf::gathered(array)),
            Flavour::Coff => None,
        }
    }

    /// The marker a linker looks for in every input, where there is one.
    fn marker(self, obj: &mut Writer<'_>) {
        match self {
            Flavour::Elf => elf::marker(obj),
            Flavour::Coff => coff::marker(obj),
        }
    }

    /// What the file says it was built to have checked, where the format has a way to say it.
    ///
    /// ELF writes a note the linker keeps only the agreed part of. A PE image says the same thing in
    /// the header of the finished image rather than in its inputs, so an object carries nothing and
    /// the instructions the flag asked for are in the text either way.
    fn property(self, obj: &mut Writer<'_>, property: Property) {
        if !property.any() {
            return;
        }
        match self {
            Flavour::Elf => {
                let note = obj.section_id(StandardSection::GnuProperty);
                obj.append_section_data(note, &elf::record(property), 8);
            }
            Flavour::Coff => {}
        }
    }

    /// Anything that has to be written into the finished bytes rather than said to the writer.
    fn finish(self, bytes: &mut [u8], ordered: &[String]) {
        match self {
            Flavour::Elf => elf::link(bytes, ordered),
            Flavour::Coff => debug_assert!(ordered.is_empty(), "a record this format cannot write"),
        }
    }
}

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

/// One text section and the variables beside it, as a relocatable object in the target's format.
///
/// # Errors
///
/// [`Error::Format`] for a machine or a platform this does not write, and [`Error::Refused`] for
/// anything the writer underneath objected to, which would be a bug here. An alias whose target
/// this file does not define is refused the same way, since the front end is what reports that as
/// a program's mistake and one reaching here means it did not. So is anything the target's format
/// has no way to write, which for COFF is a thread-local variable, a reference through a table the
/// platform does not have, a record of where a patcher's room is, an unwind table and a section the
/// startup code is expected to gather. See [`Error`].
pub fn write(
    text: &Text,
    data: &Data,
    aliases: &[Alias],
    target: &TargetInfo,
    output: Output,
) -> Result<Vec<u8>, Error> {
    let Output { sections, property } = output;
    let flavour = Flavour::of(target).filter(|_| target.tuple.arch() == Arch::X86_64);
    let Some(flavour) = flavour else {
        return Err(Error::Format { triple: target.tuple.to_string() });
    };
    if flavour == Flavour::Coff {
        beyond(text, data)?;
    }
    let mut obj = Writer::new(flavour.binary(), Architecture::X86_64, Endianness::Little);
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
            let name = elf::PATCHABLE.as_bytes().to_vec();
            let id = obj.add_section(Vec::new(), name, SectionKind::Data);
            obj.section_mut(id).flags = elf::ordered();
            obj.append_section_data(id, &[0; 8], 8);
            let symbol = obj.section_symbol(section);
            let flags = flavour.reloc(Reference::Address { bytes: 8 }, 0).ok_or_else(|| {
                Error::Refused { why: "no relocation holds an address here".to_owned() }
            })?;
            obj.add_relocation(
                id,
                Relocation { offset: 0, symbol, addend: (patch.at - base) as i64, flags },
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
        flavour.see(&mut obj, id, func.binding, func.visibility);
        symbols.insert(func.name.clone(), id);
        split.push((section, at));
    }

    // The places inside a function that have names of their own, which is where a label whose
    // address an image holds is. After the functions, because the section one goes in is the
    // section of the function it is inside and that is what the walk above worked out.
    for label in &text.labels {
        let after = text.funcs.partition_point(|func| func.start <= label.at);
        let Some(index) = after.checked_sub(1) else {
            let why = format!("'{}' is at {} and in front of every function", label.name, label.at);
            return Err(Error::Refused { why });
        };
        let func = &text.funcs[index];
        let (section, at) = if sections.functions {
            // From the start of the section rather than from the symbol, which is the same
            // correction a relocation inside a function gets below.
            let base = func.start - func.patch.map_or(0, |patch| patch.before);
            (split[index].0, (label.at - base) as u64)
        } else {
            (whole, label.at as u64)
        };
        let id = obj.add_symbol(Symbol {
            name: label.name.clone().into_bytes(),
            value: at,
            // A label has no length. What is at it is the rest of the function, and a size here
            // would be a claim that the bytes after it are a thing of their own.
            size: 0,
            kind: SymbolKind::Label,
            // Never offered to another file. The name is one the compiler minted and what it
            // points at is the middle of a function, so the only thing that resolves against it
            // is the image in this same file that asked for it.
            scope: SymbolScope::Compilation,
            weak: false,
            section: SymbolSection::Section(section),
            flags: SymbolFlags::None,
        });
        symbols.insert(label.name.clone(), id);
    }

    // Where each variable's image landed in the section it went into, kept because a relocation in
    // an image counts from the start of the image and one in a file counts from the start of the
    // section. A variable that is not in a section has no entry, since nothing in a merged one can
    // hold a relocation: the linker is being asked for zeroed space rather than for an image.
    let mut placed = Vec::with_capacity(data.objects.len());
    // The sections the writer has no name of its own for, remembered by name so that every variable
    // that wants one lands in the same one. The rest come back from `section_id`, which already
    // answers with the section it made the first time it was asked.
    let mut named = HashMap::new();
    for object in &data.objects {
        let (section, offset) = put(&mut obj, object, &mut named, sections, flavour);
        let id = obj.add_symbol(Symbol {
            name: object.name.clone().into_bytes(),
            // A common symbol says what it wants rather than where it is, and what it wants is
            // recorded where an ordinary symbol records its address.
            value: if object.place == Place::Merged { object.align } else { offset },
            size: object.size,
            // A thread-local variable is a different kind of symbol rather than a symbol in a
            // different section, and it has to be both: the kind is what a linker checks a
            // relocation against, so a `R_X86_64_PC32` aimed at one is refused rather than
            // resolved to an address that would have been one thread's and is nobody's.
            kind: match object.place {
                Place::Thread { .. } => SymbolKind::Tls,
                _ => SymbolKind::Data,
            },
            scope: scope_of(object.binding),
            weak: object.binding == Binding::Weak,
            section,
            flags: SymbolFlags::None,
        });
        flavour.see(&mut obj, id, object.binding, object.visibility);
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
        flavour.see(&mut obj, id, alias.binding, alias.visibility);
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
        add(&mut obj, section, at, reloc, &symbols, flavour)?;
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
            let flags = flavour.reloc(reloc.kind, reloc.after).ok_or_else(|| Error::Refused {
                why: format!("no relocation is {:?}", reloc.kind),
            })?;
            let record = Relocation {
                offset: reloc.at as u64,
                symbol,
                // Where the function starts inside its section, since the section symbol is where
                // the section starts and the two are the same byte only for the first function in
                // one.
                addend: reloc.addend + at as i64,
                flags,
            };
            obj.add_relocation(frames, record)
                .map_err(|why| Error::Refused { why: why.to_string() })?;
        }
    }
    for (object, &(section, offset)) in data.objects.iter().zip(&placed) {
        let Some(section) = section else { continue };
        for reloc in &object.relocs {
            add(&mut obj, section, offset + reloc.at as u64, reloc, &symbols, flavour)?;
        }
    }

    // What the file was built to have checked, when it was built to have anything checked. Left
    // out otherwise rather than written as a zero, because a linker treats a missing note and a
    // note with no bits in it the same way and gcc writes nothing.
    flavour.property(&mut obj, property);

    // Written rather than left out, because a linker that does not find it in every input marks
    // the stack executable, on the format that has one.
    flavour.marker(&mut obj);

    let mut bytes = obj.write().map_err(|why| Error::Refused { why: why.to_string() })?;
    flavour.finish(&mut bytes, &ordered);
    Ok(bytes)
}

/// Everything in this module the target's format has no way to write, refused by name.
///
/// Each of these is something ELF has and COFF does not, and each would otherwise be written as the
/// nearest thing rather than refused, which is worse: a thread-local variable written as an ordinary
/// one is a program where every thread shares what the source said each would have its own copy of,
/// and a constructor list under a name the Windows runtime does not gather is a program whose
/// constructors never run. A message naming the feature is what the caller turns into a diagnostic,
/// and the front end refusing first is what stops one ever being seen.
///
/// # Errors
///
/// [`Error::Refused`], naming the one it found first.
fn beyond(text: &Text, data: &Data) -> Result<(), Error> {
    let why = |why: String| Err(Error::Refused { why });
    if text.funcs.iter().any(|func| func.patch.is_some()) {
        return why("a record of where a patcher's room is has no section flags here".to_owned());
    }
    if !text.unwind.bytes.is_empty() {
        return why("unwinding is a table rather than a section of records here".to_owned());
    }
    for reloc in text.relocs.iter().chain(data.objects.iter().flat_map(|object| &object.relocs)) {
        if matches!(reloc.kind, Reference::Got | Reference::Thread) {
            return why(format!("nothing reaches '{}' through a table here", reloc.symbol));
        }
    }
    for object in &data.objects {
        if matches!(object.place, Place::Thread { .. }) {
            return why(format!("'{}' is thread-local and this format is not", object.name));
        }
        if let Place::Named(name) = &object.place
            && Array::of(name).is_some()
        {
            return why(format!("'{name}' is not a list the startup code here gathers"));
        }
    }
    Ok(())
}

/// Every name a linker can find in the object [`write()`] would write from the same input.
///
/// What asks for this is the archive writer. A static link resolves through the symbol index, so an
/// index entry has to name a symbol the member really defines: an entry for a name that is not in
/// the member is an archive the linker searches, pulls the member out of, and then still reports
/// the name undefined. So the list comes from the writer rather than from the caller, because the
/// writer is the only thing that knows what it wrote.
///
/// The names are as the C program spelled them, with nothing in front of them, which is what both
/// the formats this writes have on this machine. Mach-O puts an underscore there and so does COFF on
/// a 32-bit machine, and when either of those is written this is the function that has to say so,
/// which is why it asks about the target it otherwise would not have to.
///
/// Order is the functions, then the variables, then the aliases, each in the order the module held
/// them, which is the order [`write()`] adds the symbols in. A `static` is left out: it is a name the
/// link has already finished with by the time an archive is searched, and an index entry for one
/// would offer the linker a definition it is not allowed to use.
///
/// # Errors
///
/// [`Error::Format`] for a machine or a platform this does not write, which is the same refusal
/// [`write()`] gives and is here for the same reason: a list of undecorated names for a format whose
/// symbols carry an underscore is worse than no list at all.
pub fn defines(
    text: &Text,
    data: &Data,
    aliases: &[Alias],
    target: &TargetInfo,
) -> Result<Vec<String>, Error> {
    if target.tuple.arch() != Arch::X86_64 || Flavour::of(target).is_none() {
        return Err(Error::Format { triple: target.tuple.to_string() });
    }
    let names = text
        .funcs
        .iter()
        .filter(|func| func.binding != Binding::Local)
        .map(|func| func.name.clone())
        .chain(
            data.objects
                .iter()
                .filter(|object| object.binding != Binding::Local)
                .map(|object| object.name.clone()),
        )
        .chain(
            aliases
                .iter()
                .filter(|alias| alias.binding != Binding::Local)
                .map(|alias| alias.name.clone()),
        )
        .collect();
    Ok(names)
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
    named: &mut HashMap<String, object::write::SectionId>,
    sections: Sections,
    flavour: Flavour,
) -> (SymbolSection, u64) {
    // A section of its own, named after the variable and after the section it would have gone in,
    // which is what `-fdata-sections` asks for. A merged variable has no section to split and a
    // named one was named by the program, so both are left where they are: the first is a request
    // to the linker rather than an image, and the second would otherwise have the flag silently
    // overrule what the source said.
    if sections.data {
        if let Some(name) = object.place.split(&object.name) {
            let section = obj.add_section(Vec::new(), name.into_bytes(), kind_of(&object.place));
            let offset = if carries_no_bytes(&object.place) {
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
        Place::RelocReadOnly { local } => match flavour.rel_ro_local().filter(|_| *local) {
            Some(name) => made(obj, named, name, SectionKind::ReadOnlyDataWithRel),
            None => obj.section_id(StandardSection::ReadOnlyDataWithRel),
        },
        Place::Zero => obj.section_id(StandardSection::UninitializedData),
        Place::Thread { zero: false } => obj.section_id(StandardSection::Tls),
        Place::Thread { zero: true } => obj.section_id(StandardSection::UninitializedTls),
        Place::Merged => return (SymbolSection::Common, 0),
        // A named section is the program's word for where this goes, and a program that names one
        // wants what it named rather than what would have been chosen. It is written as ordinary
        // data because nothing in the IR says otherwise, except for the three names the startup
        // code calls what it finds in, which have a section type of their own and are gathered by
        // the linker whether or not they carry it.
        Place::Named(name) => {
            let section = made(obj, named, name, SectionKind::Data);
            if let Some(flags) = Array::of(name).and_then(|array| flavour.gathered(array)) {
                obj.section_mut(section).flags = flags;
            }
            section
        }
    };
    let offset = if carries_no_bytes(&object.place) {
        obj.append_section_bss(section, object.size, object.align)
    } else {
        obj.append_section_data(section, &object.bytes, object.align)
    };
    (SymbolSection::Section(section), offset)
}

/// Whether the section this goes in says how big the variable is and holds none of its bytes.
///
/// Two of them, and they are the same answer twice: `.bss` is the image that is all zeros, and
/// `.tbss` is a thread's own copy of one. A section like this costs its size in the section header
/// and nothing in the file, which is what keeps a program with a large zeroed array small.
fn carries_no_bytes(place: &Place) -> bool {
    matches!(place, Place::Zero | Place::Thread { zero: true })
}

/// The section of this name, made the first time it is asked for and found afterwards.
///
/// Two variables the program put the same section name on belong in one section, the way two in
/// `.data` do. Asking the writer for a new one each time would make a second header with the same
/// name, which a linker takes and which makes a file with ten constructors in it carry ten section
/// headers describing eight bytes each. `section_id` does this already for the sections it has
/// names of its own for, and this is the same answer for the ones it does not.
fn made(
    obj: &mut Writer<'_>,
    named: &mut HashMap<String, object::write::SectionId>,
    name: &str,
    kind: SectionKind,
) -> object::write::SectionId {
    if let Some(section) = named.get(name) {
        return *section;
    }
    let section = obj.add_section(Vec::new(), name.as_bytes().to_vec(), kind);
    named.insert(name.to_owned(), section);
    section
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
        Place::Thread { zero: false } => SectionKind::Tls,
        Place::Thread { zero: true } => SectionKind::UninitializedTls,
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
    flavour: Flavour,
) -> Result<(), Error> {
    let flags = flavour
        .reloc(reloc.kind, reloc.after)
        .ok_or_else(|| Error::Refused { why: format!("no relocation is {:?}", reloc.kind) })?;
    obj.add_relocation(
        section,
        Relocation { offset: at, symbol: symbols[&reloc.symbol], addend: reloc.addend, flags },
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
pub(crate) fn scope_of(binding: Binding) -> SymbolScope {
    match binding {
        Binding::Local => SymbolScope::Compilation,
        Binding::Global | Binding::Weak => SymbolScope::Dynamic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use object::read::elf::Sym as _;
    use object::read::{Object as _, ObjectSection as _, ObjectSymbol as _};
    use object::{elf, pe};
    use rucc_target::{Arch, Env, Os, Triple};

    use crate::elf::PATCHABLE;
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
                after: 0,
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
            (Reference::Thread, elf::R_X86_64_GOTTPOFF),
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
            after: 0,
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
                after: 0,
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
                after: 0,
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
            after: 0,
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
            after: 0,
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
            bytes: if carries_no_bytes(&place) { Vec::new() } else { vec![1, 0, 0, 0] },
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
            (Place::Thread { zero: false }, ".tdata"),
            (Place::Thread { zero: true }, ".tbss"),
            (Place::Named(".init_array".to_owned()), ".init_array"),
        ] {
            let bytes = holding(variable("x", place.clone()));
            let file = object::File::parse(&bytes[..]).expect("a readable object");
            let section = file.section_by_name(wanted).unwrap_or_else(|| panic!("{place:?}"));
            assert_eq!(section.size(), 4, "{place:?}");
            // The zero filled one is as long as it says and carries none of it, which is the
            // whole reason the section exists.
            let carried = section.data().expect("the bytes").len();
            assert_eq!(carried, if carries_no_bytes(&place) { 0 } else { 4 }, "{place:?}");
        }
    }

    /// The section is half of it and the symbol is the other half.
    ///
    /// A linker checks a relocation against the kind of the symbol it names, so a variable that is
    /// in `.tdata` and is an ordinary data symbol is one an ordinary reference resolves to an
    /// address that belongs to no thread. `STT_TLS` is what makes that reference an error instead.
    #[test]
    fn a_thread_local_variable_is_a_thread_local_symbol_and_not_only_a_thread_local_section() {
        for place in [Place::Thread { zero: false }, Place::Thread { zero: true }] {
            let bytes = holding(variable("counter", place.clone()));
            let file = object::File::parse(&bytes[..]).expect("a readable object");
            let symbol = file
                .symbols()
                .find(|symbol| symbol.name() == Ok("counter"))
                .unwrap_or_else(|| panic!("{place:?}"));
            assert_eq!(symbol.kind(), SymbolKind::Tls, "{place:?}");
        }
    }

    /// The section type a startup list carries, which is what makes the CRT call what is in it.
    ///
    /// A section of the ordinary type with the right name is gathered by the linker in the same run
    /// and called by nobody, so the type is the whole of what this is about. The numbered name is
    /// the same kind of section as the plain one: the number is there so that the linker sorts it.
    #[test]
    fn a_section_of_function_addresses_carries_the_type_the_runtime_looks_for() {
        for (name, wanted) in [
            (".init_array", elf::SHT_INIT_ARRAY),
            (".init_array.00101", elf::SHT_INIT_ARRAY),
            (".fini_array", elf::SHT_FINI_ARRAY),
            (".preinit_array", elf::SHT_PREINIT_ARRAY),
            (".init_arrays", elf::SHT_PROGBITS),
        ] {
            let bytes = holding(variable("x", Place::Named(name.to_owned())));
            let file = object::File::parse(&bytes[..]).expect("a readable object");
            let section = file.section_by_name(name).unwrap_or_else(|| panic!("{name}"));
            let SectionFlags::Elf { sh_type, sh_flags } = section.flags() else {
                panic!("{name} is not an elf section");
            };
            assert_eq!(sh_type, wanted, "{name}");
            assert!(sh_flags.contains(elf::SHF_ALLOC | elf::SHF_WRITE), "{name}");
        }
    }

    /// Two variables the program put one section name on, which belong in one section.
    ///
    /// A file with ten constructors in it would otherwise carry ten section headers describing eight
    /// bytes each, and the order the entries run in would be the order the linker happened to put
    /// the headers in rather than the order they were written.
    #[test]
    fn two_variables_in_one_named_section_share_it() {
        let objects = vec![
            variable("x", Place::Named(".init_array".to_owned())),
            variable("y", Place::Named(".init_array".to_owned())),
        ];
        let data = Data { objects };
        let bytes =
            write(&Text::default(), &data, &[], &target(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let named: Vec<_> =
            file.sections().filter(|section| section.name() == Ok(".init_array")).collect();
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].size(), 8);
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
            (Place::Thread { zero: false }, ".tdata.x"),
            (Place::Thread { zero: true }, ".tbss.x"),
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
            assert_eq!(carried, if carries_no_bytes(&place) { 0 } else { 4 }, "{place:?}");
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
                after: 0,
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
                after: 0,
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
                after: 0,
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

    /// What the archive's symbol index is built from is what the linker can find in the member.
    ///
    /// Written against the object rather than against the list, because the two agreeing is the
    /// whole point: a list that says more than the file does is an archive that promises a
    /// definition it does not have, and a list that says less is a member nothing pulls out.
    #[test]
    fn the_names_a_linker_can_find_are_the_names_the_list_gives() {
        let mut text = calling("puts");
        text.funcs.push(extent("hidden".to_owned(), 16, 1, Binding::Local));
        text.funcs.push(extent("shared".to_owned(), 32, 1, Binding::Weak));
        text.bytes.resize(33, 0x90);
        let data = Data {
            objects: vec![variable("seen", Place::Written), {
                let mut quiet = variable("quiet", Place::Zero);
                quiet.binding = Binding::Local;
                quiet
            }],
        };
        let aliases = [Alias {
            name: "second".to_owned(),
            target: "f".to_owned(),
            binding: Binding::Global,
            visibility: Visibility::Default,
        }];

        let names = defines(&text, &data, &aliases, &target()).expect("a list");
        assert_eq!(names, ["f", "shared", "seen", "second"]);

        let bytes = write(&text, &data, &aliases, &target(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let found: Vec<String> = file
            .symbols()
            .filter(|symbol| symbol.is_global() && symbol.is_definition())
            .map(|symbol| symbol.name().unwrap_or_default().to_owned())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        let mut theirs = found;
        theirs.sort();
        assert_eq!(sorted, theirs, "the list and the file have to say the same thing");
    }

    /// A windows x86-64 target, which is the other format this writes.
    fn windows() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Windows, Env::Gnu))
    }

    /// What the four bytes a relocation covers hold, which is where COFF keeps its addend.
    fn inline(bytes: &[u8], section: &str, at: usize) -> i32 {
        let file = object::File::parse(bytes).expect("a readable object");
        let found = file.section_by_name(section).expect("the section").data().expect("the bytes");
        i32::from_le_bytes(found[at..at + 4].try_into().expect("four bytes"))
    }

    #[test]
    fn a_windows_target_is_written_rather_than_refused() {
        let text = calling("puts");
        let bytes =
            write(&text, &Data::default(), &[], &windows(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert_eq!(file.format(), BinaryFormat::Coff);
        let section = file.section_by_name(".text").expect("a text section");
        assert_eq!(section.data().expect("the bytes"), &text.bytes[..]);
        let names: Vec<&str> = file.symbols().filter_map(|symbol| symbol.name().ok()).collect();
        assert!(names.contains(&"f"), "{names:?}");
        assert!(names.contains(&"puts"), "{names:?}");
    }

    /// The whole reason a relocation carries where the instruction ended as well as the addend.
    ///
    /// A call ends at the four bytes the linker writes over, and a store of a constant through an
    /// address counted from the instruction pointer has the constant after them, and ELF tells the
    /// two apart by the addend alone. COFF cannot: it says how far the end is in the relocation type
    /// and works the addend out from that, so the same four bytes come out of two different types
    /// and both have to end up meaning the same distance.
    #[test]
    fn how_far_the_instruction_runs_past_the_hole_is_in_the_relocation_type() {
        for (after, typ) in [
            (0, pe::IMAGE_REL_AMD64_REL32),
            (1, pe::IMAGE_REL_AMD64_REL32_1),
            (4, pe::IMAGE_REL_AMD64_REL32_4),
            (5, pe::IMAGE_REL_AMD64_REL32_5),
        ] {
            let mut text = calling("puts");
            // The same distance every time, said the way ELF says it: from where the four bytes
            // start, with everything else folded in.
            text.relocs[0].addend = -4 - i64::from(after);
            text.relocs[0].after = after;
            text.bytes.resize(6 + after as usize, 0x90);
            text.funcs[0].len = text.bytes.len();
            let bytes = write(&text, &Data::default(), &[], &windows(), Output::default())
                .expect("an object");
            let file = object::File::parse(&bytes[..]).expect("a readable object");
            let section = file.section_by_name(".text").expect("a text section");
            let (_, reloc) = section.relocations().next().expect("the relocation");
            assert_eq!(reloc.flags(), RelocationFlags::Coff { typ }, "{after}");
            // And the bytes come out holding nothing, because the distance the instruction wants
            // and the distance the type already says are the same one.
            assert_eq!(inline(&bytes, ".text", 1), 0, "{after}");
        }
    }

    /// The addend a COFF object keeps is in the bytes rather than in the relocation, so the number
    /// the caller handed over has to survive the trip through the type.
    #[test]
    fn a_distance_the_instruction_did_not_ask_for_stays_in_the_bytes() {
        let mut text = calling("puts");
        text.relocs[0].addend = 12;
        let bytes =
            write(&text, &Data::default(), &[], &windows(), Output::default()).expect("an object");
        assert_eq!(inline(&bytes, ".text", 1), 16, "twelve past the end, which is four past here");
    }

    #[test]
    fn an_address_written_into_an_image_is_the_wide_relocation_here_too() {
        let object = Object {
            bytes: vec![0; 8],
            size: 8,
            align: 8,
            relocs: vec![Reloc {
                at: 0,
                symbol: "y".to_owned(),
                kind: Reference::Address { bytes: 8 },
                addend: 0,
                after: 0,
            }],
            ..variable("p", Place::Written)
        };
        let data = Data { objects: vec![object] };
        let bytes =
            write(&Text::default(), &data, &[], &windows(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".data").expect("a data section");
        let (_, reloc) = section.relocations().next().expect("the relocation");
        let typ = pe::IMAGE_REL_AMD64_ADDR64;
        assert_eq!(reloc.flags(), RelocationFlags::Coff { typ });
    }

    /// `.data.rel.ro` is an ELF answer to a problem this format solves elsewhere, so both halves of
    /// it land in ordinary read only data, which is where the platform's own linker puts them.
    #[test]
    fn a_variable_the_loader_writes_into_is_read_only_data_here() {
        for local in [false, true] {
            let data = Data { objects: vec![variable("p", Place::RelocReadOnly { local })] };
            let bytes = write(&Text::default(), &data, &[], &windows(), Output::default())
                .expect("an object");
            let file = object::File::parse(&bytes[..]).expect("a readable object");
            assert!(file.section_by_name(".rdata").is_some(), "{local}");
            assert!(file.section_by_name(".data.rel.ro.local").is_none(), "{local}");
        }
    }

    /// No marker and no note, because a PE image says both of those things in the header of the
    /// finished image rather than in each of its inputs.
    #[test]
    fn the_sections_only_elf_reads_are_left_out_rather_than_written_empty() {
        let text = calling("puts");
        let output = Output { property: Property { features: 3 }, ..Output::default() };
        let bytes = write(&text, &Data::default(), &[], &windows(), output).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert!(file.section_by_name(".note.GNU-stack").is_none());
        assert!(file.section_by_name(".note.gnu.property").is_none());
    }

    /// Each of these is something this format has no way to write, and writing the nearest thing
    /// would be worse than refusing: a thread-local variable written as an ordinary one is one copy
    /// where the program asked for one per thread, and a constructor list under a name nothing
    /// gathers is a program whose constructors never run.
    #[test]
    fn what_this_format_cannot_say_is_refused_by_name() {
        let ordinary = Text::default();
        let empty = Data::default();

        let mut thread = Data::default();
        thread.objects.push(variable("t", Place::Thread { zero: false }));

        let mut gathered = Data::default();
        gathered.objects.push(variable("c", Place::Named(".init_array".to_owned())));

        let mut table = calling("puts");
        table.relocs[0].kind = Reference::Got;

        let mut room = calling("puts");
        room.funcs[0].patch = Some(Patch { at: 0, before: 0 });

        let mut unwound = calling("puts");
        unwound.unwind.bytes = vec![0; 32];

        let cases: [(&str, &Text, &Data); 5] = [
            ("thread-local", &ordinary, &thread),
            ("startup", &ordinary, &gathered),
            ("table", &table, &empty),
            ("patcher", &room, &empty),
            ("unwinding", &unwound, &empty),
        ];
        for (what, text, data) in cases {
            let error = write(text, data, &[], &windows(), Output::default())
                .expect_err("something this format cannot write");
            assert!(matches!(error, Error::Refused { .. }), "{what}: {error:?}");
        }
    }

    /// A visibility is not refused, because there is nothing to refuse: it is a fact about a dynamic
    /// symbol table and a COFF symbol has nowhere to keep one, which is what gcc does on the
    /// platform as well.
    #[test]
    fn a_visibility_this_format_cannot_keep_changes_nothing_rather_than_failing() {
        let mut text = calling("puts");
        text.funcs[0].visibility = Visibility::Hidden;
        let bytes =
            write(&text, &Data::default(), &[], &windows(), Output::default()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let symbol = file.symbols().find(|symbol| symbol.name() == Ok("f")).expect("the function");
        assert!(symbol.is_global(), "a name others may use either way");
    }

    #[test]
    fn the_names_a_linker_can_find_are_the_same_list_on_either_format() {
        let text = calling("puts");
        let data = Data { objects: vec![variable("shared", Place::Written)] };
        let theirs = defines(&text, &data, &[], &windows()).expect("a list");
        assert_eq!(theirs, defines(&text, &data, &[], &target()).expect("a list"));
    }

    /// The same refusal the writer gives, for the reason the function says: an undecorated name is
    /// the wrong answer for a format whose symbols carry an underscore, and a wrong index entry is
    /// worse than no archive.
    #[test]
    fn a_platform_this_does_not_write_has_no_list_of_names_either() {
        let text = calling("puts");
        for triple in [
            Triple::new(Arch::Aarch64, Os::Linux, Env::Gnu),
            Triple::new(Arch::X86_64, Os::Darwin, Env::Gnu),
        ] {
            let error = defines(&text, &Data::default(), &[], &TargetInfo::new(triple))
                .expect_err("no writer");
            assert!(matches!(error, Error::Format { .. }), "{error:?}");
        }
    }
}
