//! An object file written from what a file of assembly says, rather than from a compilation.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1, the paragraph that says we also accept
//! assembly as input.
//!
//! # Why this is not [`crate::Text`] and [`crate::Data`]
//!
//! Those two are the compiler's view of a file and they are the right view of one. A function is a
//! run of bytes with a name and a length, a variable is an image with a name and a place worked out
//! from what the variable is, and neither carries a section name because where a thing goes is an
//! answer rather than a question. That is exactly what makes them the wrong shape for assembly.
//!
//! A file of assembly says the section, so the place is a question again, and it may say a section
//! this compiler would never have chosen and flags that go with it. It puts names at offsets rather
//! than around images, so `.long 0` followed by `foo:` is four bytes belonging to nothing with a
//! name after them, which no list of named variables can hold. It defines names that are not at any
//! offset at all, which is what `.set` and `.equ` produce. And it may name a symbol in the middle of
//! a section, with a size the program stated rather than one worked out from the bytes.
//!
//! So this is the assembler's view: a list of sections that each know their own name, flags and
//! bytes, and a list of names that point into them. Bending one into the other would mean deciding
//! here what a program already said, and a wrong answer about which section something is in is not
//! visible until a link or a load.
//!
//! The two views meet at the [`object`] crate's writer, which is what both call, and at the short
//! list of format opinions beside it, which is what both ask where the formats differ. So
//! there is one place that knows how an object file is laid out and one that knows what each format
//! calls the things in it.

use object::write::{Object as Writer, Relocation, Symbol, SymbolSection};
use object::{Architecture, Endianness, RelocationFlags, SectionKind, SymbolFlags, elf};
use rucc_target::TargetInfo;
use rucc_target::aarch64::Fixup;
use rucc_tuple::Arch;

use crate::file::{Error, Flavour};
use crate::section::{Array, Binding, Info, Reloc, Visibility};

/// One section, as a file of assembly describes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// What it is called, with the leading dot the source wrote.
    pub name: String,
    /// Its bytes, which are empty for a section that says how big it is and holds none of them.
    pub bytes: Vec<u8>,
    /// How long it is. The same as the length of the bytes for every section that has any, and the
    /// whole of what a `@nobits` section says about itself.
    pub size: u64,
    /// The boundary it starts on, which is the largest any directive in it asked for.
    pub align: u64,
    /// The flags and the type, which the source states and this does not work out.
    pub shape: Shape,
    /// Every place in it that names something, counted from the start of the section.
    pub relocs: Vec<Reloc>,
}

/// What a section is, which on ELF is a handful of flag letters and a type.
///
/// Held as the separate facts rather than as one of a fixed list of kinds, because the list is not
/// fixed: a program may write `.section .init.text,"ax",@progbits` and mean a section this compiler
/// has no name for, and the letters are the whole of what it said about it. The writer underneath
/// takes a [`SectionKind`], so `Shape::kind` is the one place that turns these back into one, and
/// the cases it cannot say are written as flags directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Shape {
    /// `a`: the section takes space in the loaded image. A section without this is for a debugger
    /// or a linker to read and is not in the program at run time.
    pub alloc: bool,
    /// `w`: the program may write to it.
    pub write: bool,
    /// `x`: the processor may execute it.
    pub exec: bool,
    /// `T`: one copy per thread rather than one copy per program.
    pub thread: bool,
    /// Whether the file carries the bytes. False is `@nobits`, which is what `.bss` is.
    pub bits: bool,
    /// Which kind of table of function addresses this is, for the three ELF has a type for.
    pub array: Option<Array>,
    /// `M`: how long each entry is in a section of constants the linker may keep one copy of
    /// wherever two objects hold the same one, and zero for a section that is not one of those.
    /// gcc puts a `double` it loads from memory in `.rodata.cst8`, which is one of these.
    pub merge: u64,
    /// `S`: the entries are strings ended by a zero rather than all of one length, which is where
    /// gcc puts every string literal. Only means anything beside `merge`.
    pub strings: bool,
}

impl Shape {
    /// What a section of this name is when the source named it and said nothing else.
    ///
    /// `.text`, `.data` and the rest are names an assembler already knows the flags of, which is
    /// why a program may write `.data` on its own and why `.section .data` without letters is the
    /// same section rather than an unallocated one. A name nothing here knows gets the flags of an
    /// ordinary allocated writable section, which is what gas does with one.
    #[must_use]
    pub fn of(name: &str) -> Shape {
        let base = Shape { alloc: true, bits: true, ..Shape::default() };
        let head = name.split_once('.').map_or(name, |(_, rest)| rest);
        let head = head.split_once('.').map_or(head, |(first, _)| first);
        match head {
            "text" | "init" | "fini" => Shape { exec: true, ..base },
            "rodata" | "eh_frame_hdr" => base,
            "bss" => Shape { write: true, bits: false, ..base },
            "tbss" => Shape { write: true, thread: true, bits: false, ..base },
            "tdata" => Shape { write: true, thread: true, ..base },
            // The three the linker gathers and the startup code walks. The type is what makes one
            // of them that, rather than the name: a section of the ordinary type under the same
            // name is gathered into the same run and called by nobody.
            _ if Array::of(name).is_some() => Shape { write: true, array: Array::of(name), ..base },
            // Not allocated, because nothing in the running program reads it. A debugger reads it
            // out of the file, and a section marked allocated would take space in every process.
            "debug_info" | "debug_abbrev" | "debug_line" | "debug_str" | "comment" => {
                Shape { alloc: false, bits: true, ..Shape::default() }
            }
            _ => Shape { write: true, ..base },
        }
    }

