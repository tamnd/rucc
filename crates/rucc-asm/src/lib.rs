//! Instruction encoders, the integrated assembler, inline assembly and relaxation.
//!
//! Design: `spec/11-asm-objects-debug.md`. Layer rank 10, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! What is written is the assembly text `-S` produces, which section 11.1 asks for because
//! people read it. It is written from the instruction description in `rucc-target`, the same one
//! the encoder will be generated from, so the listing and the object file cannot come to
//! disagree about what an instruction is.
//!
//! The encoder itself, the assembler that reads `.s` and `.S`, inline assembly and relaxation
//! are the rest of M3 and M4 and are not here yet.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-asm/0.3.4")]

mod att;
mod format;

pub use crate::att::print;
pub use crate::format::Directives;

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
    /// A machine this crate cannot write assembly for.
    Machine {
        /// The triple that was asked for.
        triple: String,
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
            Error::Machine { triple } => {
                write!(f, "there is no assembly writer for {triple} in this compiler yet")
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
