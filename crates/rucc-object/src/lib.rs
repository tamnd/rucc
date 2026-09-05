//! ELF, Mach-O and COFF object writers.
//!
//! Design: `spec/11-asm-objects-debug.md`. Layer rank 8, see `spec/18-package-layout.md`.
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
//! What it is given is [`Text`] and [`Data`], which are here rather than beside the assembler that
//! fills them in because they are what an object file is made of and because a writer cannot
//! depend on the thing that produces its input without the layer graph going the wrong way round.
//!
//! Mach-O and COFF are not written yet. Both wait on the target that needs them.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-object/0.4.1")]

mod elf;
mod section;

pub use crate::elf::{Error, write};
pub use crate::section::{
    Binding, Data, Extent, FUNC_ALIGN, Object, Place, Reference, Reloc, Text,
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
