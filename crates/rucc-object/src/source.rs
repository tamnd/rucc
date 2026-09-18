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
//! The two views meet at the [`object`] crate's writer, which is what both call, so there is one
//! place that knows how an ELF file is laid out.

use object::write::{Object as Writer, Relocation, Symbol, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, RelocationFlags, SectionFlags, SectionKind,
    SymbolFlags, SymbolKind, elf,
};
use rucc_target::{ObjectFormat, TargetInfo};
use rucc_tuple::Arch;

use crate::file::Error;
use crate::section::{Array, Binding, Reloc, Visibility};

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

/// That, as a relocatable ELF object.
///
/// ELF only, where the rest of this crate writes COFF as well. What a [`Part`] carries is the
/// section type and flags the source wrote in as many words, which are ELF's and which a COFF
/// section header has no field for, so a file of assembly for a Windows target is refused here
/// rather than written with the flags guessed back from the name.
///
/// # Errors
///
/// [`Error::Format`] for a machine or a platform this does not write, and [`Error::Refused`] for a
/// relocation against a name the list does not hold or one this machine has no relocation for.
pub fn assembled(input: &Assembled, target: &TargetInfo) -> Result<Vec<u8>, Error> {
    if target.tuple.arch() != Arch::X86_64 || target.object_format != ObjectFormat::Elf {
        return Err(Error::Format { triple: target.tuple.to_string() });
    }
    let mut obj = Writer::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);

    // Every section first, because a symbol says which one it is in and a relocation says which one
    // it is written into, so both need the whole list before either can be added.
    let mut made = Vec::with_capacity(input.parts.len());
    for part in &input.parts {
        let id = obj.add_section(Vec::new(), part.name.clone().into_bytes(), part.shape.kind());
        // The flags in full rather than whatever the kind implied, because the kind is a summary of
        // them and the source said them exactly. A section the program wrote `"ax"` on is executable
        // whether or not its name is one this compiler would have made executable.
        obj.section_mut(id).flags =
            SectionFlags::Elf { sh_type: part.shape.sh_type(), sh_flags: part.shape.sh_flags() };
        let align = part.align.max(1);
        if part.shape.bits {
            obj.append_section_data(id, &part.bytes, align);
        } else {
            obj.append_section_bss(id, part.size, align);
        }
        made.push(id);
    }

    // Then every name. A relocation names one, and the writer wants the symbol before the
    // relocation that points at it, so this whole pass is in front of the one below.
    let mut symbols = std::collections::BTreeMap::new();
    for name in &input.names {
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
            kind: sort_of(name.sort),
            scope: crate::file::scope_of(name.binding),
            weak: name.binding == Binding::Weak,
            section,
            flags: SymbolFlags::None,
        });
        crate::elf::see(&mut obj, id, name.binding, name.visibility);
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
            let Some(symbol) = symbols.get(&reloc.symbol) else {
                let why =
                    format!("'{}' is named by a relocation and by nothing else", reloc.symbol);
                return Err(Error::Refused { why });
            };
            let r_type = crate::elf::r_type(reloc.kind).ok_or_else(|| Error::Refused {
                why: format!("no relocation is {:?}", reloc.kind),
            })?;
            obj.add_relocation(
                *id,
                Relocation {
                    offset: reloc.at as u64,
                    symbol: *symbol,
                    addend: reloc.addend,
                    flags: RelocationFlags::Elf { r_type },
                },
            )
            .map_err(|why| Error::Refused { why: why.to_string() })?;
        }
    }

    // The same marker every other object this compiler writes gets, and for the same reason: a
    // linker that does not find it in every input marks the stack executable. Not a second one if
    // the file already said it, which a file written by hand for a linker that cares often does.
    if !input.parts.iter().any(|part| part.name == ".note.GNU-stack") {
        obj.add_section(Vec::new(), b".note.GNU-stack".to_vec(), SectionKind::Metadata);
    }

    obj.write().map_err(|why| Error::Refused { why: why.to_string() })
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

/// What the writer underneath calls one of these.
///
/// `Label` is the one that is not obvious from its name. It is what that writer turns into
/// `STT_NOTYPE`, which is what gas records for a label nobody stated a type for, and it says
/// nothing about whether the name is local: `Unknown` would have been the reading of the name, and
/// that writer refuses a defined one of those outright.
fn sort_of(sort: Sort) -> SymbolKind {
    match sort {
        Sort::Func => SymbolKind::Text,
        Sort::Object => SymbolKind::Data,
        Sort::Thread => SymbolKind::Tls,
        Sort::File => SymbolKind::File,
        Sort::Untyped => SymbolKind::Label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use object::read::elf::{FileHeader as _, Sym as _};
    use object::read::{Object as _, ObjectSection as _, ObjectSymbol as _};
    use rucc_target::{Arch as TargetArch, Env, Os, Triple};

    use crate::section::Reference;

    /// A linux x86-64 target, which is the only one this writes.
    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(TargetArch::X86_64, Os::Linux, Env::Gnu))
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
        let elsewhere = TargetInfo::new(Triple::new(TargetArch::Aarch64, Os::Linux, Env::Gnu));
        let why = assembled(&input, &elsewhere).expect_err("this cannot be written");
        assert!(format!("{why}").contains("aarch64"), "{why}");
    }
}
