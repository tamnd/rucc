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

use object::write::{Object as Writer, Relocation, SectionId, Symbol, SymbolSection};
use object::{
    Architecture, Endianness, SectionFlags, SectionKind, SymbolFlags, SymbolKind, SymbolScope, elf,
    pe,
};
use rucc_base::hash::{Map, Set};
use rucc_target::aarch64::Fixup;
use rucc_target::{ObjectFormat, TargetInfo};
use rucc_tuple::Arch;

use crate::file::{Error, Flavour};
use crate::section::{Array, Binding, Compress, Info, Reloc, Visibility};

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
    /// The COMDAT it is, on COFF, where `.section name,"flags",discard,symbol` makes a section one
    /// the linker keeps a single copy of out of every object that has one about the same symbol,
    /// and the section group it is in on ELF, where `.section name,"axG",@progbits,symbol,comdat`
    /// says the same. [`None`] for every other section and on Mach-O.
    pub group: Option<Group>,
    /// The name whose section this one goes with, on ELF, which is what the `o` flag and the
    /// operand after the type say. The linker keeps or drops the two together and lays this one out
    /// in the order of the other, which is what a record of where a patcher's room is in front of
    /// a function needs. [`None`] for every other section.
    pub link: Option<String>,
}

/// A section the linker keeps one copy of, which is what the third and fourth operands of
/// `.section` say on COFF and the `G` flag and the operands after the type say on ELF. Every
/// section of one ELF object that names the same symbol is in the one group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The name the group is about, which the file defines in the section.
    pub symbol: String,
    /// Which copy the linker keeps.
    pub keep: Keep,
}

/// How the linker picks the copy of a [`Group`] it keeps, in the words gas and llvm-mc take for
/// each of COFF's selection numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// `one_only`: there must be only one, and two is an error.
    One,
    /// `discard`: any one, and the rest are dropped. What a `.refptr.` pointer is.
    Any,
    /// `same_size`: any one, and two of different sizes are an error.
    SameSize,
    /// `same_contents`: any one, and two with different bytes are an error.
    SameContents,
    /// `largest`: the biggest one.
    Largest,
    /// `newest`: the newest one, which no toolchain writes and link.exe does not implement.
    Newest,
    /// A group that is not a COMDAT, on ELF, which is `G` without `comdat` after the symbol. The
    /// linker keeps or drops its sections together and keeps every copy of them.
    Together,
}

impl Keep {
    /// The word for it, as `.section` spells it.
    #[must_use]
    pub fn of(word: &str) -> Option<Keep> {
        Some(match word {
            "one_only" => Keep::One,
            "discard" => Keep::Any,
            "same_size" => Keep::SameSize,
            "same_contents" => Keep::SameContents,
            "largest" => Keep::Largest,
            "newest" => Keep::Newest,
            _ => return None,
        })
    }

    pub(crate) const fn kind(self) -> object::ComdatKind {
        match self {
            Keep::One => object::ComdatKind::NoDuplicates,
            Keep::Any => object::ComdatKind::Any,
            Keep::SameSize => object::ComdatKind::SameSize,
            Keep::SameContents => object::ComdatKind::ExactMatch,
            Keep::Largest => object::ComdatKind::Largest,
            Keep::Newest => object::ComdatKind::Newest,
            // The writer underneath writes every ELF group as a COMDAT, and the word that says so
            // is cleared afterwards. See [`together`].
            Keep::Together => object::ComdatKind::Any,
        }
    }
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
    /// `@note`: the bytes are notes for a loader or a tool to find by type, such as the build ID
    /// and the entry point notes a boot loader reads out of the kernel. Only means anything beside
    /// `bits`, and only on ELF, whose section type says it.
    pub note: bool,
    /// `R`: the linker keeps the section when it drops the ones nothing refers to, which is what
    /// `__attribute__((retain))` asks for on ELF.
    pub retain: bool,
    /// The type and attributes of a Mach-O section, in the one word the format keeps them in,
    /// which is what [`Shape::mach`] works out. Zero on the other two formats, where the fields
    /// above are the whole answer, and zero is also an ordinary Mach-O section with nothing said.
    pub mach: u32,
    /// The characteristics of a COFF section whose `.section` gave COFF's own letters, which is
    /// what [`Shape::coff`] works out. Zero when it gave none, and then the writer works them out
    /// from the name and the kind, as it does for a section this compiler made.
    pub coff: u32,
}

impl Shape {
    /// What a section of this name is when the source named it and said nothing else.
    ///
    /// `.text`, `.data` and the rest are names an assembler already knows the flags of, which is
    /// why a program may write `.data` on its own and why `.section .data` without letters is the
    /// same section rather than an unallocated one. A name nothing here knows gets the flags of an
    /// ordinary allocated writable section. An ELF `.section` in a file of assembly reads
    /// [`Shape::unflagged`] instead, which is gas's table.
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

    /// What gas makes a section of this name on ELF when `.section` gives it no letters, and the
    /// type it gives it when the letters come with no type.
    ///
    /// The names are binutils' table of special sections. Most of them hold for the name and for
    /// any name that adds a dot and more to it, so `.text.foo` is code and `.textfoo` is not, a
    /// few hold only for the name itself, and `.note` holds for anything that starts with it.
    /// Everything else is bytes with no flags at all, which is what the kernel's
    /// `.pushsection .discard.ibt_endbr_noseal` and the like are, and which [`Shape::of`] used to
    /// make writable data.
    #[must_use]
    pub fn unflagged(name: &str) -> Shape {
        let bits = Shape { bits: true, ..Shape::default() };
        let family = |head: &str| {
            name.strip_prefix(head).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        };
        let alloc = Shape { alloc: true, ..bits };
        if family(".text") || name == ".init" || name == ".fini" {
            Shape { exec: true, ..alloc }
        } else if family(".data") || name == ".data1" {
            Shape { write: true, ..alloc }
        } else if family(".rodata") || name == ".rodata1" {
            alloc
        } else if family(".bss") || family(".gnu.linkonce.b") || family(".noinit") {
            Shape { write: true, bits: false, ..alloc }
        } else if family(".persistent") {
            Shape { write: true, ..alloc }
        } else if family(".tbss") {
            Shape { write: true, thread: true, bits: false, ..alloc }
        } else if family(".tdata") {
            Shape { write: true, thread: true, ..alloc }
        } else if let Some(array) = Array::of(name) {
            Shape { write: true, array: Some(array), ..alloc }
        } else if name.starts_with(".note") && name != ".note.GNU-stack" {
            Shape { note: true, ..bits }
        } else {
            bits
        }
    }

    /// The flags a section of this name has whatever letters the source gave it.
    ///
    /// gas adds these to the letters rather than taking the letters alone, so
    /// `.section .data.rel.ro.local,"a"` is writable all the same. GMP names its jump tables that
    /// way, and a linker making a position independent program refuses an address it would have to
    /// fix up in a section it may not write. Only the names gas treats as a family are here, which
    /// is fewer than [`Shape::of`] knows: `.init.data` is not executable just because `.init` is.
    #[must_use]
    pub fn implied(name: &str) -> Shape {
        let base = Shape { alloc: true, ..Shape::default() };
        let head = name.split_once('.').map_or(name, |(_, rest)| rest);
        let head = head.split_once('.').map_or(head, |(first, _)| first);
        match head {
            "text" => Shape { exec: true, ..base },
            "rodata" => base,
            "data" | "bss" => Shape { write: true, ..base },
            "tdata" | "tbss" => Shape { write: true, thread: true, ..base },
            _ if Array::of(name).is_some() => Shape { write: true, ..base },
            _ => Shape::default(),
        }
    }

