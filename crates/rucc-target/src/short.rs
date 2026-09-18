//! The instructions that have a shorter spelling of the same answer, and what the shorter one is.
//!
//! Design: `spec/optimizer/37-machine-level-optimization.md` section 37.4.
//!
//! There is more than one instruction for putting a number in a register, and on a machine with
//! variable length instructions they are not the same number of bytes. Putting zero there is the
//! case worth having a description for: `movl $0, %eax` spells the zero out and is five bytes, and
//! `xorl %eax, %eax` says it without spelling it and is two. GCC writes the second one everywhere
//! and rucc writes the first, which over the corpus at `-Os` is twenty thousand instructions
//! against seven.
//!
//! What stops it being a thing the encoder does on its own is that the two are not the same
//! instruction. The exclusive or writes the condition state and the move does not, so the rewrite
//! is legal where nothing reads the state before something else writes it and is wrong where
//! anything does. That is a question about the instructions behind it rather than about the
//! instruction itself, which is what makes it a pass rather than a choice of encoding, and
//! [`crate::FlagInsts`] is the description that pass asks.
//!
//! It is here rather than in the pass for the reason [`crate::FlagInsts`] and
//! [`crate::BranchInsts`] are here. The pass is in a pipeline crate and `spec/10-backend.md`
//! section 10.8 says a pipeline crate holds no target-specific code, so what the pass knows about
//! a machine arrives as a description rather than as a name it says out loud.

/// The shorter spellings this target has.
#[derive(Debug)]
pub struct ShortInsts {
    /// What a rule file and the machine IR put in front of this target's opcodes, such as `x64.`.
    pub prefix: &'static str,
    /// Every instruction that puts a constant in a register and has a shorter way of putting zero
    /// there.
    pub zeroing: &'static [Zeroed],
}

impl ShortInsts {
    /// The shorter way of writing zero into a register, for the instruction of that name.
    #[must_use]
    pub fn zeroed(&self, name: &str) -> Option<&'static str> {
        self.zeroing.iter().find(|entry| entry.name == name).map(|entry| entry.into)
    }
}

/// One instruction that writes a constant, and the instruction that writes zero in fewer bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zeroed {
    /// The opcode that takes the constant.
    pub name: &'static str,
    /// The opcode that writes zero into the one register the first one wrote, reading that same
    /// register twice. It writes the condition state, which is the whole of why this is a
    /// description and not a rewrite anyone could do without looking around.
    pub into: &'static str,
}
