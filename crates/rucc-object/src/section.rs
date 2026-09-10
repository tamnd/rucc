//! What an object writer is given, which is a section of bytes and what the linker has to be
//! told about them.
//!
//! Design: `spec/11-asm-objects-debug.md` sections 11.1 and 11.3.
//!
//! These types are here rather than beside the assembler that fills them in because they are what
//! an object file is made of, and because a writer cannot depend on the thing that produces its
//! input without the graph going the wrong way round. The assembler at layer rank 11 reaches down
//! to these at rank 9, which is the direction `spec/18-package-layout.md` asks for.

/// What a function is aligned to when nothing asked for more.
///
/// Sixteen because that is what every x86-64 toolchain puts a function at, and because it is what
/// keeps the loop inside one from straddling one more cache line than it has to. Here rather than
/// beside the assembler because the assembler pads to it and the writer records it, and two
/// copies of one number is how the padding and the record come apart.
pub const FUNC_ALIGN: u32 = 16;

/// Whether each function and each variable gets a section to itself.
///
/// Design: `spec/11-asm-objects-debug.md` section 11.3, and `spec/04-driver-and-cli.md` section 4.7
/// for the flags that ask for it.
///
/// A linker can drop a section nothing reaches and cannot drop half of one, so a file whose
/// functions share a section keeps every function that file defines in the output as soon as any
/// one of them is called. Splitting them is what makes `--gc-sections` do anything, which is how an
/// embedded image or a kernel gets small, and it is the whole of what these two flags are for. The
/// cost is a section header per name, which is why it is asked for rather than always done.
///
/// Not one flag, because gcc has two and a build that wants one of them and not the other is a
/// build that measured something. Splitting the code is nearly free at link time; splitting the
/// data can defeat the linker's ordering of what is next to what.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sections {
    /// `-ffunction-sections`. Each function in `.text.<name>` rather than all of them in `.text`.
    pub functions: bool,
    /// `-fdata-sections`. Each variable in a section named after it rather than in the one its
    /// contents would otherwise have chosen.
    pub data: bool,
}

impl Sections {
    /// Whether either of them was asked for.
    #[must_use]
    pub const fn any(self) -> bool {
        self.functions || self.data
    }
}

/// What a file says it was built to have checked, which is what `-fcf-protection=` asks for.
///
/// Design: `spec/11-asm-objects-debug.md` section 11.3, and `spec/04-driver-and-cli.md` section 4.7
/// for the flag.
///
/// A machine's control flow checks are turned on for a whole process or not at all, never for one
/// function, so a program made of one object built with them and one built without has to be run
/// one way or the other. What everybody settled on is that each object records what it was built
/// for, the linker keeps only what every input agreed on, and the loader turns on what is left. So
/// an object that records nothing turns the check off for every object it is linked with, which is
/// why this is written even when the flag changed no instruction in the file.
///
/// One number rather than a pair of flags, because that is what the record holds: a word of bits
/// whose meaning is the machine's, and a linker that has never heard of a bit still knows to drop
/// it when one input does not have it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Property {
    /// The bits of the x86 feature word, which are [`Self::IBT`] and [`Self::SHSTK`].
    pub features: u32,
}

impl Property {
    /// Which property the feature word is, which is the key the record is written under.
    pub const X86_FEATURES: u32 = 0xc000_0002;
    /// Indirect branch tracking: every indirect call and jump in the file arrives at a landing
    /// pad, so the machine may fault on one that does not.
    pub const IBT: u32 = 1;
    /// The shadow stack: every return in the file goes where a second copy of the return address
    /// says it should, so the machine may fault when the two disagree.
    pub const SHSTK: u32 = 2;

    /// Whether anything is recorded at all, which is whether the record is written.
    #[must_use]
    pub const fn any(self) -> bool {
        self.features != 0
    }
}

/// What the command line decided about the file being written, as against what the code in it
/// decided.
///
/// Two answers with nothing to do with each other, together because they arrive together: neither
/// can be worked out from a function, and the listing and the byte writer have to be handed the
/// same pair or the two outputs of one command line would not be the same file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Output {
    /// Whether each function and each variable gets a section to itself.
    pub sections: Sections,
    /// What the file says it was built to have checked.
    pub property: Property,
}