    /// The flag word ELF holds these in.
    ///
    /// Not public, and neither are the two below it. The fields above are the whole of what a
    /// caller says about a section, and how ELF spells them is this crate's business: a reader that
    /// had to name an ELF constant to describe an executable section would be one that could not
    /// describe one for any other format.
    pub(crate) fn sh_flags(self) -> elf::SectionFlags {
        let mut flags = 0;
        if self.alloc {
            flags |= elf::SHF_ALLOC.0;
        }
        if self.write {
            flags |= elf::SHF_WRITE.0;
        }
        if self.exec {
            flags |= elf::SHF_EXECINSTR.0;
        }
        if self.thread {
            flags |= elf::SHF_TLS.0;
        }
        if self.merge != 0 {
            flags |= elf::SHF_MERGE.0;
            if self.strings {
                flags |= elf::SHF_STRINGS.0;
            }
        }
        elf::SectionFlags(flags)
    }

    /// The type ELF holds in the header beside those flags.
    pub(crate) fn sh_type(self) -> elf::SectionType {
        match self.array {
            _ if !self.bits => elf::SHT_NOBITS,
            Some(Array::Init) => elf::SHT_INIT_ARRAY,
            Some(Array::Fini) => elf::SHT_FINI_ARRAY,
            Some(Array::Preinit) => elf::SHT_PREINIT_ARRAY,
            None => elf::SHT_PROGBITS,
        }
    }

    /// What the writer underneath calls the nearest thing to this.
    ///
    /// It is told the flags in full afterwards, so this only has to be close enough that nothing
    /// else the writer decides from the kind comes out wrong, which is the default alignment and
    /// whether it appends bytes or counts them.
    pub(crate) const fn kind(self) -> SectionKind {
        match self {
            Shape { bits: false, thread: true, .. } => SectionKind::UninitializedTls,
            Shape { bits: false, .. } => SectionKind::UninitializedData,
            Shape { thread: true, .. } => SectionKind::Tls,
            Shape { exec: true, .. } => SectionKind::Text,
            Shape { alloc: false, .. } => SectionKind::Other,
            Shape { write: false, .. } => SectionKind::ReadOnlyData,
            Shape { .. } => SectionKind::Data,
        }
    }
}

/// One name in the symbol table, as a file of assembly defines one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    /// The name, spelled as the source spelled it.
    pub name: String,
    /// Where it is.
    pub at: Held,
    /// How long the thing it names is, which is what `.size` said and is zero when nothing did.
    pub size: u64,
    /// What kind of thing it names, which is what `.type` said.
    pub sort: Sort,
    /// Who can see it.
    pub binding: Binding,
    /// How far outside a shared library it reaches.
    pub visibility: Visibility,
}

/// Where a name is, which is four different things and not an offset with special cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// At an offset into one of the sections, which is what a label is.
    In {
        /// Which section, as an index into the list given alongside.
        part: usize,
        /// How far into it.
        offset: u64,
    },
    /// A number rather than a place, which is what `.set` and `.equ` produce. The linker resolves
    /// a reference to one to the number itself and there is nothing for it to be relative to.
    Absolute(u64),
    /// That much zeroed space asked of the linker under this name, which is `.comm` and `.lcomm`.
    /// Every definition of the name across every object is merged into one.
    Common {
        /// How much space.
        size: u64,
        /// What boundary it has to start on. ELF records this where an ordinary symbol records its
        /// address, which is why the two cannot both be said.
        align: u64,
    },
    /// Named and not defined here, which the linker has to find somewhere else.
    Undefined,
}

/// What kind of thing a name names, which is what `.type` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// `@function`. A call through the procedure linkage table may be made to it.
    Func,
    /// `@object`. Data.
    Object,
    /// `@tls_object`. A thread-local variable, which a linker checks relocations against.
    Thread,
    /// `.file`, which names the source this was assembled from rather than anything in it.
    ///
    /// Not a thing `.type` can say, and here because it is a symbol and there is nowhere else for
    /// it. A debugger reads it and so does `nm`, and gas writes one for every file that says its
    /// own name, which is every file gcc produces.
    File,
    /// Nothing was said, which is what a plain label gets and is a real answer rather than a
    /// missing one: gas writes `STT_NOTYPE` for a label nobody stated a type for.
    #[default]
    Untyped,
}

/// Everything an assembled file holds: its sections, and the names that point into them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assembled {
    /// The sections, in the order the file first mentioned each of them.
    pub parts: Vec<Part>,
    /// The names, in the order the file defined or first referred to each of them.
    pub names: Vec<Name>,
}

/// That, as a relocatable object in whichever of the two formats the target wants.
///
/// Both formats, the same two the module that writes a compilation writes it into, and the
/// differences between them are the same answers there. That is the whole reason this is not two
/// functions: a file of assembly names its own sections and a compilation does not, but what a
/// relocation is called and whether a symbol has anywhere to keep a visibility are facts about the
/// format rather than about where the bytes came from, and a second set of answers to them would
/// be a second set to get wrong.
///
/// What a [`Part`] carries is the section type and flags the source wrote in as many words. ELF has
/// a field for each of them and they are written down as they stand. COFF has no field they map
/// onto, so what the section is comes from the kind on the shape and the writer underneath turns it
/// into the characteristics every other Windows assembler writes. A program that means a Windows
/// section to be something other than what its name says is a program that has to say so some other
/// way, which is what `.section` with COFF's own letters is for and what tamnd/rucc#1514 left open.
///
/// # Errors
///
/// [`Error::Format`] for a machine or a platform this does not write, and [`Error::Refused`] for a
/// relocation against a name the list does not hold or one this format has no relocation for.
pub fn assembled(input: &Assembled, target: &TargetInfo) -> Result<Vec<u8>, Error> {
    assembled_described(input, target, &Info::default())
}

