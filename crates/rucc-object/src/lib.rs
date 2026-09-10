//! ELF, Mach-O and COFF object writers.
//!
//! Design: `spec/11-asm-objects-debug.md`. Layer rank 9, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! ELF, which is what Linux and the freestanding targets want and what M3 needs. A text section,
//! the symbols that say where each function in it is and how long it is, the names it wanted that
//! are not in it, the relocations that ask a linker to find them, and the marker whose absence
//! makes the stack executable. An object this writes links with the system linker and runs.
//!
//! The variables a file defines are written too: the section each one goes in, the symbol that
//! says where it is and how long it is, the binding that says who can see it, and the relocations
//! an image asks for when it holds the address of something. A tentative definition is asked of
//! the linker rather than put in a section, which is the one case where a variable has a symbol
//! and no bytes anywhere.
//!
//! An [`Alias`] is written as a second symbol at the first one's section, value and size, with a
//! binding of its own and no second copy of the bytes, which is what a file gets from
//! `__attribute__((alias("target")))` and what makes one name reach another at no cost.
//!
//! [`Sections`] says whether each function and each variable gets a section to itself, which is
//! what `-ffunction-sections` and `-fdata-sections` ask for and what makes `--gc-sections` able to
//! drop anything: a linker can leave out a section nothing reaches and cannot leave out half of
//! one. The names and the offsets are the same either way, so the only thing that moves is which
//! section header a symbol points at.
//!
//! [`Property`] is what the file says it was built to have checked, which is what
//! `-fcf-protection=` asks for. It is written as a note the linker keeps only the agreed part of
//! and the loader reads out of the result, which is why a file that says nothing about it turns
//! the check off for the whole program rather than only for itself.
//!
//! What it is given is [`Text`], [`Data`] and the aliases between them, which are here rather than
//! beside the assembler that fills them in because they are what an object file is made of and
//! because a writer cannot depend on the thing that produces its input without the layer graph
//! going the wrong way round.
//!
//! Mach-O and COFF are not written yet. Both wait on the target that needs them.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-object/0.10.10")]

mod elf;
mod section;

pub use crate::elf::{Error, write};
pub use crate::section::{
    Alias, Binding, Data, Extent, FUNC_ALIGN, Object, Output, Place, Property, Reference, Reloc,
    Sections, Text, Unwind, Visibility,
};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
