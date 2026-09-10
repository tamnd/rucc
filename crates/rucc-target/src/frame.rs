//! The instructions a frame is made of.
//!
//! Design: `spec/10-backend.md` sections 10.7 and 10.8.
//!
//! A prologue pushes registers and moves the stack pointer, an epilogue puts them back, and a
//! spill is a store and a reload is a load. None of that is chosen by a lowering rule, because
//! none of it comes from anything the program wrote: it comes from how many registers the
//! allocator ran out of and which of them the convention says a call leaves alone. So the
//! opcodes are named here, which is the list of what a frame may produce, rather than only in
//! [`crate::x86_64::INSTS`], which is the list of what the selector may produce and what the
//! allocator therefore has to understand. The encoder reads both.
//!
//! Some names are in both lists, which is not a duplication of anything. A load is a load
//! whether a rule selected it or a reload wrote it, and the instruction description in `INSTS`
//! is what the allocator reads about the one the selector produced. What the two lists are is
//! two answers to two questions, and an instruction being an answer to both is ordinary. What
//! would be a mistake is a frame opcode nobody has described anywhere, which is why an entry
//! here that is not in `INSTS` is still an entry the encoder has to know.
//!
//! Everything named here is a name rather than a variant, for the same reason
//! `rucc_mir::Opcode` is: the crate that writes the prologue is a pipeline crate and
//! `spec/10-backend.md` section 10.8 says a pipeline crate holds no target-specific code. It
//! reads the names out of the target it was handed and writes them into the machine IR, and what
//! any of them means is the encoder's answer against this same description.
//!
//! # What each one has to be
//!
//! The shapes are fixed, because the code that writes them writes one shape each. A push reads
//! one register and a pop writes one. A move writes a register and reads another of the same
//! class. A load writes a register and reads memory, a store reads a register and writes memory,
//! and both reach the frame through the stack pointer with a constant added. The arithmetic on
//! the stack pointer is two-address, so it writes the stack pointer and reads it back. A target
//! whose instructions do not fit those shapes needs more than a table, and it will say so by not
//! being able to fill this in.

use crate::regs::RegClass;

/// How a register of one class is moved between two registers and between a register and the
/// frame.
///
/// Three names rather than one, because a machine that moves a general purpose register with
/// `mov` moves a vector register with something else, and because a load and a store are
/// different instructions on every machine here even when a dump writes them with the same
/// mnemonic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassMoves {
    /// Writes the first register with what is in the second.
    pub mov: &'static str,
    /// Writes the register with what is in the frame.
    pub load: &'static str,
    /// Writes the frame with what is in the register.
    pub store: &'static str,
}

/// How a prologue touches the stack as it takes a frame, on a target that can.
///
/// What `-fstack-clash-protection` asks for. An operating system leaves one page below every
/// stack unmapped, so that a stack which grows into it faults rather than running into whatever
/// is under it, and a frame larger than that page can move the stack pointer clean over it
/// without ever writing to it. A prologue that takes the frame a page at a time and writes
/// something to each page as it arrives cannot: the first page it reaches that is not mapped is
/// the one that faults.
///
/// Two facts rather than one because neither implies the other. What touches a page is an
/// instruction of the machine, and how far apart the pages are is what the kernel that runs the
/// program left, and they are together here because a prologue that has one and not the other
/// cannot write anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// Writes an address without changing what is there, which is what makes it safe to do to a
    /// page a local has not been put in yet.
    pub inst: &'static str,
    /// How far apart the touches are, which is the page the operating system leaves below the
    /// stack. A prologue never moves the stack pointer further than this without touching where
    /// it landed.
    pub interval: u32,
}

/// Every instruction a prologue, an epilogue, a spill or a reload is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInsts {
    /// What a rule file and the machine IR put in front of this target's opcodes, such as
    /// `x64.`, which says which target a term belongs to and is not part of the opcode.
    pub prefix: &'static str,
    /// How a register of each class is moved, one for each class of the register file in the
    /// order the file numbers them.
    ///
    /// Shorter than the file when the classes at the end are ones nothing spills. An x87 stack
    /// register is one of those: the allocator is never given one to hand out, so nothing ever
    /// asks how to move it, and a target that answered anyway would be writing down a guess.
    pub classes: &'static [ClassMoves],
    /// Puts a register on the stack and moves the stack pointer down by one word.
    pub push: &'static str,
    /// Takes a word off the stack into a register and moves the stack pointer back up.
    pub pop: &'static str,
    /// Adds a constant to the stack pointer, which is how an epilogue gives the frame back.
    pub add: &'static str,
    /// Takes a constant off the stack pointer, which is how a prologue takes the frame.
    pub sub: &'static str,
    /// Clears the low bits of the stack pointer, which is how a prologue forces an alignment
    /// nothing else can give it.
    pub align: &'static str,
    /// Writes a register with an address rather than with what is at it, which is how an
    /// epilogue puts the stack pointer back when the frame pointer is the only record of where
    /// it was.
    pub lea: &'static str,
    /// Returns to the caller.
    pub ret: &'static str,
    /// Compares two general purpose registers and writes whether they differ into a third.
    ///
    /// The stack protector's check is the only thing that asks for this, and it is here rather
    /// than left to a lowering rule because no rule ever sees the comparison: the two words being
    /// compared are the canary the prologue wrote and the one the runtime still holds, and neither
    /// of them is a value the program named.
    pub differ: &'static str,
    /// Calls the name it is given and reads no register.
    ///
    /// Here for the same reason, and used for the one call an epilogue can make, which is the one
    /// a changed canary makes.
    pub call: &'static str,
    /// How a prologue touches a page of the stack, or `None` on a target where nothing can.
    ///
    /// See [`Probe`]. It is an option rather than a name because a target that has no such
    /// instruction is a target where `-fstack-clash-protection` has to do nothing, and a name
    /// standing for nothing is worse than an absence a caller has to look at.
    pub probe: Option<Probe>,
}

impl FrameInsts {
    /// How a register of that class is moved, or `None` for a class nothing spills.
    #[must_use]
    pub fn moves(&self, class: RegClass) -> Option<ClassMoves> {
        self.classes.get(usize::from(class.number())).copied()
    }
}
