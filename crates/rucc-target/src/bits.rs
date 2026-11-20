//! How much of a register each instruction reads and writes.
//!
//! Design: `spec/optimizer/37-machine-level-optimization.md` section 37.4.
//!
//! A register is one register at every width, and what says how much of it an instruction is
//! about is the instruction. `movzbl %sil, %esi` writes thirty two bits and reads eight of them,
//! `movb %sil, %r12b` writes eight and reads eight, and put the two next to each other and the
//! twenty four bits the first one worked out are bits nothing reads. Finding that out is the bit
//! group liveness of `gcc/ext-dce.cc`, which tracks what is live per group of bits rather than
//! per register, and what it needs from a target is this: for each operand of each instruction,
//! how much of it the instruction names.
//!
//! It is here rather than in the pass for the reason [`crate::FrameInsts`] and
//! [`crate::BranchInsts`] are here. The pass is in a pipeline crate and
//! `spec/10-backend.md` section 10.8 says a pipeline crate holds no target-specific code, so what
//! the pass knows about a machine arrives as a description rather than as a name it says out
//! loud.
//!
//! # Why two questions rather than a table of instructions
//!
//! Neither question is about a particular instruction. The first is how wide an operand is, which
//! every instruction of every target has an answer to, and the second is whether an instruction
//! is one that copies the low bits of its source into its destination, which is the one family
//! the transformation acts on. A target with no widening instructions answers `false` to the
//! second everywhere and the pass finds nothing, which is the right answer rather than a special
//! case.

/// What a pass has to know about a machine to work out which bits of a register anything reads.
///
/// Both are functions of an opcode's name rather than tables, because a target already has the
/// table both of them read: the assembly listing names each operand at the width the instruction
/// uses it at, and the encoder is generated from the same description, so the widths here are the
/// ones the machine really has rather than a second opinion about them.
#[derive(Debug, Clone, Copy)]
pub struct BitInsts {
    /// What a rule file and the machine IR put in front of this target's opcodes, such as `x64.`.
    pub prefix: &'static str,
    /// How many bits of the operand at that index the instruction of that name uses.
    ///
    /// `None` means all of it, which is the answer for an operand the target's description does
    /// not name: a register inside an addressing mode, an operand of an opcode that encodes to
    /// nothing, and an opcode this target does not have. Answering "all of it" where nothing is
    /// known is what keeps the analysis from believing bits are dead because a description was
    /// silent about them.
    pub width: fn(&str, u8) -> Option<u32>,
    /// Whether the instruction of that name puts the low bits of its one source into its one
    /// destination, and nothing else.
    ///
    /// The widenings and the narrowings, and nothing else. It is what makes the instruction one
    /// the pass may take out when the bits above the source's width turn out to be read by
    /// nobody, since what is left of it then is a copy.
    pub copies_low: fn(&str) -> bool,
}
