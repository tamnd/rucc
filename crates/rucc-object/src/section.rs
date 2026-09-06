//! What an object writer is given, which is a section of bytes and what the linker has to be
//! told about them.
//!
//! Design: `spec/11-asm-objects-debug.md` sections 11.1 and 11.3.
//!
//! These types are here rather than beside the assembler that fills them in because they are what
//! an object file is made of, and because a writer cannot depend on the thing that produces its
//! input without the graph going the wrong way round. The assembler at layer rank 10 reaches down
//! to these at rank 8, which is the direction `spec/18-package-layout.md` asks for.

/// What a function is aligned to when nothing asked for more.
///
/// Sixteen because that is what every x86-64 toolchain puts a function at, and because it is what
/// keeps the loop inside one from straddling one more cache line than it has to. Here rather than
/// beside the assembler because the assembler pads to it and the writer records it, and two
/// copies of one number is how the padding and the record come apart.
pub const FUNC_ALIGN: u32 = 16;

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
}

impl Default for Text {
    fn default() -> Self {
        Self { bytes: Vec::new(), funcs: Vec::new(), relocs: Vec::new(), align: FUNC_ALIGN }
    }
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
    /// How the linker sees the name, which is what the C `static` reaches the object file as.
    pub binding: Binding,
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
    /// All zeros, so the file says how big it is and carries none of it. `.bss`.
    Zero,
    /// A tentative definition, which is not in a section at all: the linker is asked for that
    /// much zeroed space and merges every definition of the name into one. `.comm`.
    Merged,
    /// The section the program named, from `__attribute__((section(...)))`.
    Named(String),
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
/// The first two are the distance from the end of an instruction to something, which is what every
/// reference the code makes is, because this compiler generates position independent code and
/// nothing else. They are told apart because the linker may answer one of them with a stub and may
/// not answer the other one that way. The third is not a distance at all and is the only kind an
/// image asks for, since an initializer holding the address of something holds the address itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference {
    /// A call, which the linker may satisfy with a stub that reaches further than the four bytes
    /// would. `R_X86_64_PLT32` on ELF, and the same relocation a branch gets on the other two.
    Call,
    /// A datum, reached from the instruction pointer. `R_X86_64_PC32` on ELF.
    Data,
    /// The address itself, written into an image. `int *p = &y;` and nothing else in C.
    Address {
        /// How many bytes of it are written, which is the pointer width except on a target with
        /// a narrower relocation for it. `R_X86_64_64` and `R_X86_64_32` on ELF.
        bytes: u8,
    },
}