/// A text section, and what the linker has to be told about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    /// The instructions, in the order they were laid out.
    pub bytes: Vec<u8>,
    /// Where each function starts and how long it is, in the order they were written.
    pub funcs: Vec<Extent>,
    /// Every place in the bytes that names something the linker has to find.
    pub relocs: Vec<Reloc>,
    /// What the whole section has to be aligned to, which is the largest alignment any function
    /// in it asked for.
    ///
    /// A function is at a fixed offset inside the section, so a function at a multiple of two
    /// hundred and fifty six is one only if the section itself is at one. The padding between the
    /// functions is the assembler's half of the same job and this is the linker's.
    pub align: u32,
    /// What an unwinder is told about the functions, which is empty for a format that has no such
    /// section or a build that asked for none.
    pub unwind: Unwind,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            bytes: Vec::new(),
            funcs: Vec::new(),
            relocs: Vec::new(),
            align: FUNC_ALIGN,
            unwind: Unwind::default(),
        }
    }
}

/// The unwind table, as the bytes of its own section and what the linker has to be told about them.
///
/// Bytes rather than rows, because what a record is is DWARF's answer and not the object format's,
/// and the layer that knows what a frame did is the one that can say it in the fewest of them. What
/// is left for the writer is where the section goes and what its relocations are, which is the part
/// the three formats disagree about.
///
/// Each record says where its function is as a distance from the record to the function, which is
/// a number no compilation knows: a function is at a fixed offset inside its own section and the
/// section is placed by the linker. So there is one relocation per record and it is the ordinary
/// instruction pointer relative one, since the distance is between two things in the same file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unwind {
    /// The records, one shared header and one per function.
    pub bytes: Vec<u8>,
    /// Every place in them that names a function the linker has to place.
    pub relocs: Vec<Reloc>,
}

/// Where the room a patcher was promised at the top of a function ended up.
///
/// What `-fpatchable-function-entry=` asks for, once it is bytes rather than instructions. Two
/// numbers because the writer has two questions: where the address it records points, and how much
/// of the function is in front of the symbol.
///
/// They are not the same number. The room can be split by the landing pad a function opens with,
/// since the pad has to be the first instruction after the label and the room does not, so the part
/// in front of the label and the part after it are not always next to each other. What is recorded
/// is the front of the whole thing, which is the part in front of the label when there is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Patch {
    /// Where the room begins, as an offset into the same bytes [`Extent::start`] is one into.
    pub at: usize,
    /// How many bytes of the function are in front of [`Extent::start`], which is where its symbol
    /// is and where an unwinder is told the function begins.
    pub before: usize,
}

/// Where one function ended up.
///
/// How long a function is is a fact ELF records and Mach-O has no way to, so it is handed over
/// rather than worked out again: the writer that wants it has it and the one that does not
/// ignores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extent {
    /// The function's name, as the C program spelled it. The underscore an Apple symbol carries
    /// is the object writer's business, not this one's.
    pub name: String,
    /// Where its first instruction is.
    pub start: usize,
    /// How many bytes of instructions it is, not counting the padding in front of the next one.
    pub len: usize,
    /// What this one function asked to be aligned to, which is not always what the section it is
    /// in was aligned to.
    ///
    /// The two are the same number only when this function is the one that asked for the most.
    /// Under [`Sections::functions`] each function is a section of its own and this is what that
    /// section is aligned to, so the number has to survive the trip rather than be recovered from
    /// the offset, which says nothing once the function is at zero in a section of its own.
    pub align: u32,
    /// How the linker sees the name, which is what the C `static` reaches the object file as.
    pub binding: Binding,
    /// How far outside a shared library holding this the name reaches.
    pub visibility: Visibility,
    /// Where the room a patcher was promised is, or `None` in a function promised none, which is
    /// every function on a command line that did not ask. See [`Patch`].
    pub patch: Option<Patch>,
}

/// The variables a file defines, and what the linker has to be told about them.
///
/// One entry per variable rather than one section of everything, because where a variable goes is
/// worked out from what it is and two of them that land in one section still have their own
/// alignment, their own size and their own symbol. Putting them together is the writer's job and
/// is the one part of it the three formats disagree about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Data {
    /// Every variable this file defines, in the order the module held them.
    pub objects: Vec<Object>,
}

