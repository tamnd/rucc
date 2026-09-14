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
    /// Whether an instruction of that name writes to the memory it names.
    ///
    /// Asked by a pass moving a read of memory from where it is to somewhere later, which is safe
    /// while nothing it passes could have written what it is about to read. The answer is about
    /// the instruction rather than about the address, because whether two addresses are the same
    /// place is a question nothing below selection has an analysis for.
    ///
    /// A call answers `false` here and is not safe to move a read past. What a call does to memory
    /// is not in the instruction at all, which is why [`Self::calls`] is a separate question and
    /// why a pass asking this one has to ask that one as well.
    pub writes_mem: fn(&str) -> bool,
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

    /// Whether an instruction of that name writes to memory on this target.
    #[must_use]
    pub fn writes_mem(&self, name: &str) -> bool {
        (self.writes_mem)(self.bare(name))
    }

    /// Whether this target multiplies an index by that.
    #[must_use]
    pub fn scales(&self, scale: u8) -> bool {
        self.scales.contains(&scale)
    }
}