/// The same object as [`assembled`], with the debug sections in `info` added to it.
///
/// For a compilation that went through a listing and asked for debug information, where the line
/// table and the entries are built from the compilation rather than read from the file. A
/// relocation in a chunk names another chunk or a name the file defines, and the second is written
/// against the section the name is in, for the reason [`crate::write`] gives: a distance to a
/// global name is not one a linker can work out.
///
/// # Errors
///
/// As for [`assembled`], and [`Error::Refused`] for a chunk that names something the file does
/// not define.
pub fn assembled_described(
    input: &Assembled,
    target: &TargetInfo,
    info: &Info,
) -> Result<Vec<u8>, Error> {
    // AArch64 on ELF and x86-64 on both. What an AArch64 file for Windows would need is a table of
    // its own relocations and an unwind table of its own shape, and neither is written yet.
    let (flavour, machine) = match (Flavour::of(target), target.tuple.arch()) {
        (Some(flavour), Arch::X86_64) => (flavour, Architecture::X86_64),
        (Some(Flavour::Elf), Arch::Aarch64) => (Flavour::Elf, Architecture::Aarch64),
        _ => return Err(Error::Format { triple: target.tuple.to_string() }),
    };
    let flags_of = |kind, after| match machine {
        Architecture::Aarch64 => {
            crate::elf::r_type_aarch64(kind).map(|r_type| RelocationFlags::Elf { r_type })
        }
        _ => flavour.reloc(kind, after),
    };
    let mut obj = Writer::new(flavour.binary(), machine, Endianness::Little);

    // Every section first, because a symbol says which one it is in and a relocation says which one
    // it is written into, so both need the whole list before either can be added.
    let mut made = Vec::with_capacity(input.parts.len());
    for part in &input.parts {
        let id = obj.add_section(Vec::new(), part.name.clone().into_bytes(), part.shape.kind());
        // The flags in full rather than whatever the kind implied, because the kind is a summary of
        // them and the source said them exactly. A section the program wrote `"ax"` on is executable
        // whether or not its name is one this compiler would have made executable. Only where the
        // format has the fields: see [`Flavour::stated`].
        if let Some(flags) = flavour.stated(part.shape) {
            obj.section_mut(id).flags = flags;
        }
        let align = part.align.max(1);
        if part.shape.bits {
            obj.append_section_data(id, &part.bytes, align);
        } else {
            obj.append_section_bss(id, part.size, align);
        }
        made.push(id);
    }

    // Which relocations point at the section a name is in rather than at the name, and which names
    // are then asked for by nothing and left out, before either is written down.
    let defined: std::collections::HashMap<&str, &Name> =
        input.names.iter().map(|name| (name.name.as_str(), name)).collect();
    let onto = |reloc: &Reloc| moved(flavour, input, &defined, reloc);
    let wanted: std::collections::HashSet<&str> = input
        .parts
        .iter()
        .flat_map(|part| &part.relocs)
        .filter(|reloc| onto(reloc).is_none())
        .map(|reloc| reloc.symbol.as_str())
        .collect();

    // Then every name. A relocation names one, and the writer wants the symbol before the
    // relocation that points at it, so this whole pass is in front of the one below.
    let mut symbols = std::collections::BTreeMap::new();
    for name in &input.names {
        if flavour == Flavour::Elf && unseen(name) && !wanted.contains(name.name.as_str()) {
            continue;
        }
        let (section, value, size) = match name.at {
            Held::In { part, offset } => {
                let Some(id) = made.get(part) else {
                    let why = format!(
                        "'{}' is in section {part} and there is no such section",
                        name.name
                    );
                    return Err(Error::Refused { why });
                };
                (SymbolSection::Section(*id), offset, name.size)
            }
            Held::Absolute(value) => (SymbolSection::Absolute, value, name.size),
            // A common symbol says what it wants rather than where it is, and ELF records the
            // boundary it wants where an ordinary symbol records its address.
            Held::Common { size, align } => (SymbolSection::Common, align, size),
            Held::Undefined => (SymbolSection::Undefined, 0, 0),
        };
        let id = obj.add_symbol(Symbol {
            name: name.name.clone().into_bytes(),
            value,
            size,
            kind: flavour.sort(name.sort, name.binding),
            scope: crate::file::scope_of(name.binding),
            weak: name.binding == Binding::Weak,
            section,
            flags: SymbolFlags::None,
        });
        flavour.see(&mut obj, id, name.binding, name.visibility);
        // The writer underneath records a common symbol as `STT_COMMON` and gas records the same
        // symbol as `STT_OBJECT`. Both are a request for storage and a linker reads either, and the
        // one gas writes is written here, because an object that says the same thing a different
        // way is the kind of difference that turns up years later in a tool that only ever saw the
        // other one. A common symbol is global by definition, so there is no binding to preserve.
        if matches!(name.at, Held::Common { .. }) {
            if let SymbolFlags::Elf { st_info, .. } = obj.symbol_flags_mut(id) {
                *st_info = elf::STB_GLOBAL | elf::STT_OBJECT;
            }
        }
        symbols.insert(name.name.clone(), id);
    }

    for (part, id) in input.parts.iter().zip(&made) {
        for reloc in &part.relocs {
            let (symbol, addend) = match onto(reloc) {
                Some((part, offset)) => {
                    (obj.section_symbol(made[part]), reloc.addend + offset as i64)
                }
                None => {
                    let Some(&symbol) = symbols.get(&reloc.symbol) else {
                        let why = format!(
                            "'{}' is named by a relocation and by nothing else",
                            reloc.symbol
                        );
                        return Err(Error::Refused { why });
                    };
                    (symbol, reloc.addend)
                }
            };
            let flags = flags_of(reloc.kind, reloc.after).ok_or_else(|| Error::Refused {
                why: format!("no relocation is {:?}", reloc.kind),
            })?;
            obj.add_relocation(*id, Relocation { offset: reloc.at as u64, symbol, addend, flags })
                .map_err(|why| Error::Refused { why: why.to_string() })?;
        }
    }

    // The debug information, every section before any relocation because a relocation in one of
    // them names another as often as it names a function.
    let mut named = std::collections::HashMap::new();
    for chunk in &info.chunks {
        let id = obj.add_section(Vec::new(), chunk.name.clone().into_bytes(), SectionKind::Debug);
        obj.append_section_data(id, &chunk.bytes, 1);
        named.insert(chunk.name.as_str(), id);
    }
    for chunk in &info.chunks {
        let section = named[chunk.name.as_str()];
        for reloc in &chunk.relocs {
            let (symbol, addend) = match named.get(reloc.symbol.as_str()) {
                Some(&id) => (obj.section_symbol(id), reloc.addend),
                None => match defined.get(reloc.symbol.as_str()).map(|name| name.at) {
                    Some(Held::In { part, offset }) => {
                        (obj.section_symbol(made[part]), reloc.addend + offset as i64)
                    }
                    _ => match symbols.get(&reloc.symbol) {
                        Some(&symbol) => (symbol, reloc.addend),
                        None => {
                            let why = format!(
                                "'{}' is named by the debug information and is not defined here",
                                reloc.symbol
                            );
                            return Err(Error::Refused { why });
                        }
                    },
                },
            };
            let flags = flags_of(reloc.kind, reloc.after).ok_or_else(|| Error::Refused {
                why: format!("no relocation is {:?}", reloc.kind),
            })?;
            let record = Relocation { offset: reloc.at as u64, symbol, addend, flags };
            obj.add_relocation(section, record)
                .map_err(|why| Error::Refused { why: why.to_string() })?;
        }
    }

    // The same marker every other object this compiler writes gets, and for the same reason: a
    // linker that does not find it in every input marks the stack executable. Not a second one if
    // the file already said it, which a file written by hand for a linker that cares often does,
    // and nothing at all on a format whose answer to the question is in the finished image.
    if !input.parts.iter().any(|part| part.name == ".note.GNU-stack") {
        flavour.marker(&mut obj);
    }

    let mut bytes = obj.write().map_err(|why| Error::Refused { why: why.to_string() })?;
    if flavour == Flavour::Elf {
        for part in input.parts.iter().filter(|part| part.shape.merge != 0) {
            entry_size(&mut bytes, &part.name, part.shape.merge);
        }
    }
    Ok(bytes)
}

