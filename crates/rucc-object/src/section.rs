//! What an object writer is given, which is a section of bytes and what the linker has to be
//! told about them.
//!
//! Design: `spec/11-asm-objects-debug.md` sections 11.1 and 11.3.
//!
//! These types are here rather than beside the assembler that fills them in because they are what
//! an object file is made of, and because a writer cannot depend on the thing that produces its
//! input without the graph going the wrong way round. The assembler at layer rank 10 reaches down
//! to these at rank 8, which is the direction `spec/18-package-layout.md` asks for.

/// A text section, and what the linker has to be told about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    /// The instructions, in the order they were laid out.
    pub bytes: Vec<u8>,
    /// Where each function starts and how long it is, in the order they were written.
    pub funcs: Vec<Extent>,
    /// Every place in the bytes that names something the linker has to find.
    pub relocs: Vec<Reloc>,
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
}

/// One reference to something this file does not contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reloc {
    /// Where the four bytes the linker writes over begin.
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
/// Both are the distance from the end of an instruction to something, which is what every
/// reference this compiler makes is, because it generates position independent code and nothing
/// else. They are told apart because the linker may answer one of them with a stub and may not
/// answer the other one that way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference {
    /// A call, which the linker may satisfy with a stub that reaches further than the four bytes
    /// would. `R_X86_64_PLT32` on ELF, and the same relocation a branch gets on the other two.
    Call,
    /// A datum, reached from the instruction pointer. `R_X86_64_PC32` on ELF.
    Data,
}
