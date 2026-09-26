//! ELF and COFF object writers.
//!
//! Design: `spec/11-asm-objects-debug.md`. Layer rank 9, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! ELF, which is what Linux and the freestanding targets want and what M3 needs, and COFF, which is
//! what Windows wants. A text section, the symbols that say where each function in it is and how
//! long it is, the names it wanted that are not in it, the relocations that ask a linker to find
//! them, and the marker whose absence makes the stack executable. An object this writes links with
//! the system linker and runs.
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
//! [`Array`] is what a section of function addresses for the startup code to call is, which is what
//! the `constructor` and `destructor` attributes produce. ELF has a section type for each of the
//! three kinds, and a section of the ordinary type under one of those names is gathered by the
//! linker in the same run and called by nobody, so the type is written out rather than left to the
//! default. COFF has nothing of the sort under those names, so a file with one is refused for a
//! Windows target rather than written with its constructors never run.
//!
//! [`Property`] is what the file says it was built to have checked, which is what
//! `-fcf-protection=` asks for. It is written as a note the linker keeps only the agreed part of
//! and the loader reads out of the result, which is why a file that says nothing about it turns
//! the check off for the whole program rather than only for itself.
//!
//! [`defines`] says which names a linker can find in what [`write()`] wrote, which is what the symbol
//! index of an archive is built from. It is here rather than worked out by whoever writes the
//! archive because the index has to agree with the member, and the writer is the only thing that
//! knows what it put in one.
//!
//! What it is given is [`Text`], [`Data`] and the aliases between them, which are here rather than
//! beside the assembler that fills them in because they are what an object file is made of and
//! because a writer cannot depend on the thing that produces its input without the layer graph
//! going the wrong way round.
//!
//! [`assembled`] is the other way in, for an object written from a file of assembly rather than
//! from a compilation. It takes [`Assembled`], which is a list of sections that each carry their
//! own name and flags and a flat list of names that point into them at offsets. That is a different
//! shape from [`Text`] and [`Data`] because a file of assembly says things neither of them can hold:
//! which section something is in, a name at an offset that no variable covers, and a name that is a
//! number rather than a place. Both go through the same writer underneath, so there is still one
//! place that knows how a file is laid out.
//!
//! An unwind table is written for both, and the two formats want it laid out differently: ELF has
//! one section of records that each carry their own codes, and Windows has a table of fixed rows in
//! `.pdata` pointing at the descriptions in `.xdata`. Both come in as bytes and relocations, because
//! what a record is is the platform's answer and the layer that knows what a frame did is the one
//! that can say it. Thread-local storage, a reference through a global offset table and a record of
//! where a patcher's room is are refused for COFF, each being something that format has no way to
//! write rather than something not written yet.
//!
//! Mach-O is not written yet. It waits on the target that needs it.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-object/0.11.11")]

mod coff;
mod elf;
mod file;
mod section;
mod source;

pub use crate::file::{Error, defines, write};
pub use crate::section::{
    Alias, Apart, Array, Binding, Chunk, Data, Extent, FUNC_ALIGN, Info, Marker, Object, Output,
    Patch, Place, Property, Reference, Reloc, Sections, Table, Text, Unwind, Visibility,
};
pub use crate::source::{
    Assembled, Held, Name, Part, Shape, Sort, assembled, assembled_defines, assembled_described,
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