/// Write how long an entry of a mergeable section is into its header, which the linker needs and
/// the writer underneath has no field for. It writes one only for a section of strings it made
/// itself. The file is a 64 bit little endian ELF one, since that is the only kind this writes, and
/// the section is found by its name, which is unique because the assembler gave every name one
/// section.
fn entry_size(bytes: &mut [u8], name: &str, size: u64) {
    let word = |bytes: &[u8], at: usize, width: usize| {
        bytes[at..at + width].iter().rev().fold(0u64, |sum, &byte| sum << 8 | u64::from(byte))
    };
    let table = word(bytes, 0x28, 8) as usize;
    let each = word(bytes, 0x3a, 2) as usize;
    let count = word(bytes, 0x3c, 2) as usize;
    let names = table + each * word(bytes, 0x3e, 2) as usize;
    let names = word(bytes, names + 0x18, 8) as usize;
    for header in (0..count).map(|nth| table + nth * each) {
        let at = names + word(bytes, header, 4) as usize;
        if bytes[at..].starts_with(name.as_bytes()) && bytes.get(at + name.len()) == Some(&0) {
            bytes[header + 0x38..header + 0x40].copy_from_slice(&size.to_le_bytes());
        }
    }
}

/// The section and the offset into it a relocation is written against in place of the name it
/// gave, when gas would do the same.
///
/// A name only this file can see is a place in a section and nothing more, so gas writes the
/// section's own symbol and how far into it the place is, and a `.L` label then has no reason to be
/// in the table at all. It keeps the name where the linker has to see it: a call, which may go
/// through a stub the linker makes for that name, a slot of the global offset table, and a place in
/// a section the linker may merge, where the offset into the section is not an offset into the
/// merged one. The last of those is only a problem for a distance, or for an address with
/// something added to it, since the address of the start of a string is what the linker follows.
fn moved(
    flavour: Flavour,
    input: &Assembled,
    defined: &std::collections::HashMap<&str, &Name>,
    reloc: &Reloc,
) -> Option<(usize, u64)> {
    use crate::section::Reference;
    let name = defined.get(reloc.symbol.as_str())?;
    let Held::In { part, offset } = name.at else { return None };
    if flavour != Flavour::Elf || name.binding != Binding::Local {
        return None;
    }
    let near = matches!(reloc.kind, Reference::Data | Reference::Away);
    let fixed = match reloc.kind {
        Reference::Call
        | Reference::Got
        | Reference::GotBare
        | Reference::GotKept
        | Reference::Thread => false,
        // The same for a field of an instruction that goes through a stub or a table slot, or that
        // says where a thread-local variable is, which a linker checks against the name's type.
        Reference::Field(
            Fixup::Call26
            | Fixup::Jump26
            | Fixup::GotPage21
            | Fixup::GotLo12
            | Fixup::GotTprelPage21
            | Fixup::GotTprelLo12Nc
            | Fixup::TprelHi12
            | Fixup::TprelLo12Nc,
        ) => false,
        _ if input.parts.get(part)?.shape.merge != 0 => !near && reloc.addend == 0,
        _ => true,
    };
    fixed.then_some((part, offset))
}

