//! What a machine level pass has to ask before it may keep a rewrite.
//!
//! Design: `spec/optimizer/37-machine-level-optimization.md` sections 37.2 and 37.3.
//!
//! A machine level rewrite is speculative. Section 37.3 quotes `gcc/combine.cc` on the shape of
//! it: substitute the earlier instruction into the later one, ask the machine description whether
//! what came out is an instruction this target has, install it if it is and put everything back if
//! it is not. GCC's word for the asking is `recog`, the machine description is the `.md` file, and
//! the reason the arrangement is worth copying is in the same section: a target that adds a
//! pattern makes the optimizer smarter without anybody editing the optimizer.
//!
//! This is that question for this compiler. A pass proposes an instruction and asks whether the
//! target has one of that shape, and a target answers out of the description it already keeps for
//! the allocator and the encoder rather than out of a second list written for this. Two lists
//! would be two opinions about one machine and they would disagree eventually.
//!
//! # What it does not answer
//!
//! Whether the rewrite is worth making. That is the pass's own question and no target has an
//! opinion about it.
//!
//! Whether the values are right. An instruction of a shape this target has can still read the
//! wrong register, and what checks that is `rucc_regalloc::check` after allocation and the shape
//! of the pass before it. This says the instruction exists and nothing further.

use crate::operand::OperandDesc;

/// What a pass has to know about a machine to tell an instruction it has from one it does not.
///
/// Every question is of a name, for the reason [`crate::BitInsts`] takes names: the pass is in a
/// pipeline crate, `spec/10-backend.md` section 10.8 says a pipeline crate holds no target
/// specific code, so an opcode is a name to it and what any name means is the target's answer.
#[derive(Debug, Clone, Copy)]
pub struct MachineInsts {
    /// What a rule file and the machine IR put in front of this target's opcodes, such as `x64.`.
    pub prefix: &'static str,
    /// The operands an instruction of that name has, the ones it writes before the ones it reads.
    ///
    /// `None` for a name this target does not have, which is the answer that makes a proposal
    /// naming an opcode nobody described a proposal the target refuses rather than one it lets
    /// through with no opinion.
    ///
    /// The registers an addressing mode names are not in here, for the reason they are not in
    /// `rucc_target::x86_64::Form::operands`: they are operands and the allocator rewrites them
    /// like any other, but which operand each of them is belongs to the addressing mode.
    pub operands: fn(&str) -> Option<&'static [OperandDesc]>,
    /// Whether an instruction of that name carries an immediate.
    pub takes_imm: fn(&str) -> bool,
    /// Whether an instruction of that name carries an addressing mode.
    pub takes_mem: fn(&str) -> bool,
    /// Whether an instruction of that name reads or writes memory.
    ///
    /// Asked by a pass moving a memory access from where it is to somewhere later, which is safe
    /// while nothing it passes touches memory at all. Reading and writing are one question rather
    /// than two, because moving a read past a read is still a reordering of two accesses, and
    /// machine IR does not say which accesses the program insisted on: a `volatile` read and an
    /// ordinary one are the same instruction with the same operands by the time a pass here sees
    /// them.
    ///
    /// It is a coarser answer than an alias analysis would give and a target does not have to know
    /// anything it does not already know to give it. Whether two addresses are the same place is a
    /// question nothing below selection has an analysis for, so the answer to arriving at one is to
    /// stop rather than to guess.
    ///
    /// A call answers `true` here and that is still not the whole answer about a call. What a call
    /// does to memory is not in the instruction at all, which is why [`Self::calls`] is a separate
    /// question and why a pass that has to know what survived one has to ask that as well.
    pub touches_mem: fn(&str) -> bool,
    /// Whether an instruction of that name is a call.
    ///
    /// Asked by a pass that has to know which registers an instruction leaves alone, because a
    /// call is the one instruction whose operands do not answer that. The registers a convention
    /// does not preserve are gone across one, and the ones an argument travelled in are written
    /// down as reads rather than as writes, so a pass reading the operand vector would be told a
    /// value in an argument register survives a call it does not survive.
    pub calls: fn(&str) -> bool,
    /// What an addressing mode on this target may multiply its index by.
    ///
    /// A list rather than a range because the machines that have an index have a handful of
    /// scales and not an interval, and a pass folding an address into a memory operand has to ask
    /// whether the number it worked out is one of them.
    pub scales: &'static [u8],
    /// Whether an addressing mode with an index may have a displacement beside it.
    ///
    /// x86-64 adds all three in one mode. AArch64 adds a base to a constant or to a register and
    /// not to both, so a pass that would fold an `add` of two registers into a load that already
    /// has an offset is proposing an address that machine has no way to write.
    pub index_and_disp: bool,
}

impl MachineInsts {
    /// The name with this target's prefix taken off, which is how its own description spells it.
    ///
    /// The machine IR holds the prefixed spelling and every table behind these functions is
    /// written without it, so this is the one place the two spellings meet.
    #[must_use]
    pub fn bare<'a>(&self, name: &'a str) -> &'a str {
        name.strip_prefix(self.prefix).unwrap_or(name)
    }

    /// Whether this target has an instruction of that name at all.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        (self.operands)(self.bare(name)).is_some()
    }

    /// Whether an instruction of that name is a call on this target.
    #[must_use]
    pub fn calls(&self, name: &str) -> bool {
        (self.calls)(self.bare(name))
    }

    /// Whether an instruction of that name reads or writes memory on this target.
    #[must_use]
    pub fn touches_mem(&self, name: &str) -> bool {
        (self.touches_mem)(self.bare(name))
    }

    /// Whether this target multiplies an index by that.
    #[must_use]
    pub fn scales(&self, scale: u8) -> bool {
        self.scales.contains(&scale)
    }
}

/// What an address constructor's arguments are.
///
/// An addressing mode is an argument to an instruction rather than an instruction, and a rule
/// file writes one as a term so that a rule can say which registers go where. The selector has
/// to turn that term into a machine IR memory operand, and what each constructor's arguments
/// mean is the same kind of target fact as an instruction's operands, so it is written here
/// rather than in the selector.
///
/// The names are shared. A rule file writes an address with whichever of these its machine has,
/// and a target with only some of them lists which, so a selector reads a constructor the same
/// way whichever machine it is selecting for.
///
/// The scale and the displacement are arguments rather than part of the name because each is a
/// number the rule matched and the machine encodes it as a number. There is none with a symbol
/// yet, because the rules that would need one are the ones about a global and those are not
/// written.
///
/// What the arguments mean is the whole of what tells these apart, and there is deliberately no
/// predicate here that answers half the question: the same register is a base in one of these
/// and an index in another, and the same constant is a scale in one and a displacement in
/// another, so anything building an address out of one has to look at which it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    /// A base register, an index register and a scale, in that order.
    BaseIndexScale,
    /// An index register and a scale, which is an address with nothing to add it to.
    IndexScale,
    /// A base register on its own, which is what a pointer already in a register is.
    Base,
    /// A base register and a constant added to it, which is every field of a structure and
    /// every local reached through a frame pointer.
    BaseOffset,
}