/// A second name for something the same file defines.
///
/// Not a section and not a byte of anything, which is the whole point of it: an alias is a symbol
/// table entry pointing at an address something else already occupies, so a file with one in it is
/// no larger than the same file without. `.set b, a` is what an assembler is told and a second
/// entry at the first one's section, value and size is what a writer produces, and the two say the
/// same thing.
///
/// The target is a name rather than an index into anything above, because the two output paths
/// find it in different places: a listing hands the name to an assembler that resolves it, and a
/// writer looks it up among the symbols it has already added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alias {
    /// The name being defined, as the C program spelled it.
    pub name: String,
    /// The name it stands for, which has to be something this same file defines.
    pub target: String,
    /// How the linker sees the new name, which is not always how it sees the old one: the target
    /// of `extern int b __attribute__((alias("a")))` may be a `static`.
    pub binding: Binding,
    /// How far outside a shared library holding this the new name reaches, which is its own
    /// answer for the same reason the binding is: the attribute is written on the alias.
    pub visibility: Visibility,
}

/// One global variable, laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    /// Its name, as the C program spelled it. The underscore an Apple symbol carries is the
    /// object writer's business, not this one's.
    pub name: String,
    /// Its image, and nothing at all when it is zero filled and the file carries none of it.
    pub bytes: Vec<u8>,
    /// How many bytes it occupies, which is the length of the image except when there is none.
    pub size: u64,
    /// What it has to be aligned to, always a power of two.
    pub align: u64,
    /// Which section it goes in.
    pub place: Place,
    /// How the linker sees the name.
    pub binding: Binding,
    /// How far outside a shared library holding this the name reaches.
    pub visibility: Visibility,
    /// Every place in its image that holds the address of a symbol, counted from the start of
    /// the image rather than from the start of the section it lands in.
    pub relocs: Vec<Reloc>,
}

/// Which section a variable goes in.
///
/// Worked out from what the variable is rather than named by it, except in the one case where the
/// program named it. A reader who wants to know why a variable is in `.rodata` should be able to
/// find the answer in the variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// Written to, and its image is not all zeros. `.data`.
    Written,
    /// Never written to, so it can go in a page the loader maps read only and every process
    /// running the program can share. `.rodata`.
    ReadOnly,
    /// Never written to by the program, but written once by the dynamic linker, because its image
    /// holds the address of something and an address is not known until the image is loaded.
    /// `.data.rel.ro`.
    ///
    /// The section has to be writable for that one write and read only afterwards, which is what
    /// the `PT_GNU_RELRO` segment is: the loader maps it, the relocations are applied, and then it
    /// is turned read only before the program starts. Putting the variable in `.rodata` instead
    /// means asking the linker to leave a relocation in a section that is never writable, and what
    /// it does about that is give the whole image `DT_TEXTREL`, which gives up the protection the
    /// section was for. Some hardened toolchains refuse the link outright.
    RelocReadOnly {
        /// Whether every address in the image is of something this file defines and does not
        /// export, which means the link can resolve them all and none can be interposed.
        ///
        /// Those go in `.data.rel.ro.local`, which the linker puts in the first pages of the
        /// segment, so the pages holding them are the ones the loader is done with soonest. It is
        /// a hint about layout rather than a difference in what the section is.
        local: bool,
    },
    /// All zeros, so the file says how big it is and carries none of it. `.bss`.
    Zero,
    /// A tentative definition, which is not in a section at all: the linker is asked for that
    /// much zeroed space and merges every definition of the name into one. `.comm`.
    Merged,
    /// The section the program named, from `__attribute__((section(...)))`.
    Named(String),
}

impl Place {
    /// What the section this variable goes in is called under [`Sections::data`], and nothing at
    /// all for a variable that has no section of its own to be given.
    ///
    /// The name is the section it would otherwise have shared with a dot and the variable's name
    /// after it, which is what gcc writes and is not merely a convention: `--gc-sections`, the
    /// linker scripts a kernel and an embedded image are linked with, and the default placement
    /// rules all match on the part in front of the dot, so a section called anything else would be
    /// placed by whatever the catch all rule is.
    ///
    /// Two kinds of variable are left alone. A merged one is a request to the linker for that much
    /// zeroed space rather than an image, so there is no section to split, and one the program put
    /// a name on already has the answer the source gave, which this must not overrule.
    ///
    /// Here rather than beside either output path, so that the listing `-S` writes and the object
    /// `-c` writes cannot come to disagree about where a variable went.
    #[must_use]
    pub fn split(&self, name: &str) -> Option<String> {
        Some(format!("{}.{name}", self.base()?))
    }

