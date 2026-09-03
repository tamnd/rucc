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
//! What it is given is [`Text`], which is here rather than beside the assembler that fills it in
//! because it is what an object file is made of and because a writer cannot depend on the thing
//! that produces its input without the layer graph going the wrong way round.
//!
//! Mach-O and COFF are not written yet, and neither is any section other than the text. Both wait
//! on the target and the global variables that need them.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-object/0.3.5")]

mod elf;
mod section;

pub use crate::elf::{Error, write};
pub use crate::section::{Extent, Reference, Reloc, Text};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