/// Whether a name is one the assembler made up or a label only it sees, which gas leaves out of the
/// table unless a relocation still names it. `.L` is the prefix for those that ELF assemblers agree
/// on, and a name with a `\u{1}` in it is one this assembler made for a numbered label or a frame.
fn unseen(name: &Name) -> bool {
    name.binding == Binding::Local
        && (name.name.starts_with(".L")
            || name.name.starts_with("..")
            || name.name.contains('\u{1}'))
}

/// Every name in it a linker can find, which is what an archive's symbol index is built from.
///
/// The same rule as [`crate::defines`]: a local is left out, because a name the static link has
/// already finished with is not one an archive may offer, and an undefined one is left out because
/// this file does not have it.
#[must_use]
pub fn assembled_defines(input: &Assembled) -> Vec<String> {
    input
        .names
        .iter()
        .filter(|name| name.binding != Binding::Local && name.at != Held::Undefined)
        .map(|name| name.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use object::read::elf::{FileHeader as _, Sym as _};
    use object::read::{Object as _, ObjectSection as _, ObjectSymbol as _};
    use object::{RelocationFlags, SectionFlags};
    use rucc_target::{Arch as TargetArch, Env, Os, Triple};

    use crate::section::Reference;

    /// A linux x86-64 target, which is the one most of these are written against.
    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(TargetArch::X86_64, Os::Linux, Env::Gnu))
    }

    /// The same machine under mingw-w64, which is the target the COFF cases below are about.
    fn windows() -> TargetInfo {
        TargetInfo::new(Triple::new(TargetArch::X86_64, Os::Windows, Env::Gnu))
    }

    /// One section of that name holding those bytes, with the flags the name implies.
    fn part(name: &str, bytes: Vec<u8>) -> Part {
        Part {
            name: name.to_owned(),
            size: bytes.len() as u64,
            bytes,
            align: 1,
            shape: Shape::of(name),
            relocs: Vec::new(),
        }
    }

    /// One name at an offset into the first section.
    fn at(name: &str, offset: u64, sort: Sort, binding: Binding) -> Name {
        Name {
            name: name.to_owned(),
            at: Held::In { part: 0, offset },
            size: 0,
            sort,
            binding,
            visibility: Visibility::Default,
        }
    }

    /// The raw `st_info` and `st_value` of a symbol, as the file holds them.
    ///
    /// The reader's own `kind()`, `is_global()` and `address()` are a translation of these, and a
    /// translation is what several of the cases below are about, so they ask the file rather than
    /// the reading. A common symbol is the clearest of them: `address()` gives zero for one because
    /// it has no address, and the field an ordinary symbol keeps its address in is where a common
    /// one states the boundary it has to start on.
    fn raw(bytes: &[u8], want: &str) -> (u8, u64) {
        let header = elf::FileHeader64::<Endianness>::parse(bytes).expect("a header");
        let endian = header.endian().expect("an endianness");
        let table = header.sections(endian, bytes).expect("the sections");
        let symbols = table.symbols(endian, bytes, elf::SHT_SYMTAB).expect("a symbol table");
        for symbol in symbols.iter() {
            if symbols.symbol_name(endian, symbol).expect("a name") == want.as_bytes() {
                return (symbol.st_info().0, symbol.st_value(endian));
            }
        }
        panic!("there is no symbol called '{want}'");
    }

    /// The first half of that.
    fn st_info(bytes: &[u8], want: &str) -> u8 {
        raw(bytes, want).0
    }

    #[test]
    fn a_section_carries_the_flags_the_source_said_and_not_the_ones_its_name_suggests() {
        // The whole reason a shape is separate facts rather than a kind. A program may write
        // `.section .init.text,"ax"` and mean a section with a name this compiler has never heard
        // of, and what it said about it is the letters.
        let mut odd = part(".init.text", vec![0x90]);
        odd.shape = Shape { alloc: true, exec: true, bits: true, ..Shape::default() };
        let input = Assembled { parts: vec![odd], names: Vec::new() };
        let bytes = assembled(&input, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".init.text").expect("the section");
        assert_eq!(section.data().expect("the bytes"), &[0x90]);
        let SectionFlags::Elf { sh_flags, sh_type } = section.flags() else {
            panic!("this is an ELF file");
        };
        assert_eq!(sh_flags.0, elf::SHF_ALLOC.0 | elf::SHF_EXECINSTR.0);
        assert_eq!(sh_flags.0 & elf::SHF_WRITE.0, 0, "nothing said it was writable");
        assert_eq!(sh_type, elf::SHT_PROGBITS);
    }

    #[test]
    fn a_section_that_holds_no_bytes_still_says_how_long_it_is() {
        // `.bss` is a length and no bytes, and a writer that appended its data would produce a file
        // with that much zero in it, which is the difference between an object and a big object.
        let mut room = part(".bss", Vec::new());
        room.size = 4096;
        room.align = 16;
        let input = Assembled { parts: vec![room], names: Vec::new() };
        let bytes = assembled(&input, &target()).expect("an object");
        assert!(bytes.len() < 4096, "the empty space was written out: {} bytes", bytes.len());
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".bss").expect("the section");
        assert_eq!(section.size(), 4096);
        assert_eq!(section.align(), 16);
        let SectionFlags::Elf { sh_type, .. } = section.flags() else { panic!("an ELF file") };
        assert_eq!(sh_type, elf::SHT_NOBITS);
    }

    #[test]
    fn a_label_nobody_stated_a_type_for_is_a_symbol_with_no_type() {
        // `STT_NOTYPE` is what gas writes for one, and it is a real answer rather than a missing
        // one. The writer underneath refuses a defined symbol whose kind is `Unknown` outright, so
        // this is also the case that says the mapping went to `Label` and not there.
        let input = Assembled {
            parts: vec![part(".text", vec![0; 8])],
            names: vec![at("plain", 4, Sort::Untyped, Binding::Global)],
        };
        let bytes = assembled(&input, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let plain = file.symbols().find(|s| s.name() == Ok("plain")).expect("the label");
        assert_eq!(plain.address(), 4);
        assert_eq!(st_info(&bytes, "plain") & 0xf, elf::STT_NOTYPE.0);
    }

    #[test]
    fn what_type_said_is_what_the_symbol_gets() {
        let input = Assembled {
            parts: vec![part(".text", vec![0; 8])],
            names: vec![
                at("run", 0, Sort::Func, Binding::Global),
                at("held", 4, Sort::Object, Binding::Local),
            ],
        };
        let bytes = assembled(&input, &target()).expect("an object");
        assert_eq!(st_info(&bytes, "run") & 0xf, elf::STT_FUNC.0);
        assert_eq!(st_info(&bytes, "held") & 0xf, elf::STT_OBJECT.0);
        assert_eq!(st_info(&bytes, "run") >> 4, elf::STB_GLOBAL.0);
        assert_eq!(st_info(&bytes, "held") >> 4, elf::STB_LOCAL.0);
    }

    #[test]
    fn a_common_symbol_is_written_the_way_gas_writes_one() {
        // The writer underneath records `STT_COMMON` and gas records `STT_OBJECT` for the same
        // `.comm`. Both are a request for storage and a linker takes either, and the one gas writes
        // is the one written here, so an object of ours and an object of theirs do not differ in a
        // field somebody's tool reads years from now.
        let input = Assembled {
            parts: Vec::new(),
            names: vec![Name {
                name: "shared".to_owned(),
                at: Held::Common { size: 8, align: 8 },
                size: 0,
                sort: Sort::Object,
                binding: Binding::Global,
                visibility: Visibility::Default,
            }],
        };
        let bytes = assembled(&input, &target()).expect("an object");
        assert_eq!(st_info(&bytes, "shared"), elf::STB_GLOBAL.0 << 4 | elf::STT_OBJECT.0);
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let shared = file.symbols().find(|s| s.name() == Ok("shared")).expect("the symbol");
        assert!(shared.is_common(), "the linker has to be asked for the space");
        assert_eq!(shared.size(), 8);
        // Where an ordinary symbol keeps its address, which is why the two cannot both be said.
        assert_eq!(raw(&bytes, "shared").1, 8, "the boundary it has to start on");
    }

    #[test]
    fn a_set_is_a_number_rather_than_a_place() {
        let input = Assembled {
            parts: vec![part(".text", vec![0; 8])],
            names: vec![Name {
                name: "size_of_it".to_owned(),
                at: Held::Absolute(25),
                size: 0,
                sort: Sort::Untyped,
                binding: Binding::Global,
                visibility: Visibility::Default,
            }],
        };
        let bytes = assembled(&input, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let sym = file.symbols().find(|s| s.name() == Ok("size_of_it")).expect("the symbol");
        assert_eq!(sym.address(), 25);
        assert_eq!(sym.section(), object::SymbolSection::Absolute, "it is not in any section");
    }

    #[test]
    fn a_relocation_names_a_symbol_and_lands_where_the_bytes_are() {
        let mut data = part(".data", vec![0; 8]);
        data.relocs.push(Reloc {
            at: 0,
            symbol: "message".to_owned(),
            kind: Reference::Address { bytes: 8 },
            addend: 0,
            after: 0,
        });
        let input = Assembled {
            parts: vec![data],
            names: vec![Name {
                name: "message".to_owned(),
                at: Held::Undefined,
                size: 0,
                sort: Sort::Untyped,
                binding: Binding::Global,
                visibility: Visibility::Default,
            }],
        };
        let bytes = assembled(&input, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".data").expect("the section");
        let (at, reloc) = section.relocations().next().expect("one relocation");
        assert_eq!(at, 0);
        assert_eq!(reloc.addend(), 0);
        let RelocationFlags::Elf { r_type } = reloc.flags() else { panic!("an ELF file") };
        assert_eq!(r_type, elf::R_X86_64_64);
    }

    #[test]
    fn a_place_only_this_file_sees_is_reached_through_its_section_as_gas_does() {
        // The `.L` label goes, the static function stays in the table, and both relocations are
        // against `.text` at their offsets. A call keeps its name, since the linker may give it a
        // stub, and so does a name the linker is allowed to see.
        let mut text = part(".text", vec![0; 32]);
        for (at, symbol, kind) in [
            (0, ".L3", Reference::Data),
            (4, "helper", Reference::Data),
            (8, "helper", Reference::Call),
            (12, "shared", Reference::Data),
        ] {
            let symbol = symbol.to_owned();
            text.relocs.push(Reloc { at, symbol, kind, addend: -4, after: 0 });
        }
        let input = Assembled {
            parts: vec![text],
            names: vec![
                at(".L3", 20, Sort::Untyped, Binding::Local),
                at("helper", 24, Sort::Func, Binding::Local),
                at("shared", 28, Sort::Func, Binding::Global),
            ],
        };
        let bytes = assembled(&input, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let names: Vec<_> = file.symbols().filter_map(|sym| sym.name().ok()).collect();
        assert!(!names.contains(&".L3") && names.contains(&"helper"), "{names:?}");
        let section = file.section_by_name(".text").expect("the section");
        let reached: Vec<_> = section
            .relocations()
            .map(|(at, reloc)| {
                let object::RelocationTarget::Symbol(index) = reloc.target() else {
                    panic!("a symbol")
                };
                let symbol = file.symbol_by_index(index).expect("the symbol");
                let name = if symbol.kind() == object::SymbolKind::Section {
                    ".text"
                } else {
                    symbol.name().expect("a name")
                };
                (at, name, reloc.addend())
            })
            .collect();
        assert_eq!(
            reached,
            [(0, ".text", 16), (4, ".text", 20), (8, "helper", -4), (12, "shared", -4)]
        );
    }

    #[test]
    fn a_section_of_constants_may_be_merged_and_a_distance_into_it_keeps_its_name() {
        let mut text = part(".text", vec![0; 8]);
        text.relocs.push(Reloc {
            at: 0,
            symbol: ".LC0".to_owned(),
            kind: Reference::Data,
            addend: -4,
            after: 0,
        });
        let strings = Part {
            shape: Shape { merge: 1, strings: true, ..Shape::of(".rodata") },
            ..part(".rodata.str1.1", b"hi\0".to_vec())
        };
        let mut name = at(".LC0", 0, Sort::Untyped, Binding::Local);
        name.at = Held::In { part: 1, offset: 0 };
        let input = Assembled { parts: vec![text, strings], names: vec![name] };
        let bytes = assembled(&input, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".rodata.str1.1").expect("the section");
        let SectionFlags::Elf { sh_flags, .. } = section.flags() else { panic!("an ELF file") };
        assert_eq!(sh_flags.0, elf::SHF_ALLOC.0 | elf::SHF_MERGE.0 | elf::SHF_STRINGS.0);
        let header = elf::FileHeader64::<Endianness>::parse(&bytes[..]).expect("a header");
        let endian = header.endian().expect("an endianness");
        let table = header.sections(endian, &bytes[..]).expect("the sections");
        let (_, found) = table.section_by_name(endian, b".rodata.str1.1").expect("the section");
        assert_eq!(found.sh_entsize.get(endian), 1);
        let text = file.section_by_name(".text").expect("the section");
        let (_, reloc) = text.relocations().next().expect("one relocation");
        let object::RelocationTarget::Symbol(index) = reloc.target() else { panic!("a symbol") };
        assert_eq!(file.symbol_by_index(index).and_then(|sym| sym.name()), Ok(".LC0"));
    }

    #[test]
    fn a_relocation_against_a_name_the_file_never_mentions_is_refused() {
        // Rather than written against symbol zero, which is a file that links and resolves the
        // reference to address zero. The list of names is the whole of what the reader found, so a
        // relocation naming something outside it is a mistake in this compiler.
        let mut data = part(".data", vec![0; 8]);
        data.relocs.push(Reloc {
            at: 0,
            symbol: "nowhere".to_owned(),
            kind: Reference::Address { bytes: 8 },
            addend: 0,
            after: 0,
        });
        let input = Assembled { parts: vec![data], names: Vec::new() };
        let why = assembled(&input, &target()).expect_err("this cannot be written");
        assert!(format!("{why}").contains("nowhere"), "{why}");
    }

    #[test]
    fn the_stack_is_marked_once_whoever_asked_for_it() {
        // A linker that does not find this marker in every input marks the stack executable, and a
        // file written by hand for one that cares often says it itself.
        let bare = Assembled { parts: vec![part(".text", vec![0x90])], names: Vec::new() };
        let bytes = assembled(&bare, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert!(file.section_by_name(".note.GNU-stack").is_some(), "the marker was left out");

        let said = Assembled {
            parts: vec![part(".text", vec![0x90]), part(".note.GNU-stack", Vec::new())],
            names: Vec::new(),
        };
        let bytes = assembled(&said, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let marks = file.sections().filter(|s| s.name() == Ok(".note.GNU-stack")).count();
        assert_eq!(marks, 1, "the file said it and it was said again");
    }

    #[test]
    fn only_the_names_a_linker_could_find_are_offered_to_an_archive() {
        let input = Assembled {
            parts: vec![part(".text", vec![0; 8])],
            names: vec![
                at("reachable", 0, Sort::Func, Binding::Global),
                at("mine", 4, Sort::Func, Binding::Local),
                Name {
                    name: "elsewhere".to_owned(),
                    at: Held::Undefined,
                    size: 0,
                    sort: Sort::Untyped,
                    binding: Binding::Global,
                    visibility: Visibility::Default,
                },
            ],
        };
        assert_eq!(assembled_defines(&input), vec!["reachable".to_owned()]);
    }

    #[test]
    fn a_machine_this_does_not_write_is_refused_rather_than_written_wrong() {
        let input = Assembled { parts: vec![part(".text", vec![0x90])], names: Vec::new() };
        let elsewhere = TargetInfo::new(Triple::new(TargetArch::Aarch64, Os::Windows, Env::Msvc));
        let why = assembled(&input, &elsewhere).expect_err("this cannot be written");
        assert!(format!("{why}").contains("aarch64"), "{why}");
    }

    #[test]
    fn a_file_of_assembly_for_aarch64_is_written_with_that_machine_s_relocations() {
        // `adrp x0, table` and `add x0, x0, :lo12:table+8`, then `bl g`, then the address of
        // `table` in a table of its own. A field is its fixup's relocation and an address is the
        // AArch64 one of that width, and a label only this file sees is written against its
        // section, the way gas writes it.
        let mut text = part(".text", vec![0; 12]);
        let field = |at, symbol: &str, fixup, addend| Reloc {
            at,
            symbol: symbol.to_owned(),
            kind: Reference::Field(fixup),
            addend,
            after: 0,
        };
        text.relocs = vec![
            field(0, ".Ltable", Fixup::AdrPage21, 8),
            field(4, ".Ltable", Fixup::AddLo12, 8),
            field(8, "g", Fixup::Call26, 0),
        ];
        let mut data = part(".data", vec![0; 16]);
        data.relocs = vec![Reloc {
            at: 8,
            symbol: ".Ltable".to_owned(),
            kind: Reference::Address { bytes: 8 },
            addend: 0,
            after: 0,
        }];
        let mut table = at(".Ltable", 0, Sort::Object, Binding::Local);
        table.at = Held::In { part: 1, offset: 0 };
        let input = Assembled {
            parts: vec![text, data],
            names: vec![
                table,
                Name { at: Held::Undefined, ..at("g", 0, Sort::Untyped, Binding::Global) },
            ],
        };
        let target = TargetInfo::new(Triple::new(TargetArch::Aarch64, Os::Linux, Env::Gnu));
        let bytes = assembled(&input, &target).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert_eq!(file.architecture(), Architecture::Aarch64);
        let relocs = |name: &str| -> Vec<(u64, elf::RelocationType, i64)> {
            let section = file.section_by_name(name).expect("the section");
            section
                .relocations()
                .map(|(at, reloc)| {
                    let RelocationFlags::Elf { r_type } = reloc.flags() else { panic!("ELF") };
                    (at, r_type, reloc.addend())
                })
                .collect()
        };
        assert_eq!(
            relocs(".text"),
            [
                (0, elf::R_AARCH64_ADR_PREL_PG_HI21, 8),
                (4, elf::R_AARCH64_ADD_ABS_LO12_NC, 8),
                (8, elf::R_AARCH64_CALL26, 0)
            ]
        );
        assert_eq!(relocs(".data"), [(8, elf::R_AARCH64_ABS64, 0)]);
        assert!(file.symbols().all(|s| s.name() != Ok(".Ltable")), "a label only this file sees");
    }

    #[test]
    fn a_file_of_assembly_for_windows_is_written_as_coff() {
        // What tamnd/rucc#1514 was about. `runtime/builtins/chkstk.S` is a file of assembly for a
        // Windows target, and until this it was refused with a message about there being no object
        // writer for the triple, which read as the whole back end being missing rather than this
        // one path through it.
        let input = Assembled { parts: vec![part(".text", vec![0xc3])], names: Vec::new() };
        let bytes = assembled(&input, &windows()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert_eq!(file.format(), object::BinaryFormat::Coff);
        let section = file.section_by_name(".text").expect("the section");
        assert_eq!(section.data().expect("the bytes"), &[0xc3]);
        assert_eq!(section.kind(), SectionKind::Text);
        assert!(
            file.section_by_name(".note.GNU-stack").is_none(),
            "a format with no marker got one anyway"
        );
    }

    #[test]
    fn a_global_label_with_no_type_under_it_is_still_offered_on_coff() {
        // The case a `.globl` and a label is, which is most of what a hand written file says. On
        // ELF that is `STT_NOTYPE` and the binding is a separate field, so the name is global
        // whatever its type. COFF has no such split: what the writer underneath calls a label is
        // storage class `LABEL`, which is a name inside one file, and a symbol written that way is
        // one no linker resolves against. `___chkstk_ms` came out of the archive as a local under
        // that mapping and mingw-w64's own objects went on wanting it.
        let input = Assembled {
            parts: vec![part(".text", vec![0; 8])],
            names: vec![
                at("offered", 0, Sort::Untyped, Binding::Global),
                at("ours", 4, Sort::Untyped, Binding::Local),
            ],
        };
        let bytes = assembled(&input, &windows()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let offered = file.symbols().find(|s| s.name() == Ok("offered")).expect("the label");
        assert!(offered.is_global(), "a `.globl` label came out local");
        let ours = file.symbols().find(|s| s.name() == Ok("ours")).expect("the other label");
        assert!(!ours.is_global(), "a label nothing offered came out global");
        // And the same input on ELF is still what gas writes there, which is the half of this that
        // would otherwise have been changed to fix the other half.
        let bytes = assembled(&input, &target()).expect("an object");
        assert_eq!(st_info(&bytes, "offered") & 0xf, elf::STT_NOTYPE.0);
    }

    #[test]
    fn a_relocation_on_coff_says_how_much_of_the_instruction_comes_after_it() {
        // The one real difference between the two formats' relocations. ELF folds the distance
        // between the hole and the end of the instruction into the addend and has one type. COFF
        // counts from the end of the instruction and has no addend field, so the count is in the
        // type: `IMAGE_REL_AMD64_REL32_4` is four bytes of immediate behind the displacement.
        let mut text = part(".text", vec![0; 16]);
        text.relocs.push(Reloc {
            at: 2,
            symbol: "elsewhere".to_owned(),
            kind: Reference::Data,
            addend: -8,
            after: 4,
        });
        let input = Assembled {
            parts: vec![text],
            names: vec![Name {
                name: "elsewhere".to_owned(),
                at: Held::Undefined,
                size: 0,
                sort: Sort::Untyped,
                binding: Binding::Global,
                visibility: Visibility::Default,
            }],
        };
        let bytes = assembled(&input, &windows()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".text").expect("the section");
        let (at, reloc) = section.relocations().next().expect("the relocation");
        assert_eq!(at, 2);
        assert_eq!(
            reloc.flags(),
            RelocationFlags::Coff {
                typ: object::pe::RelocationType(object::pe::IMAGE_REL_AMD64_REL32.0 + 4)
            }
        );
    }
}