    /// The section this variable goes in when nothing is being split up, and nothing at all for
    /// the two kinds that are not in one.
    #[must_use]
    pub fn base(&self) -> Option<&'static str> {
        Some(match self {
            Place::Written => ".data",
            Place::ReadOnly => ".rodata",
            Place::RelocReadOnly { local: false } => ".data.rel.ro",
            Place::RelocReadOnly { local: true } => ".data.rel.ro.local",
            Place::Zero => ".bss",
            Place::Merged | Place::Named(_) => return None,
        })
    }
}

/// How the linker sees a name.
///
/// Three of the five linkages the IR has, because that is how many an object file can say. Which
/// of the two weak ones a symbol had is a fact the optimizer needs and the linker does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// Visible to every other object, and the definition here is the definition.
    Global,
    /// Invisible outside this object, which is what `static` at file scope means.
    Local,
    /// Visible, and allowed to lose to a definition in another object.
    Weak,
}

/// How far outside a shared library a name reaches.
///
/// A different question from [`Binding`] and asked of a different linker. The binding is what the
/// static linker does with a name while it is building the output, and this is what the dynamic
/// linker may do with it once the output is a shared library and is being loaded. A hidden name is
/// still global to the static link, so two files in the same library can call each other by it; it
/// is simply not in the dynamic symbol table afterwards, so nothing outside can name it.
///
/// Written down here as its own thing rather than folded into the binding because it is the
/// mistake tamnd/rucc#733 was: a writer that has one word for both ends up saying something about
/// visibility while it thinks it is saying something about linkage, and what it said was hidden.
///
/// It means nothing for a [`Binding::Local`] name. `static` is already invisible to the whole
/// world outside the file, and ELF records `STV_DEFAULT` for one, which is what gcc writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Visibility {
    /// In the dynamic symbol table, and a reference from inside the library may be satisfied by a
    /// definition somewhere else, which is what makes `LD_PRELOAD` work. What a name gets when
    /// nothing said otherwise.
    #[default]
    Default,
    /// Not in the dynamic symbol table at all, so nothing outside the library can name it and
    /// every reference to it from inside binds here. `__attribute__((visibility("hidden")))`.
    Hidden,
    /// In the dynamic symbol table, so something outside can name it, but a reference from inside
    /// the library binds to the definition inside it and cannot be interposed.
    Protected,
}

/// One reference to something this file does not contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reloc {
    /// Where the bytes the linker writes over begin.
    pub at: usize,
    /// What is wanted, as the C program spelled it.
    pub symbol: String,
    /// What the linker is being asked for.
    pub kind: Reference,
    /// What to add to the distance, which is the constant the instruction already meant plus the
    /// bytes between the hole and the end of the instruction, negated. An instruction counts from
    /// where it ends and a relocation counts from where it starts, and this is the difference.
    pub addend: i64,
}

/// What kind of thing a relocation is asking the linker for.
///
/// The first three are the distance from the end of an instruction to something, which is what
/// every reference the code makes is, because this compiler generates position independent code and
/// nothing else. They are told apart by what the linker is allowed to do about each one. The fourth
/// is not a distance at all and is the only kind an image asks for, since an initializer holding the
/// address of something holds the address itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference {
    /// A call, which the linker may satisfy with a stub that reaches further than the four bytes
    /// would. `R_X86_64_PLT32` on ELF, and the same relocation a branch gets on the other two.
    Call,
    /// A datum, reached from the instruction pointer. `R_X86_64_PC32` on ELF.
    Data,
    /// A slot of the global offset table, reached from the instruction pointer, holding the
    /// address of something another object may be the one that defines.
    ///
    /// The distance to the slot rather than to the thing, which is the whole difference: the
    /// distance to the thing is a number only a link that puts the thing in this program can
    /// work out, and a shared library is a link that does not. `R_X86_64_REX_GOTPCRELX` on ELF,
    /// which says the instruction is a `mov` with a REX prefix and lets the linker turn it back
    /// into the `lea` it would have been if the symbol had been here all along.
    Got,
    /// The address itself, written into an image. `int *p = &y;` and nothing else in C.
    Address {
        /// How many bytes of it are written, which is the pointer width except on a target with
        /// a narrower relocation for it. `R_X86_64_64` and `R_X86_64_32` on ELF.
        bytes: u8,
    },
}
