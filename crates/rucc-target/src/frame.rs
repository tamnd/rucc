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
//! the stack pointer is two-address, so it writes the stack pointer and reads it back, whether
//! the amount is a constant or a register. A target whose instructions do not fit those shapes
//! needs more than a table, and it will say so by not being able to fill this in.

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

/// The two instructions that put two registers on the stack in one go and take them back.
///
/// AArch64 has these as `stp` and `ldp` with a writeback, and it is how the frame pointer and the
/// link register go on the stack together as the frame record. The first register named is the one
/// that ends up at the lower address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pair {
    /// Stores two registers below the stack pointer and moves it down past both.
    pub push: &'static str,
    /// Loads two registers from the stack pointer and moves it back up past both.
    pub pop: &'static str,
}

/// Every instruction a prologue, an epilogue, a spill or a reload is made of.
#[derive(Debug, Clone, Copy)]
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
    /// Pushes two registers at once, or `None` on a machine that pushes one at a time. See
    /// [`Pair`].
    pub pair: Option<Pair>,
    /// Adds a constant to the stack pointer, which is how an epilogue gives the frame back.
    pub add: &'static str,
    /// Takes a constant off the stack pointer, which is how a prologue takes the frame.
    pub sub: &'static str,
    /// Takes whatever is in a register off the stack pointer, which is how a function makes room
    /// for an array whose size it does not know until it runs.
    ///
    /// The same shape as [`Self::sub`] and different in where the amount comes from, which is the
    /// whole of the difference between the bytes a prologue takes and the bytes a variable length
    /// array takes. A prologue knows its number when it is written and a declaration in the body
    /// does not know it until the expression in the brackets has been worked out.
    pub grow: &'static str,
    /// Clears the low bits of the stack pointer, which is how a prologue forces an alignment
    /// nothing else can give it.
    pub align: &'static str,
    /// Writes a constant into a general purpose register.
    ///
    /// The one thing a prologue has to do that is not about the stack pointer, and it is here for
    /// a platform that hands the size of the frame to a routine rather than reaching the pages
    /// itself. See [`crate::Chkstk`]. A rule file selects this same opcode for a constant the
    /// program wrote, for the reason the header of a target's table gives: a prologue writing a
    /// number into a register is the same instruction as an assignment, and the encoder should
    /// not have two answers for it.
    pub imm: &'static str,
    /// Writes a register with an address rather than with what is at it, which is how an
    /// epilogue puts the stack pointer back when the frame pointer is the only record of where
    /// it was.
    pub lea: &'static str,
    /// Adds two general purpose registers at the width of an address.
    ///
    /// Not something a prologue writes. It is here because the sum of two registers is the one
    /// address the selector leaves as arithmetic rather than as a `lea`, and the pass that folds
    /// addresses into their readers has to know which instruction that is to read it as a base and
    /// an index at a scale of one.
    pub sum: &'static str,
    /// Returns to the caller.
    pub ret: &'static str,
    /// Compares two general purpose registers and writes whether they differ into a third.
    ///
    /// The stack protector's check is the only thing that asks for this, and it is here rather
    /// than left to a lowering rule because no rule ever sees the comparison: the two words being
    /// compared are the canary the prologue wrote and the one the runtime still holds, and neither
    /// of them is a value the program named.
    pub differ: &'static str,
    /// Compares two general purpose registers as unsigned numbers and writes whether the first is
    /// above the second into a third.
    ///
    /// Here for the same reason [`Self::differ`] is, and asked for by the one loop that walks a
    /// distance nothing knew when it was written, which is the pages a variable length array takes.
    /// A prologue knows how many pages its own frame is and can stop when the stack pointer reaches
    /// an address worked out in advance, so equality is enough for it. A declaration in the body
    /// does not: the bytes arrive in a register, the last step down is a whole page whatever is
    /// left, and the stack pointer lands at or past where it was going rather than on it.
    ///
    /// Unsigned because both registers hold addresses. A stack that has grown past the middle of
    /// the address space is one where a signed comparison of two stack pointers says the wrong
    /// thing, and nothing about a guard page cares which half of the space it is in.
    pub above: &'static str,
    /// Jumps to the name it is given, which is how a tail call ends, or `None` on a target where
    /// nothing writes one yet and every call in tail position stays a call.
    pub away: Option<&'static str>,
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
    /// What says an indirect branch may arrive at an address, or `None` on a target where nothing
    /// does.
    ///
    /// What `-fcf-protection=branch` asks for, and an option for the same reason [`Self::probe`]
    /// is: a target with no such instruction is one the flag cannot be honoured on, and the answer
    /// there is to say so rather than to write a name that stands for nothing. A prologue puts one
    /// at the top of every function, because a function's own address is the one address of it a
    /// pointer can hold, and one goes at the top of every label a program took the address of,
    /// because a computed `goto` is an indirect branch and those are the addresses it arrives at.
    pub landing: Option<&'static str>,
    /// A byte that does nothing, or `None` on a target where nothing is written for the purpose.
    ///
    /// What `-fpatchable-function-entry=` reserves room with, and an option for the same reason
    /// [`Self::landing`] is. The room is counted in bytes, so what is wanted is the shortest
    /// instruction the machine has that does nothing rather than the shortest sequence that adds
    /// up to the length: a patcher writes over the room from its start and wants a whole number of
    /// places it could have started at.
    pub pad: Option<&'static str>,
    /// How many bits of constant [`Self::add`] and [`Self::sub`] carry, when a frame can need
    /// more, or `None` when they carry any size a frame can be.
    ///
    /// AArch64's carry twelve bits, or twelve bits shifted up by twelve, which is the machine's
    /// whole answer and not a form the encoder has yet to learn. A frame of 4608 bytes is taken as
    /// 4096 and then 512, which is what gcc writes, and [`Self::steps`] is the rule for that.
    pub step_bits: Option<u32>,
    /// Whether the instruction of that name can carry that displacement from the stack pointer or
    /// the frame pointer, or `None` on a target where every offset a frame has fits.
    ///
    /// An AArch64 load reaches 4095 bytes, or that many of its own size, and `add` carries twelve
    /// bits, so a local more than a few kilobytes into a large frame is out of reach of the one
    /// instruction the lowering wrote for it. What reaches is the encoder's to say, since it is
    /// the one that refuses, and the finish pass asks it and writes the address into a scratch
    /// register first when the answer is no.
    pub reaches: Option<fn(&str, i32) -> bool>,
}

impl FrameInsts {
    /// The amounts one [`Self::add`] or [`Self::sub`] each moves the stack pointer by, which
    /// together move it by `bytes`.
    ///
    /// One step on a machine whose instruction carries the whole of it. Otherwise the part above
    /// the low bits goes first, in steps as large as the shifted form holds, and the low bits
    /// last, so a frame under sixteen megabytes on AArch64 is at most two instructions.
    #[must_use]
    pub fn steps(&self, bytes: u32) -> Vec<u32> {
        let Some(bits) = self.step_bits else { return vec![bytes] };
        let low = bytes & ((1 << bits) - 1);
        let most = ((1 << bits) - 1) << bits;
        let mut high = bytes - low;
        let mut steps = Vec::new();
        while high > 0 {
            let step = high.min(most);
            steps.push(step);
            high -= step;
        }
        if low > 0 || steps.is_empty() {
            steps.push(low);
        }
        steps
    }

    /// How a register of that class is moved, or `None` for a class nothing spills.
    #[must_use]
    pub fn moves(&self, class: RegClass) -> Option<ClassMoves> {
        self.classes.get(usize::from(class.number())).copied()
    }
}