    /// What a Mach-O section is, from its segment, its section and the type and attributes a
    /// `.section` directive gave after them.
    ///
    /// The word the format keeps is the answer and the fields beside it are filled in from it, so
    /// that what reads a shape without knowing the format still sees code as code and a zero
    /// filled section as one that holds no bytes.
    ///
    /// # Errors
    ///
    /// A type or an attribute Apple's assembler does not take, as a sentence.
    pub fn mach(
        segment: &str,
        section: &str,
        kind: Option<&str>,
        attributes: &[&str],
    ) -> Result<Shape, String> {
        let mach = crate::macho::section_flags(segment, section, kind, attributes)?;
        let exec = mach & object::macho::S_ATTR_PURE_INSTRUCTIONS.0 != 0;
        let typ = object::macho::SectionFlags(mach).typ();
        let thread = matches!(
            typ,
            object::macho::S_THREAD_LOCAL_REGULAR | object::macho::S_THREAD_LOCAL_ZEROFILL
        );
        Ok(Shape {
            alloc: true,
            write: segment != "__TEXT",
            exec,
            thread,
            bits: !crate::macho::zero_filled(mach),
            mach,
            ..Shape::default()
        })
    }

    /// What a COFF section is, from the letters after its name in `.section`.
    ///
    /// COFF's letters are not ELF's, and `d`, `r` and `n` mean nothing to ELF at all. These are
    /// read the way llvm-mc reads them, which is also how gas reads them: `x` is code, `d` is data,
    /// `b` is zero filled, `r` takes away writing and `w` gives it back, `n` is a section the
    /// linker drops, `i` holds options for the linker, `y` is not readable, `D` may be discarded,
    /// `s` is shared, and `a` is taken and means nothing. A section with no letters at all is
    /// readable and writable data. `.drectve,"yni"` is the one a DLL's exports are said in.
    ///
    /// # Errors
    ///
    /// A letter that is not one of those, as the letter.
    pub fn coff(letters: &str) -> Result<Shape, char> {
        let (mut code, mut data, mut zero, mut drop, mut info) =
            (false, false, false, false, false);
        let (mut read, mut write, mut shared, mut discard) = (true, true, false, false);
        let mut writable = false;
        for letter in letters.chars() {
            match letter {
                'a' => {}
                'b' => zero = true,
                'd' => {
                    data = true;
                    write = true;
                }
                'n' => drop = true,
                'D' => discard = true,
                'r' => {
                    writable = false;
                    write = false;
                    data |= !code;
                }
                's' => {
                    shared = true;
                    data = true;
                    write = true;
                }
                'w' => {
                    write = true;
                    writable = true;
                }
                'x' => {
                    code = true;
                    write &= writable;
                }
                'y' => {
                    read = false;
                    write = false;
                }
                'i' => info = true,
                other => return Err(other),
            }
        }
        let mut flags = 0;
        if code {
            flags |= pe::IMAGE_SCN_CNT_CODE.0 | pe::IMAGE_SCN_MEM_EXECUTE.0;
        }
        if data {
            flags |= pe::IMAGE_SCN_CNT_INITIALIZED_DATA.0;
        }
        if zero && !data {
            flags |= pe::IMAGE_SCN_CNT_UNINITIALIZED_DATA.0;
        }
        if drop {
            flags |= pe::IMAGE_SCN_LNK_REMOVE.0;
        }
        if read {
            flags |= pe::IMAGE_SCN_MEM_READ.0;
        }
        if write {
            flags |= pe::IMAGE_SCN_MEM_WRITE.0;
        }
        if discard {
            flags |= pe::IMAGE_SCN_MEM_DISCARDABLE.0;
        }
        if shared {
            flags |= pe::IMAGE_SCN_MEM_SHARED.0;
        }
        if info {
            flags |= pe::IMAGE_SCN_LNK_INFO.0;
        }
        Ok(Shape {
            alloc: !drop && !info,
            write,
            exec: code,
            bits: !(zero && !data),
            coff: flags,
            ..Shape::default()
        })
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
        if self.retain {
            flags |= elf::SHF_GNU_RETAIN.0;
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
            _ if self.note => elf::SHT_NOTE,
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
    /// `@gnu_indirect_function`, ELF's `STT_GNU_IFUNC`. The name is at a resolver rather than at
    /// the function: the dynamic loader calls what is there once, before anything else can, and
    /// the address it hands back is what every call and every pointer to the name then reaches.
    ///
    /// That makes it a name nothing in this file may be worked out against, because the place it
    /// marks is not where a call to it goes. A reference to one stays a relocation against the
    /// name even when the name is local and in the same section, where any other name would have
    /// been turned into the section and an offset. See `moved`.
    Ifunc,
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
    /// Whether the file said `.subsections_via_symbols`, which tells a Mach-O linker it may cut
    /// every section at every symbol in it. Nothing on the other two formats.
    pub subsections: bool,
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
    // x86-64, AArch64 and i386 on ELF and COFF, and AArch64 on Mach-O, which is a function
    // of its own since what it answers differently is most of what is below.
    let (flavour, machine) = match Flavour::of(target) {
        Some(flavour) => match flavour.machine(target.tuple.arch()) {
            Some(machine) => (flavour, machine),
            None => return Err(Error::Format { triple: target.tuple.to_string() }),
        },
        None if target.tuple.arch() == Arch::Aarch64
            && target.object_format == ObjectFormat::MachO =>
        {
            return crate::macho::write(input, target, info);
        }
        None => return Err(Error::Format { triple: target.tuple.to_string() }),
    };
    let flags_of = |kind, after| flavour.reloc(machine, kind, after);
    // Whether the addend of a field of an instruction goes into the field rather than into the
    // relocation, which is what a format without addends does. See [`crate::coff::carry`].
    let carried = machine == Architecture::Aarch64 && flavour == Flavour::Coff;
    let mut obj = Writer::new(flavour.binary(), machine, Endianness::Little);
    // A name in a file of assembly is already the name in the object, underscore and all, which the
    // writer underneath would otherwise put a second one on for COFF on i386.
    obj.set_mangling(object::write::Mangling::None);

    // Every section first, because a symbol says which one it is in and a relocation says which one
    // it is written into, so both need the whole list before either can be added.
    let mut made = Vec::with_capacity(input.parts.len());
    for part in &input.parts {
        let id = obj.add_section(Vec::new(), part.name.clone().into_bytes(), part.shape.kind());
        // The flags in full rather than whatever the kind implied, because the kind is a summary of
        // them and the source said them exactly. A section the program wrote `"ax"` on is executable
        // whether or not its name is one this compiler would have made executable. Only where the
        // format has the fields: see [`Flavour::stated`].
        if let Some(mut flags) = flavour.stated(part.shape) {
            // A member of a group says so in its own flags on ELF, which the writer underneath
            // leaves to whoever states them.
            if let SectionFlags::Elf { sh_flags, .. } = &mut flags {
                if part.group.is_some() {
                    sh_flags.0 |= elf::SHF_GROUP.0;
                }
                if part.link.is_some() {
                    sh_flags.0 |= elf::SHF_LINK_ORDER.0;
                }
            }
            obj.section_mut(id).flags = flags;
        }
        let align = part.align.max(1);
        if part.shape.bits {
            obj.append_section_data(id, &part.bytes, align);
        } else {
            obj.append_section_bss(id, part.size, align);
        }
        // A COMDAT's section symbol comes before the symbol the group is about, which is the
        // order the COFF writer wants the two in, so it is asked for here and not left to the
        // first relocation that happens to need it.
        if part.group.is_some() && flavour == Flavour::Coff {
            obj.section_symbol(id);
        }
        made.push(id);
    }

    // Which relocations point at the section a name is in rather than at the name, and which names
    // are then asked for by nothing and left out, before either is written down.
    let defined: Map<&str, &Name> =
        input.names.iter().map(|name| (name.name.as_str(), name)).collect();
    // COFF for i386 has temporary names of its own. See [`unseen`].
    let pe32 = flavour == Flavour::Coff && machine == Architecture::I386;
    let onto = |reloc: &Reloc| moved(flavour, pe32, input, &defined, reloc);
    let wanted: Set<&str> = input
        .parts
        .iter()
        .flat_map(|part| &part.relocs)
        .filter(|reloc| onto(reloc).is_none())
        .map(|reloc| reloc.symbol.as_str())
        .chain(
            input
                .parts
                .iter()
                .filter_map(|part| part.group.as_ref())
                .map(|group| group.symbol.as_str()),
        )
        .collect();

    // Then every name. A relocation names one, and the writer wants the symbol before the
    // relocation that points at it, so this whole pass is in front of the one below.
    let mut symbols = std::collections::BTreeMap::new();
    for name in &input.names {
        let dropped = flavour == Flavour::Elf || pe32;
        if dropped && unseen(name, pe32) && !wanted.contains(name.name.as_str()) {
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
        // A local number on COFF is a static symbol, which is what gas and clang write for a `.set`
        // such as `@feat.00`. The writer underneath would make it a label otherwise.
        let kind = match (flavour, name.at, name.sort) {
            (Flavour::Coff, Held::Absolute(_), Sort::Untyped) => SymbolKind::Data,
            _ => flavour.sort(name.sort, name.binding),
        };
        let id = obj.add_symbol(Symbol {
            name: name.name.clone().into_bytes(),
            value,
            size,
            kind,
            scope: crate::file::scope_of(name.binding),
            weak: name.binding == Binding::Weak,
            section,
            flags: SymbolFlags::None,
        });
        flavour.see(&mut obj, id, name.binding, name.visibility);
        if name.sort == Sort::Ifunc {
            // Only ELF has the type. COFF and Mach-O reach a function chosen at load time through
            // a pointer the program fills in itself, which is a different program rather than a
            // different symbol, so a listing asking for one there is refused rather than written
            // as the ordinary function it is not.
            if flavour != Flavour::Elf {
                let why = format!("'{}' is an indirect function, which only ELF has", name.name);
                return Err(Error::Refused { why });
            }
            crate::elf::indirect(&mut obj, id);
        }
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

    // One group for every symbol on ELF, holding each section that named it, and one for every
    // section on COFF, where a COMDAT is a section and the symbol it is about.
    let mut groups: Vec<(&Group, Vec<SectionId>)> = Vec::new();
    let mut grouped: Map<&str, usize> = Map::default();
    for (part, id) in input.parts.iter().zip(&made) {
        let Some(group) = &part.group else { continue };
        match grouped.get(group.symbol.as_str()) {
            Some(&at) if flavour == Flavour::Elf => groups[at].1.push(*id),
            _ => {
                grouped.insert(group.symbol.as_str(), groups.len());
                groups.push((group, vec![*id]));
            }
        }
    }
    let mut together = Vec::new();
    for (group, sections) in groups {
        let symbol = match symbols.get(&group.symbol) {
            Some(&symbol) => symbol,
            // ELF only wants the name of a group, and a group about a name the file does not
            // define gets a local one of its own. llvm-mc puts it in the group's own section,
            // which the writer underneath has no handle on, and the first member is as good a
            // place, since nothing reads where it is.
            None if flavour == Flavour::Elf => obj.add_symbol(Symbol {
                name: group.symbol.clone().into_bytes(),
                value: 0,
                size: 0,
                kind: SymbolKind::Label,
                scope: SymbolScope::Compilation,
                weak: false,
                section: SymbolSection::Section(sections[0]),
                flags: SymbolFlags::None,
            }),
            None => {
                let why = format!(
                    "section '{}' is a COMDAT about '{}', which the file does not define",
                    obj.section(sections[0]).name().unwrap_or_default(),
                    group.symbol
                );
                return Err(Error::Refused { why });
            }
        };
        together.push(group.keep == Keep::Together);
        obj.add_comdat(object::write::Comdat { kind: group.keep.kind(), symbol, sections });
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
            let addend = match reloc.kind {
                crate::section::Reference::Field(fixup) if carried => {
                    let data = obj.section_mut(*id).data_mut();
                    let Some(bytes) = data.get_mut(reloc.at..reloc.at + 4) else {
                        let why = format!("a field at {} is past the end of its section", reloc.at);
                        return Err(Error::Refused { why });
                    };
                    let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    let word = crate::coff::carry(fixup, word, addend)
                        .map_err(|why| Error::Refused { why })?;
                    bytes.copy_from_slice(&word.to_le_bytes());
                    0
                }
                _ => addend,
            };
            let record = Relocation { offset: reloc.at as u64, symbol, addend, flags };
            crate::file::relocate(&mut obj, *id, record)?;
        }
    }

    // The debug information, every section before any relocation because a relocation in one of
    // them names another as often as it names a function.
    let mut named = Map::default();
    for chunk in &info.chunks {
        // An i386 file keeps each addend in the bytes of its section, which a compressed section
        // no longer holds, so its debug sections are left as they are for now.
        let how = if flavour == Flavour::Elf && obj.architecture() != Architecture::I386 {
            info.compress
        } else {
            Compress::None
        };
        let id = crate::zlib::debug_section(&mut obj, chunk, how);
        named.insert(chunk.name.as_str(), id);
    }
    for chunk in &info.chunks {
        let section = named[chunk.name.as_str()];
        for reloc in &chunk.relocs {
            // The debug information names a function by its C name, and on i386 COFF the listing
            // gave it the underscore every C name has there.
            let spelled =
                if pe32 { crate::coff::decorate(&reloc.symbol) } else { reloc.symbol.clone() };
            let (symbol, addend) = match named.get(reloc.symbol.as_str()) {
                Some(&id) => (obj.section_symbol(id), reloc.addend),
                None => match defined.get(spelled.as_str()).map(|name| name.at) {
                    Some(Held::In { part, offset }) => {
                        (obj.section_symbol(made[part]), reloc.addend + offset as i64)
                    }
                    _ => match symbols.get(&spelled) {
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
            let kind = flavour.debug(reloc.kind, named.contains_key(reloc.symbol.as_str()));
            let flags = flags_of(kind, reloc.after)
                .ok_or_else(|| Error::Refused { why: format!("no relocation is {kind:?}") })?;
            let record = Relocation { offset: reloc.at as u64, symbol, addend, flags };
            crate::file::relocate(&mut obj, section, record)?;
        }
    }

    // No marker of its own. A file of assembly has the stack it says it has, which is the
    // `.note.GNU-stack` it wrote or the one `--noexecstack` had the reader add, as with gas.

    let mut bytes = obj.write().map_err(|why| Error::Refused { why: why.to_string() })?;
    if flavour == Flavour::Elf {
        for part in input.parts.iter().filter(|part| part.shape.merge != 0) {
            entry_size(&mut bytes, &part.name, part.shape.merge);
        }
        // A table of function addresses is a run of pointers, and gas says so in its header.
        let pointer = crate::elf::Headers::read(&bytes).entry_size().1 as u64;
        for part in input.parts.iter().filter(|part| part.shape.array.is_some()) {
            entry_size(&mut bytes, &part.name, pointer);
        }
        together_groups(&mut bytes, &together);
        linked(&mut bytes, input, &defined)?;
    }
    Ok(bytes)
}

/// Write the section each `o` section goes with into its `sh_link`, which the writer underneath has
/// no field for. See [`crate::elf::link`], which does the same for the records the compiler writes
/// itself.
///
/// What the source named is a symbol, and the section is the one the symbol is in, or a section of
/// that name when no symbol has it, which is what gas takes as well. Two sections may share a name
/// here, so a part's header is found by how many parts of the same name come before it, which is
/// the order the writer puts them in.
///
/// # Errors
///
/// [`Error::Refused`] for a name that is in no section of the file, which gas refuses too: a
/// section that goes with nothing is one a linker reads as an error.
fn linked(bytes: &mut [u8], input: &Assembled, defined: &Map<&str, &Name>) -> Result<(), Error> {
    if input.parts.iter().all(|part| part.link.is_none()) {
        return Ok(());
    }
    let headers = crate::elf::Headers::read(bytes);
    let index = |part: usize| {
        let name = &input.parts[part].name;
        let nth = input.parts[..part].iter().filter(|other| other.name == *name).count();
        headers.list.iter().enumerate().filter(|(_, header)| header.name == *name).nth(nth)
    };
    let mut writes = Vec::new();
    for (at, part) in input.parts.iter().enumerate() {
        let Some(link) = &part.link else { continue };
        let target = match defined.get(link.as_str()).map(|name| name.at) {
            Some(Held::In { part, .. }) => Some(part),
            _ => input.parts.iter().position(|other| other.name == *link),
        };
        let Some(target) = target.and_then(index) else {
            let why = format!("section '{}' goes with '{link}', which is in no section", part.name);
            return Err(Error::Refused { why });
        };
        let (_, header) = index(at).expect("a header for every section");
        let target = u32::try_from(target.0).expect("a file with this many sections in it");
        writes.push((header.at + headers.link(), target));
    }
    for (at, target) in writes {
        bytes[at..at + 4].copy_from_slice(&target.to_le_bytes());
    }
    Ok(())
}

/// Write how long an entry of a mergeable section is into its header, which the linker needs and
/// the writer underneath has no field for. It writes one only for a section of strings it made
/// itself. The section is found by its name, which is unique because the assembler gave every name
/// one section.
fn entry_size(bytes: &mut [u8], name: &str, size: u64) {
    let headers = crate::elf::Headers::read(bytes);
    let (field, width) = headers.entry_size();
    for header in headers.list.iter().filter(|header| header.name == name) {
        let at = header.at + field;
        bytes[at..at + width].copy_from_slice(&size.to_le_bytes()[..width]);
    }
}

/// Clear the word that makes a group a COMDAT in each group of an ELF file that is not one, which
/// the writer underneath writes into every group it makes. `together` holds a flag for each group
/// in the order they were added, which is the order the writer puts their headers in.
fn together_groups(bytes: &mut [u8], together: &[bool]) {
    if !together.contains(&true) {
        return;
    }
    let headers = crate::elf::Headers::read(bytes);
    let groups = headers.list.iter().filter(|header| header.sh_type == elf::SHT_GROUP.0);
    for (header, _) in groups.zip(together).filter(|(_, together)| **together) {
        bytes[header.offset..header.offset + 4].copy_from_slice(&0u32.to_le_bytes());
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
///
/// COFF for i386, `pe32`, does the same for a temporary name, whatever refers to it, since it has
/// no stubs or tables of that kind to go through. gas goes further there and writes the section for
/// every name the file defines, `_main` included. Naming the symbol instead is what the other COFF
/// machines here do, and the linker lands on the same address either way.
fn moved(
    flavour: Flavour,
    pe32: bool,
    input: &Assembled,
    defined: &Map<&str, &Name>,
    reloc: &Reloc,
) -> Option<(usize, u64)> {
    use crate::section::Reference;
    let name = defined.get(reloc.symbol.as_str())?;
    let Held::In { part, offset } = name.at else { return None };
    // A reference to an indirect function is to whatever its resolver picks, and the place the
    // name marks is the resolver. The section and an offset would be the resolver itself, so the
    // name stays, which is what gas leaves for one too.
    if name.sort == Sort::Ifunc {
        return None;
    }
    if pe32 && unseen(name, pe32) {
        return Some((part, offset));
    }
    if flavour != Flavour::Elf || name.binding != Binding::Local {
        return None;
    }
    let near = matches!(
        reloc.kind,
        Reference::Data
            | Reference::Away
            | Reference::AwayWide
            | Reference::Short
            | Reference::Tiny
    );
    let fixed = match reloc.kind {
        Reference::Call
        | Reference::Got
        | Reference::GotBare
        | Reference::GotKept
        | Reference::Slot
        | Reference::SlotKept
        | Reference::GotFront
        | Reference::Thread
        | Reference::Tls(_) => false,
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
            | Fixup::TprelLo12Nc
            | Fixup::TlsdescAdrPage21
            | Fixup::TlsdescLd64Lo12
            | Fixup::TlsdescAddLo12
            | Fixup::TlsdescCall,
        ) => false,
        _ if input.parts.get(part)?.shape.merge != 0 => !near && reloc.addend == 0,
        _ => true,
    };
    fixed.then_some((part, offset))
}

/// Whether a name is one the assembler made up or a label only it sees, which gas leaves out of the
/// table unless a relocation still names it. `.L` is the prefix for those that ELF assemblers agree
/// on, and a name with a `\u{1}` in it is one this assembler made for a numbered label or a frame.
///
/// COFF for i386, `pe32`, adds a bare `L`, which is what gcc and clang for that target start their
/// own labels with, `L3` for a block and `LC0` for a string. No C name can start that way there,
/// since every one of them has an underscore in front, and gas for `pe-i386` leaves them out too.
fn unseen(name: &Name, pe32: bool) -> bool {
    name.binding == Binding::Local
        && (name.name.starts_with(".L")
            || name.name.starts_with("..")
            || name.name.contains('\u{1}')
            || (pe32 && name.name.starts_with('L')))
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

    use object::read::elf::{FileHeader as _, SectionHeader as _, Sym as _};
    use object::read::{Object as _, ObjectComdat as _, ObjectSection as _, ObjectSymbol as _};
    use object::{RelocationFlags, SectionFlags};
    use rucc_target::{Arch as TargetArch, Env, Os, Triple};

    use crate::section::{Reference, Tls};

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
            group: None,
            link: None,
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
        let input = Assembled { parts: vec![odd], names: Vec::new(), subsections: false };
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
        let input = Assembled { parts: vec![room], names: Vec::new(), subsections: false };
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
            subsections: false,
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
            subsections: false,
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
            subsections: false,
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
            subsections: false,
        };
        let bytes = assembled(&input, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let sym = file.symbols().find(|s| s.name() == Ok("size_of_it")).expect("the symbol");
        assert_eq!(sym.address(), 25);
        assert_eq!(sym.section(), object::SymbolSection::Absolute, "it is not in any section");
    }

    /// `@feat.00` as the i386 COFF listing sets it, which has to come out static the way clang
    /// writes it rather than as a label.
    #[test]
    fn a_local_set_on_coff_is_a_static_number() {
        let input = Assembled {
            parts: vec![part(".text", vec![0xc3])],
            names: vec![Name {
                name: "@feat.00".to_owned(),
                at: Held::Absolute(1),
                size: 0,
                sort: Sort::Untyped,
                binding: Binding::Local,
                visibility: Visibility::Default,
            }],
            subsections: false,
        };
        let bytes = assembled(&input, &windows()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let sym = file.symbols().find(|s| s.name() == Ok("@feat.00")).expect("the symbol");
        // The reader gives an absolute COFF symbol no address, so the value is read as written.
        let coff = object::read::coff::CoffFile::<&[u8]>::parse(&bytes[..]).expect("COFF");
        let raw = coff.symbol_by_name("@feat.00").expect("the symbol");
        assert_eq!(object::read::coff::Symbol::value(raw.coff_symbol()), 1);
        assert_eq!(sym.section(), object::SymbolSection::Absolute);
        assert!(sym.is_local());
        assert!(
            matches!(
                sym.flags(),
                SymbolFlags::Coff { storage_class: pe::IMAGE_SYM_CLASS_STATIC, .. }
            ),
            "{:?}",
            sym.flags()
        );
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
            subsections: false,
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
            subsections: false,
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
                let name = if symbol.kind() == SymbolKind::Section {
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
        let input = Assembled { parts: vec![text, strings], names: vec![name], subsections: false };
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
        let input = Assembled { parts: vec![data], names: Vec::new(), subsections: false };
        let why = assembled(&input, &target()).expect_err("this cannot be written");
        assert!(format!("{why}").contains("nowhere"), "{why}");
    }

    #[test]
    fn the_stack_is_marked_only_by_the_file_and_only_once() {
        // gas adds no marker the file did not write unless `--noexecstack` asks, and that is the
        // reader's to do, so a file of parts gets exactly the ones it has.
        let bare = Assembled {
            parts: vec![part(".text", vec![0x90])],
            names: Vec::new(),
            subsections: false,
        };
        let bytes = assembled(&bare, &target()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert!(file.section_by_name(".note.GNU-stack").is_none(), "a marker nobody wrote");

        let said = Assembled {
            parts: vec![part(".text", vec![0x90]), part(".note.GNU-stack", Vec::new())],
            names: Vec::new(),
            subsections: false,
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
            subsections: false,
        };
        assert_eq!(assembled_defines(&input), vec!["reachable".to_owned()]);
    }

    #[test]
    fn a_machine_this_does_not_write_is_refused_rather_than_written_wrong() {
        let input = Assembled {
            parts: vec![part(".text", vec![0x90])],
            names: Vec::new(),
            subsections: false,
        };
        let elsewhere = TargetInfo::new(Triple::new(TargetArch::Riscv64, Os::Linux, Env::Gnu));
        let why = assembled(&input, &elsewhere).expect_err("this cannot be written");
        assert!(format!("{why}").contains("riscv64"), "{why}");
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
            subsections: false,
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
    fn a_file_of_assembly_for_aarch64_windows_carries_its_addends_in_the_instructions() {
        // The same instructions as the ELF test above, and a load of the eighth byte of the table,
        // a `bl` to a name, then the two halves of an offset into `.tls`. COFF has no addend field,
        // so the eight goes into the page `adrp` names, into the low twelve bits `add` carries and,
        // divided by the size of the access, into the offset of the load. A distance written as
        // data is `REL32`, which counts from the end of its four bytes, so four more is in them.
        let adrp = 0x9000_0000u32;
        let add = 0x9100_0000u32;
        let ldr = 0xf940_0000u32;
        let bl = 0x9400_0000u32;
        let words = [adrp, add, ldr, bl, add | 1 << 22, add];
        let mut text = part(".text", words.iter().flat_map(|word| word.to_le_bytes()).collect());
        let field = |at, symbol: &str, fixup, addend| Reloc {
            at,
            symbol: symbol.to_owned(),
            kind: Reference::Field(fixup),
            addend,
            after: 0,
        };
        text.relocs = vec![
            field(0, "table", Fixup::AdrPage21, 8),
            field(4, "table", Fixup::AddLo12, 8),
            field(8, "table", Fixup::Ldst64Lo12, 8),
            field(12, "g", Fixup::Call26, 0),
            field(16, "counter", Fixup::SecrelHigh12A, 0),
            field(20, "counter", Fixup::SecrelLow12A, 0),
        ];
        let mut data = part(".data", vec![0; 16]);
        data.relocs = vec![
            Reloc {
                at: 0,
                symbol: "table".to_owned(),
                kind: Reference::Address { bytes: 8 },
                addend: 8,
                after: 0,
            },
            Reloc { at: 8, symbol: "g".to_owned(), kind: Reference::Data, addend: 0, after: 0 },
            Reloc {
                at: 12,
                symbol: "table".to_owned(),
                kind: Reference::Image,
                addend: 0,
                after: 0,
            },
        ];
        let mut table = at("table", 0, Sort::Object, Binding::Global);
        table.at = Held::In { part: 1, offset: 0 };
        let undefined =
            |name| Name { at: Held::Undefined, ..at(name, 0, Sort::Untyped, Binding::Global) };
        let input = Assembled {
            parts: vec![text, data],
            names: vec![table, undefined("g"), undefined("counter")],
            subsections: false,
        };
        let target = TargetInfo::new(Triple::new(TargetArch::Aarch64, Os::Windows, Env::Gnu));
        let bytes = assembled(&input, &target).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert_eq!(file.format(), object::BinaryFormat::Coff);
        assert_eq!(file.architecture(), Architecture::Aarch64);
        let relocs = |name: &str| -> Vec<(u64, u16, String)> {
            let section = file.section_by_name(name).expect("the section");
            section
                .relocations()
                .map(|(at, reloc)| {
                    let RelocationFlags::Coff { typ } = reloc.flags() else { panic!("COFF") };
                    let object::RelocationTarget::Symbol(symbol) = reloc.target() else {
                        panic!("a symbol")
                    };
                    let symbol = file.symbol_by_index(symbol).expect("the symbol");
                    (at, typ.0, symbol.name().expect("a name").to_owned())
                })
                .collect()
        };
        let named = |at, typ: pe::RelocationType, name: &str| (at, typ.0, name.to_owned());
        assert_eq!(
            relocs(".text"),
            [
                named(0, pe::IMAGE_REL_ARM64_PAGEBASE_REL21, "table"),
                named(4, pe::IMAGE_REL_ARM64_PAGEOFFSET_12A, "table"),
                named(8, pe::IMAGE_REL_ARM64_PAGEOFFSET_12L, "table"),
                named(12, pe::IMAGE_REL_ARM64_BRANCH26, "g"),
                named(16, pe::IMAGE_REL_ARM64_SECREL_HIGH12A, "counter"),
                named(20, pe::IMAGE_REL_ARM64_SECREL_LOW12A, "counter"),
            ]
        );
        assert_eq!(
            relocs(".data"),
            [
                named(0, pe::IMAGE_REL_ARM64_ADDR64, "table"),
                named(8, pe::IMAGE_REL_ARM64_REL32, "g"),
                named(12, pe::IMAGE_REL_ARM64_ADDR32NB, "table"),
            ]
        );
        let text = file.section_by_name(".text").expect("the section");
        let text = text.data().expect("the bytes");
        let word = |nth: usize| u32::from_le_bytes(text[nth * 4..nth * 4 + 4].try_into().unwrap());
        assert_eq!(word(0), adrp | 2 << 5, "eight bytes is immhi two and immlo nothing");
        assert_eq!(word(1), add | 8 << 10);
        assert_eq!(word(2), ldr | 1 << 10, "eight bytes is one doubleword");
        assert_eq!([word(3), word(4), word(5)], [bl, add | 1 << 22, add]);
        let data = file.section_by_name(".data").expect("the section");
        let data = data.data().expect("the bytes");
        assert_eq!(data[..8], 8u64.to_le_bytes());
        assert_eq!(data[8..12], 4u32.to_le_bytes());
    }

    #[test]
    fn an_addend_an_aarch64_coff_field_cannot_carry_is_refused() {
        // A load of four bytes cannot be told to start two bytes in, since its offset is counted in
        // fours, and a branch to a name and a number is not something the field can say for every
        // linker. Both are refused rather than written as something close.
        for (word, fixup, addend) in
            [(0xb940_0000u32, Fixup::Ldst32Lo12, 2), (0x9400_0000, Fixup::Call26, 4)]
        {
            let mut text = part(".text", word.to_le_bytes().to_vec());
            text.relocs = vec![Reloc {
                at: 0,
                symbol: "g".to_owned(),
                kind: Reference::Field(fixup),
                addend,
                after: 0,
            }];
            let input = Assembled {
                parts: vec![text],
                names: vec![Name {
                    at: Held::Undefined,
                    ..at("g", 0, Sort::Untyped, Binding::Global)
                }],
                subsections: false,
            };
            let target = TargetInfo::new(Triple::new(TargetArch::Aarch64, Os::Windows, Env::Gnu));
            let why = assembled(&input, &target).expect_err("a field that cannot say it");
            assert!(format!("{why}").contains(fixup.name()), "{why}");
        }
    }

    #[test]
    fn a_file_of_assembly_for_windows_is_written_as_coff() {
        // What tamnd/rucc#1514 was about. `runtime/builtins/chkstk.S` is a file of assembly for a
        // Windows target, and until this it was refused with a message about there being no object
        // writer for the triple, which read as the whole back end being missing rather than this
        // one path through it.
        let input = Assembled {
            parts: vec![part(".text", vec![0xc3])],
            names: Vec::new(),
            subsections: false,
        };
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
    fn a_coff_section_keeps_the_letters_it_was_given_and_its_comdat() {
        // `.section .drectve,"yni"` in chkstk.S, and the `.refptr.` pointers mingw-w64 wants one
        // copy of across every object that has one.
        let drectve = Part {
            shape: Shape::coff("yni").expect("the letters"),
            ..part(".drectve", b" -exclude-symbols:f".to_vec())
        };
        let refptr = Part {
            shape: Shape::coff("dr").expect("the letters"),
            group: Some(Group { symbol: ".refptr.x".to_owned(), keep: Keep::Any }),
            ..part(".rdata$.refptr.x", vec![0; 8])
        };
        let input = Assembled {
            parts: vec![refptr, drectve],
            names: vec![at(".refptr.x", 0, Sort::Object, Binding::Global)],
            subsections: false,
        };
        let bytes = assembled(&input, &windows()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let flags = |name: &str| match file.section_by_name(name).expect("the section").flags() {
            SectionFlags::Coff { characteristics } => characteristics.0,
            other => panic!("{other:?}"),
        };
        let removed = pe::IMAGE_SCN_LNK_REMOVE.0 | pe::IMAGE_SCN_LNK_INFO.0;
        assert_eq!(flags(".drectve") & removed, removed);
        assert_eq!(flags(".drectve") & (pe::IMAGE_SCN_MEM_READ.0 | pe::IMAGE_SCN_MEM_WRITE.0), 0);
        let refptr = flags(".rdata$.refptr.x");
        assert_ne!(refptr & pe::IMAGE_SCN_LNK_COMDAT.0, 0, "not a COMDAT");
        assert_eq!(refptr & pe::IMAGE_SCN_MEM_WRITE.0, 0, "`r` did not take writing away");
        let comdat = file.comdats().next().expect("the COMDAT");
        assert_eq!(comdat.kind(), object::ComdatKind::Any);
        assert_eq!(file.symbol_by_index(comdat.symbol()).unwrap().name(), Ok(".refptr.x"));

        // And one about a name the file never defines is refused rather than written broken.
        let input = Assembled { names: Vec::new(), ..input };
        assert!(assembled(&input, &windows()).is_err());
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
            subsections: false,
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
            subsections: false,
        };
        let bytes = assembled(&input, &windows()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let section = file.section_by_name(".text").expect("the section");
        let (at, reloc) = section.relocations().next().expect("the relocation");
        assert_eq!(at, 2);
        assert_eq!(
            reloc.flags(),
            RelocationFlags::Coff { typ: pe::RelocationType(pe::IMAGE_REL_AMD64_REL32.0 + 4) }
        );
    }

    /// A linux i386 target, which is written as a 32 bit ELF file with REL relocations.
    fn i386() -> TargetInfo {
        TargetInfo::new(Triple::new(TargetArch::X86, Os::Linux, Env::Gnu))
    }

    /// Every relocation of one section of an i386 file, as the offset it is at, its type, the
    /// symbol it names, and the addend the bytes it covers hold.
    ///
    /// The reader reports an addend of nothing for a REL file and says the real one is implicit,
    /// so what is added is read out of the section, which is where the linker reads it too.
    fn implicit(file: &object::File<'_>, section: &str) -> Vec<(u64, u32, String, i64)> {
        let section = file.section_by_name(section).expect("the section");
        let data = section.data().expect("the bytes");
        section
            .relocations()
            .map(|(at, reloc)| {
                assert!(reloc.has_implicit_addend(), "a REL file keeps the addend in the bytes");
                assert_eq!(reloc.addend(), 0);
                let RelocationFlags::Elf { r_type } = reloc.flags() else { panic!("ELF") };
                let width = crate::elf::width_i386(r_type).expect("a width");
                let at_ = at as usize;
                let mut word = [0u8; 8];
                word[..width].copy_from_slice(&data[at_..at_ + width]);
                // Sign extended from however many bytes it is, since a distance is negative as
                // often as not.
                let shift = 64 - 8 * width as u32;
                let addend = (i64::from_le_bytes(word) << shift) >> shift;
                let object::RelocationTarget::Symbol(index) = reloc.target() else {
                    panic!("a relocation against a symbol")
                };
                let symbol = file.symbol_by_index(index).expect("the symbol");
                let name = match symbol.kind() {
                    SymbolKind::Section => {
                        let section = symbol.section_index().expect("a section symbol's section");
                        file.section_by_index(section)
                            .expect("it")
                            .name()
                            .expect("a name")
                            .to_owned()
                    }
                    _ => symbol.name().expect("a name").to_owned(),
                };
                (at, r_type.0, name, addend)
            })
            .collect()
    }

    /// The relocations gas writes for the same instructions and data, with the addends where gas
    /// puts them.
    ///
    /// The code is what `gcc -m32 -fPIC` makes of a function that calls through the PLT, finds the
    /// global offset table, reaches a string of its own and a variable of someone else's, plus a
    /// plain call and an absolute address, which is what code that is not position independent
    /// writes. The data is an address at each width and a distance.
    #[test]
    fn an_i386_object_is_32_bit_elf_with_the_addends_in_the_bytes() {
        let reloc = |at, symbol: &str, kind, addend| Reloc {
            at,
            symbol: symbol.to_owned(),
            kind,
            addend,
            after: 0,
        };
        let code = vec![
            0xe8, 0, 0, 0, 0, // call foo@PLT
            0xe8, 0, 0, 0, 0, // call bar
            0x81, 0xc3, 0, 0, 0, 0, // addl $_GLOBAL_OFFSET_TABLE_, %ebx
            0x8d, 0x83, 0, 0, 0, 0, // leal .LC0@GOTOFF(%ebx), %eax
            0x8b, 0x83, 0, 0, 0, 0, // movl foo@GOT(%ebx), %eax
            0x89, 0x83, 0, 0, 0, 0, // movl %eax, foo@GOT(%ebx)
            0xa1, 0, 0, 0, 0, // movl counter+8, %eax
        ];
        let mut text = part(".text", code);
        text.relocs = vec![
            reloc(1, "foo", Reference::Call, -4),
            reloc(6, "bar", Reference::Data, -4),
            reloc(12, "_GLOBAL_OFFSET_TABLE_", Reference::GotFront, 2),
            reloc(18, ".LC0", Reference::GotOffset, 0),
            reloc(24, "foo", Reference::Slot, 0),
            reloc(30, "foo", Reference::SlotKept, 0),
            reloc(35, "counter", Reference::Signed, 8),
        ];
        let rodata = part(".rodata", b"abc\0hi\0\0".to_vec());
        let mut data = part(".data", vec![0; 11]);
        data.relocs = vec![
            reloc(0, "foo", Reference::Address { bytes: 4 }, 16),
            reloc(4, "bar", Reference::Away, 0),
            reloc(8, "foo", Reference::Address { bytes: 2 }, 0),
            reloc(10, "foo", Reference::Address { bytes: 1 }, 0),
        ];
        let undefined = |name: &str| Name {
            at: Held::Undefined,
            ..at(name, 0, Sort::Untyped, Binding::Global)
        };
        let names = vec![
            at("f", 0, Sort::Func, Binding::Global),
            Name {
                at: Held::In { part: 1, offset: 4 },
                ..at(".LC0", 0, Sort::Untyped, Binding::Local)
            },
            undefined("foo"),
            undefined("bar"),
            undefined("counter"),
            undefined("_GLOBAL_OFFSET_TABLE_"),
        ];
        let input = Assembled { parts: vec![text, rodata, data], names, subsections: false };
        let bytes = assembled(&input, &i386()).expect("an object");

        let header = elf::FileHeader32::<Endianness>::parse(&bytes[..]).expect("a 32 bit header");
        let endian = header.endian().expect("an endianness");
        assert_eq!(bytes[4], elf::ELFCLASS32.0);
        assert_eq!(header.e_machine(endian), elf::EM_386);
        assert_eq!(header.e_type(endian), elf::ET_REL);

        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert!(!file.is_64());
        assert_eq!(file.architecture(), Architecture::I386);
        for name in [".rel.text", ".rel.data"] {
            let section = file.section_by_name(name).expect("a REL section");
            let SectionFlags::Elf { sh_type, .. } = section.flags() else { panic!("ELF") };
            assert_eq!(sh_type, elf::SHT_REL, "{name}");
        }
        assert!(file.section_by_name(".rela.text").is_none(), "i386 has no addend field");

        let text = implicit(&file, ".text");
        let want = [
            (1, elf::R_386_PLT32, "foo", -4),
            (6, elf::R_386_PC32, "bar", -4),
            (12, elf::R_386_GOTPC, "_GLOBAL_OFFSET_TABLE_", 2),
            // A label only this file sees is its section and how far into it, as gas writes it.
            (18, elf::R_386_GOTOFF, ".rodata", 4),
            (24, elf::R_386_GOT32X, "foo", 0),
            (30, elf::R_386_GOT32, "foo", 0),
            (35, elf::R_386_32, "counter", 8),
        ];
        let want: Vec<_> = want
            .into_iter()
            .map(|(at, r_type, name, addend)| (at, r_type.0, name.to_owned(), addend))
            .collect();
        assert_eq!(text, want);

        let data = implicit(&file, ".data");
        let want = [
            (0, elf::R_386_32, "foo", 16),
            (4, elf::R_386_PC32, "bar", 0),
            (8, elf::R_386_16, "foo", 0),
            (10, elf::R_386_8, "foo", 0),
        ];
        let want: Vec<_> = want
            .into_iter()
            .map(|(at, r_type, name, addend)| (at, r_type.0, name.to_owned(), addend))
            .collect();
        assert_eq!(data, want);
    }

    /// Each way i386 reaches a thread-local variable is the relocation gas writes for its suffix,
    /// with what is added in the bytes and the variable's own name kept even though only this file
    /// sees it, which is what gas does for these where it would have named the section for any
    /// other reference.
    #[test]
    fn the_i386_thread_local_references_are_the_relocations_gas_writes() {
        let kinds = [
            (Tls::General, elf::R_386_TLS_GD),
            (Tls::Module, elf::R_386_TLS_LDM),
            (Tls::InModule, elf::R_386_TLS_LDO_32),
            (Tls::Slot, elf::R_386_TLS_GOTIE),
            (Tls::SlotAddress, elf::R_386_TLS_IE),
            (Tls::SlotNegated, elf::R_386_TLS_IE_32),
            (Tls::Offset, elf::R_386_TLS_LE),
            (Tls::Negated, elf::R_386_TLS_LE_32),
        ];
        let mut text = part(".text", vec![0; 4 * kinds.len()]);
        text.relocs = kinds
            .iter()
            .enumerate()
            .map(|(n, &(tls, _))| Reloc {
                at: 4 * n,
                symbol: "x".to_owned(),
                kind: Reference::Tls(tls),
                addend: n as i64,
                after: 0,
            })
            .collect();
        let tdata = part(".tdata", vec![0; 8]);
        let names = vec![
            at("f", 0, Sort::Func, Binding::Global),
            Name {
                at: Held::In { part: 1, offset: 4 },
                ..at("x", 0, Sort::Thread, Binding::Local)
            },
        ];
        let input = Assembled { parts: vec![text, tdata], names, subsections: false };
        let bytes = assembled(&input, &i386()).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        let want: Vec<_> = kinds
            .iter()
            .enumerate()
            .map(|(n, &(_, r_type))| (4 * n as u64, r_type.0, "x".to_owned(), n as i64))
            .collect();
        assert_eq!(implicit(&file, ".text"), want);
    }

    /// A file of assembly for i386 on Windows is COFF with the i386 relocations, and its names are
    /// what the file spelled: a listing for that platform already has the underscore on every C
    /// name, so nothing is put in front of `_main`, `_puts` or a `__fastcall` `@f@8`.
    #[test]
    fn an_i386_windows_file_of_assembly_keeps_its_names_as_written() {
        use object::pe::{
            IMAGE_REL_I386_DIR32, IMAGE_REL_I386_DIR32NB, IMAGE_REL_I386_REL32,
            IMAGE_REL_I386_SECREL,
        };
        let reloc = |at, symbol: &str, kind, addend| Reloc {
            at,
            symbol: symbol.to_owned(),
            kind,
            addend,
            after: 0,
        };
        let code = vec![
            0xe8, 0, 0, 0, 0, // call _puts
            0xe8, 0, 0, 0, 0, // call @f@8
            0xa1, 0, 0, 0, 0, // movl _counter+8, %eax
            0xc3,
        ];
        let mut text = part(".text", code);
        text.relocs = vec![
            reloc(1, "_puts", Reference::Call, -4),
            reloc(6, "@f@8", Reference::Call, -4),
            reloc(11, "_counter", Reference::Address { bytes: 4 }, 8),
        ];
        let mut data = part(".data", vec![0; 12]);
        data.relocs = vec![
            reloc(0, "_main", Reference::Image, 0),
            reloc(4, "_main", Reference::Section, 0),
            reloc(8, "_puts", Reference::Away, 0),
        ];
        let undefined = |name: &str| Name {
            at: Held::Undefined,
            ..at(name, 0, Sort::Untyped, Binding::Global)
        };
        let names = vec![
            at("_main", 0, Sort::Func, Binding::Global),
            undefined("_puts"),
            undefined("@f@8"),
            undefined("_counter"),
        ];
        let input = Assembled { parts: vec![text, data], names, subsections: false };
        let target = TargetInfo::new(Triple::new(TargetArch::X86, Os::Windows, Env::Gnu));
        let bytes = assembled(&input, &target).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert_eq!(file.format(), object::BinaryFormat::Coff);
        assert_eq!(file.architecture(), Architecture::I386);
        for name in ["_main", "_puts", "@f@8", "_counter"] {
            assert!(file.symbol_by_name(name).is_some(), "{name}");
        }
        assert!(file.symbol_by_name("__main").is_none());
        let types = |name: &str| -> Vec<(u64, u16, String)> {
            let section = file.section_by_name(name).expect("a section");
            section
                .relocations()
                .map(|(at, reloc)| {
                    let RelocationFlags::Coff { typ } = reloc.flags() else { panic!("COFF") };
                    let object::RelocationTarget::Symbol(symbol) = reloc.target() else {
                        panic!("a symbol")
                    };
                    let symbol = file.symbol_by_index(symbol).expect("a symbol");
                    (at, typ.0, symbol.name().expect("a name").to_owned())
                })
                .collect()
        };
        let want = |list: &[(u64, pe::RelocationType, &str)]| -> Vec<(u64, u16, String)> {
            list.iter().map(|&(at, typ, name)| (at, typ.0, name.to_owned())).collect()
        };
        assert_eq!(
            types(".text"),
            want(&[
                (1, IMAGE_REL_I386_REL32, "_puts"),
                (6, IMAGE_REL_I386_REL32, "@f@8"),
                (11, IMAGE_REL_I386_DIR32, "_counter"),
            ])
        );
        assert_eq!(
            types(".data"),
            want(&[
                (0, IMAGE_REL_I386_DIR32NB, "_main"),
                (4, IMAGE_REL_I386_SECREL, "_main"),
                (8, IMAGE_REL_I386_REL32, "_puts"),
            ])
        );
        let code = file.section_by_name(".text").expect("a text section");
        let code = code.data().expect("the bytes");
        assert_eq!(&code[1..5], &0i32.to_le_bytes(), "a call counts from the end of its bytes");
        assert_eq!(&code[11..15], &8i32.to_le_bytes());
        let image = file.section_by_name(".data").expect("a data section");
        assert_eq!(&image.data().expect("the bytes")[8..12], &4i32.to_le_bytes());
    }

    /// The labels gcc writes for i686 Windows start with a bare `L`, and they are left out of the
    /// table the way gas leaves them out, with whatever pointed at one pointing at its section and
    /// the distance into it added in the bytes.
    #[test]
    fn an_i386_windows_label_of_gccs_own_is_its_section_and_not_a_name() {
        use object::pe::{IMAGE_REL_I386_DIR32, IMAGE_REL_I386_REL32};
        // movl $LC1, (%esp) ; jmp L3 ; L3: ret
        let mut text = part(".text", vec![0xc7, 0x04, 0x24, 0, 0, 0, 0, 0xe9, 0, 0, 0, 0, 0xc3]);
        let reloc = |at, symbol: &str, kind, addend| Reloc {
            at,
            symbol: symbol.to_owned(),
            kind,
            addend,
            after: 0,
        };
        text.relocs = vec![
            reloc(3, "LC1", Reference::Address { bytes: 4 }, 0),
            reloc(8, "Lfar", Reference::Data, -4),
        ];
        let rdata = part(".rdata", vec![0; 8]);
        let names = vec![
            at("_f", 0, Sort::Func, Binding::Global),
            at("L3", 12, Sort::Untyped, Binding::Local),
            Name {
                at: Held::In { part: 1, offset: 4 },
                ..at("LC1", 0, Sort::Untyped, Binding::Local)
            },
            Name {
                at: Held::In { part: 1, offset: 0 },
                ..at("Lfar", 0, Sort::Untyped, Binding::Local)
            },
        ];
        let input = Assembled { parts: vec![text, rdata], names, subsections: false };
        let target = TargetInfo::new(Triple::new(TargetArch::X86, Os::Windows, Env::Gnu));
        let bytes = assembled(&input, &target).expect("an object");
        let file = object::File::parse(&bytes[..]).expect("a readable object");
        assert!(file.symbol_by_name("_f").is_some());
        for name in ["L3", "LC1", "Lfar"] {
            assert!(file.symbol_by_name(name).is_none(), "{name}");
        }
        let section = file.section_by_name(".text").expect("a section");
        let relocs: Vec<_> = section
            .relocations()
            .map(|(at, reloc)| {
                let RelocationFlags::Coff { typ } = reloc.flags() else { panic!("COFF") };
                let object::RelocationTarget::Symbol(symbol) = reloc.target() else {
                    panic!("a symbol")
                };
                let symbol = file.symbol_by_index(symbol).expect("a symbol");
                (at, typ.0, symbol.name().expect("a name").to_owned())
            })
            .collect();
        assert_eq!(
            relocs,
            vec![
                (3, IMAGE_REL_I386_DIR32.0, ".rdata".to_owned()),
                (8, IMAGE_REL_I386_REL32.0, ".rdata".to_owned())
            ]
        );
        let code = section.data().expect("the bytes");
        assert_eq!(&code[3..7], &4i32.to_le_bytes(), "the distance into .rdata");
        assert_eq!(&code[8..12], &0i32.to_le_bytes(), "the start of .rdata, counted from the end");
    }

    /// What i386 has no relocation for is refused rather than written as the nearest thing.
    ///
    /// An address in eight bytes and a distance in eight are wider than anything this machine
    /// relocates, a load from the instruction pointer is something it cannot do, and the x86-64 slot
    /// of a thread-local variable is not how this machine reaches one.
    #[test]
    fn a_reference_i386_has_no_relocation_for_is_refused() {
        for kind in [
            Reference::Address { bytes: 8 },
            Reference::AwayWide,
            Reference::Got,
            Reference::GotBare,
            Reference::GotKept,
            Reference::Thread,
            Reference::Image,
            Reference::Section,
        ] {
            let mut text = part(".text", vec![0; 8]);
            text.relocs =
                vec![Reloc { at: 0, symbol: "foo".to_owned(), kind, addend: 0, after: 0 }];
            let names =
                vec![Name { at: Held::Undefined, ..at("foo", 0, Sort::Untyped, Binding::Global) }];
            let input = Assembled { parts: vec![text], names, subsections: false };
            let Err(Error::Refused { .. }) = assembled(&input, &i386()) else {
                panic!("{kind:?} was written for i386");
            };
        }
    }

    /// An addend that does not fit in the bytes that have to hold it is refused, since a REL file
    /// has nowhere else to put the rest of it.
    #[test]
    fn an_i386_addend_too_wide_for_its_bytes_is_refused() {
        let mut data = part(".data", vec![0; 2]);
        let kind = Reference::Address { bytes: 1 };
        data.relocs = vec![Reloc { at: 0, symbol: "foo".to_owned(), kind, addend: 300, after: 0 }];
        let names =
            vec![Name { at: Held::Undefined, ..at("foo", 0, Sort::Untyped, Binding::Global) }];
        let input = Assembled { parts: vec![data], names, subsections: false };
        let Err(Error::Refused { why }) = assembled(&input, &i386()) else {
            panic!("an addend of 300 went into one byte");
        };
        assert!(why.contains("300"), "{why}");
    }

    /// The header fields written into the finished bytes land where a 32 bit file keeps them,
    /// which is not where a 64 bit one does.
    #[test]
    fn a_32_bit_file_gets_its_entry_size_and_groups_in_the_right_place() {
        let mut strings = part(".rodata.str1.1", b"hi\0".to_vec());
        strings.shape.merge = 1;
        strings.shape.strings = true;
        let mut kept = part(".text.f", vec![0xc3]);
        kept.group = Some(Group { symbol: "f".to_owned(), keep: Keep::Together });
        let names = vec![Name {
            at: Held::In { part: 1, offset: 0 },
            ..at("f", 0, Sort::Func, Binding::Global)
        }];
        let input = Assembled { parts: vec![strings, kept], names, subsections: false };
        let bytes = assembled(&input, &i386()).expect("an object");
        let header = elf::FileHeader32::<Endianness>::parse(&bytes[..]).expect("a 32 bit header");
        let endian = header.endian().expect("an endianness");
        let sections = header.sections(endian, &bytes[..]).expect("the sections");
        let find = |name: &str| {
            sections
                .iter()
                .find(|section| sections.section_name(endian, section) == Ok(name.as_bytes()))
                .expect("the section")
        };
        assert_eq!(find(".rodata.str1.1").sh_entsize(endian), 1);
        let group = find(".group");
        let at = group.sh_offset(endian) as usize;
        assert_eq!(&bytes[at..at + 4], &[0; 4], "a group that is not a COMDAT says so");
    }

    /// Two records of the same name, each going with the text of the function it is about. The
    /// linker refuses to put a section that goes with something beside one of the same name that
    /// does not, so both have to say it, and each has to say the right section.
    #[test]
    fn a_section_that_goes_with_a_name_points_at_the_section_the_name_is_in() {
        let record = |link: &str| Part {
            shape: Shape { write: true, ..Shape::of("__patchable_function_entries") },
            link: Some(link.to_owned()),
            ..part("__patchable_function_entries", vec![0; 8])
        };
        let parts = vec![
            part(".text", vec![0xc3]),
            part(".init.text", vec![0xc3]),
            record("f"),
            record("g"),
        ];
        let names = vec![
            Name { at: Held::In { part: 1, offset: 0 }, ..at("f", 0, Sort::Func, Binding::Global) },
            at("g", 0, Sort::Func, Binding::Global),
        ];
        let bytes = assembled(&Assembled { parts, names, subsections: false }, &target())
            .expect("an object");
        let header = elf::FileHeader64::<Endianness>::parse(&bytes[..]).expect("a header");
        let endian = header.endian().expect("an endianness");
        let sections = header.sections(endian, &bytes[..]).expect("the sections");
        let index = |name: &str| {
            sections
                .iter()
                .position(|section| sections.section_name(endian, section) == Ok(name.as_bytes()))
                .expect("the section") as u32
        };
        let records: Vec<_> = sections
            .iter()
            .filter(|section| {
                sections.section_name(endian, section) == Ok(&b"__patchable_function_entries"[..])
            })
            .collect();
        assert_eq!(records.len(), 2);
        for record in &records {
            assert!(
                record.sh_flags(endian).contains(elf::SHF_LINK_ORDER),
                "a record that goes with nothing"
            );
        }
        assert_eq!(records[0].sh_link(endian), index(".init.text"));
        assert_eq!(records[1].sh_link(endian), index(".text"));
    }

    #[test]
    fn a_section_that_goes_with_a_name_in_no_section_is_refused() {
        let record = Part {
            link: Some("nowhere".to_owned()),
            ..part("__patchable_function_entries", vec![0; 8])
        };
        let input = Assembled { parts: vec![record], names: Vec::new(), subsections: false };
        assert!(assembled(&input, &target()).is_err());
    }
}
