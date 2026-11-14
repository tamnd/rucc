//! The Plan 9 assemblers `6a` and `7a`, as a part of this crate.
//!
//! Design: 07.6 of the rucc-plan9 plan.
//!
//! These assemblers read the assembly language of Plan 9 and write the same object as the C
//! programs at the pin, byte for byte. The lexer and the macro processor are in `lex`, and each
//! machine has its grammar and its object writer.

mod itab6;
mod itab7;
mod lex;
mod seven;
mod six;
mod tab7;
mod yacc;
mod ydebug6;
mod ydebug7;

pub use lex::{Reader, namelenerror};
pub use seven::assemble as assemble7;
pub use six::assemble as assemble6;
