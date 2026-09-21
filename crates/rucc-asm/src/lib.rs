//! Instruction encoders, the integrated assembler, inline assembly and relaxation.
//!
//! Design: `spec/11-asm-objects-debug.md`. Layer rank 11, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! What is written are the two things a compiler does with a machine function: the assembly text
//! `-S` produces, which is [`print()`], and the bytes of a text section, which is [`assemble`].
//! Section 11.1 asks for one instruction description behind both, and there is one: the walk over
//! a function is the same walk in both files, reading the same list out of `rucc-target`, and the
//! only difference is whether an instruction is written down by name or handed to the encoder. So
//! the listing and the object file cannot come to disagree about what an instruction is.
//!
//! What [`assemble`] hands back with the bytes is what the linker has to be told: where each
//! function starts and how long it is, and every place in the bytes that names something this
//! file does not contain. The jumps inside a function are not among them, because by the end of a
//! function every block has a place and they are filled in here.
//!
//! The variables a file defines are here for the same reason and in the same shape. [`globals`] is
//! the one walk over a module's globals, and what it gives back is a list of pieces that
//! [`print()`] writes down as directives and [`Globals::image`] writes down as bytes, so a `.long`
//! in a listing and the four bytes in the object beside it cannot come to disagree either. Where a
//! variable goes is worked out there rather than named by the front end, and what a section is
//! called is the object format's business.
//!
//! [`read`] is the other direction: a file of assembly that somebody else wrote, turned into the
//! sections and names an object is written from. The directives and the labels are one half of it
//! and the instructions are the other, and a mnemonic with no bytes behind it is refused by name
//! with its line number rather than skipped. Nothing there describes the machine a second time:
//! the bytes of an instruction come from the one encoder in `rucc-target` that the compiler's own
//! output goes through, so a file this assembles and a file this compiles cannot disagree about
//! what an instruction is. Branch relaxation is not here yet, so a jump is four bytes of distance
//! whether it needs them or not, which is correct and longer than gas would have written.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-asm/0.10.69")]

mod att;
mod bytes;
mod data;
mod format;
mod instruction;
mod source;
mod unwind;

pub use crate::att::print;
pub use crate::bytes::assemble;
pub use crate::data::{Globals, Piece, Variable, aliases, globals};
pub use crate::format::Directives;
pub use crate::source::{Trouble, read};

use std::fmt;

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

/// A function this compiler could not write out as assembly.
///
/// Neither of these is a program's fault and neither should ever reach a user, since a machine
/// function that reaches here has been through the whole backend and the tests pin both of the
/// claims below. They are errors rather than assertions because the alternative to reporting one
/// is writing a listing that is quietly wrong, and a wrong listing is the failure section 11.1 is
/// written to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// An opcode the target has no description of.
    Opcode {
        /// The function it turned up in.
        func: String,
        /// The opcode, as the machine IR spells it.
        opcode: String,
    },
    /// A register that is still virtual, which is a function that was never allocated.
    Virtual {
        /// The function it turned up in.
        func: String,
        /// The opcode the register is an operand of.
        opcode: String,
    },
    /// An instruction the description names and the encoder could not write bytes for.
    ///
    /// The two halves of the description are meant to hold the same instructions, and a test
    /// pins that they do, so this is either a row that was left out of one of them or an
    /// operand the machine cannot express in the instruction that was chosen for it.
    Encode {
        /// The function it turned up in.
        func: String,
        /// The opcode, as the machine IR spells it.
        opcode: String,
        /// What the encoder said, already formatted.
        why: String,
    },
    /// A jump inside a function to somewhere more than two gigabytes away.
    ///
    /// A single function that long is not a program anybody wrote, and the four bytes a jump
    /// carries are all there are, so this is reported rather than wrapped around into a jump
    /// somewhere else entirely.
    Distance {
        /// The function it turned up in.
        func: String,
        /// How far the jump would have had to reach.
        bytes: i64,
    },
    /// A machine this crate cannot write assembly for.
    Machine {
        /// The triple that was asked for.
        triple: String,
    },
    /// A thread-local variable on a format that does not spell one the way ELF does.
    ///
    /// The only one of these that is about a program rather than about this compiler. ELF says a
    /// thread-local variable with a section flag and a symbol type, and that is written. Windows
    /// hands out an index at load time and reaches the variable through a table the index names,
    /// and Mach-O puts a descriptor in front of every one and reaches it by calling through the
    /// descriptor, so on those two one is refused rather than written out as an ordinary variable
    /// that every thread would share.
    Thread {
        /// The variable, as the C program spelled it.
        name: String,
        /// The object format that has no writing of one here, as its own name.
        format: &'static str,
    },
    /// An ifunc, which is not a mistake and not written yet.
    ///
    /// The other thing an alias in the IR can be, and a different job from a second name for
    /// something: the symbol is resolved once at program start by calling a function in this
    /// object, which wants a symbol type of its own and a relocation of its own. One is refused
    /// rather than written as an ordinary alias that would go to the resolver instead of to what
    /// the resolver picked.
    IFunc {
        /// The name it defines, as the C program spelled it.
        name: String,
    },
    /// A prologue the target's unwind table has no way to describe.
    ///
    /// ELF carries a little program per function and can say anything an instruction did to the
    /// frame. Windows carries a fixed list of codes instead, each one of a handful of shapes a
    /// prologue is allowed to have, and a prologue outside that list has no spelling there. The
    /// one this compiler writes that does not fit is the frame pointer form, which establishes the
    /// pointer before it takes the frame, so the table is refused rather than written describing a
    /// frame of the wrong size. A build that does not want a table at all is the way past it, which
    /// is `-fno-asynchronous-unwind-tables -fno-unwind-tables`.
    Frame {
        /// The function it turned up in.
        func: String,
        /// What about its prologue, already formatted.
        why: String,
    },
    /// A piece of an initializer nothing here can write down.
    Image {
        /// The variable it is part of.
        name: String,
        /// What about it, already formatted.
        why: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Opcode { func, opcode } => {
                write!(f, "'{func}' has a '{opcode}' and the target does not say what one is")
            }
            Error::Virtual { func, opcode } => {
                write!(f, "'{func}' reached the assembler with a virtual register in a '{opcode}'")
            }
            Error::Encode { func, opcode, why } => {
                write!(f, "'{func}' has a '{opcode}' the encoder refused: {why}")
            }
            Error::Distance { func, bytes } => {
                write!(f, "'{func}' has a jump reaching {bytes} bytes, which does not fit in four")
            }
            Error::Machine { triple } => {
                write!(f, "there is no assembly writer for {triple} in this compiler yet")
            }
            Error::Thread { name, format } => {
                write!(f, "'{name}' is thread-local, which is not written on {format} yet")
            }
            Error::IFunc { name } => {
                write!(f, "'{name}' is an ifunc, which this compiler does not write yet")
            }
            Error::Frame { func, why } => {
                write!(f, "'{func}' has {why}, which no unwind table here can describe")
            }
            Error::Image { name, why } => {
                write!(f, "the initializer of '{name}' has {why} in it, which cannot be written")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
