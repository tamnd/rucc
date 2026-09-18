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
//! The other thing described here is width. A number that is not negative and fits in thirty-two
//! bits goes into a sixty-four bit register either way, because writing the low half of a register
//! on this machine clears the high half rather than leaving it alone, and the instruction that
//! writes the low half is the shorter of the two. So `movq $7, %rax` and `movl $7, %eax` leave the
//! same number in the same place and are seven bytes and five.
//!
//! That one an encoder could do without asking anything, since neither instruction touches the
//! condition state and the register ends up holding the same number. It is not done there because
//! an encoder that wrote `movl` where it was handed `movq` would be writing bytes the listing beside
//! them does not say, and the listing and the bytes saying the same thing is worth more than the
//! two bytes. Choosing the instruction is this pass's job and spelling the one it chose is the
//! encoder's.
//!
//! The third thing described here is a comparison against zero. `cmpl $0, %eax` asks whether what
//! is in the register is zero, is above it or is below it, and `testl %eax, %eax` asks the machine
//! the same three questions of the same register without a constant on the instruction, which is
//! three bytes against two. Both leave the sign, the zero and the parity of what is in the register,
//! and both clear the carry and the overflow, so every condition behind either of them reads the
//! same answer.
//!
//! That one is an encoder's rewrite even less than the width one is, since the two instructions are
//! not even the same length of operand list, and it is here for the same reason: the listing and the
//! bytes say the same thing because the pass chose the instruction the encoder then spells.
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
    /// Every instruction that puts a constant in a register and has a narrower one that writes the
    /// same register and clears the rest of it.
    pub narrowing: &'static [Narrowed],
    /// Every instruction that compares a register against a constant and has a shorter one that
    /// asks the same thing of the register against itself when the constant is zero.
    pub testing: &'static [Tested],
}

impl ShortInsts {
    /// The shorter way of writing zero into a register, for the instruction of that name.
    #[must_use]
    pub fn zeroed(&self, name: &str) -> Option<&'static str> {
        self.zeroing.iter().find(|entry| entry.name == name).map(|entry| entry.into)
    }

    /// The narrower instruction that writes the same register, for the instruction of that name.
    #[must_use]
    pub fn narrowed(&self, name: &str) -> Option<Narrowed> {
        self.narrowing.iter().find(|entry| entry.name == name).copied()
    }

    /// The shorter way of comparing a register against zero, for the instruction of that name.
    #[must_use]
    pub fn tested(&self, name: &str) -> Option<&'static str> {
        self.testing.iter().find(|entry| entry.name == name).map(|entry| entry.into)
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

/// One instruction that writes a constant, and the narrower one that writes the same register.
///
/// Narrower means fewer bytes of the number written out, and on this machine it also means fewer
/// bytes of instruction: the sixty-four bit move carries a prefix byte saying so and the thirty-two
/// bit move does not, and a number too wide to sign extend from thirty-two bits is written out
/// whole where the narrower instruction writes four bytes of it.
///
/// It says the same thing only for the numbers the narrower instruction can hold and only on a
/// machine where writing part of a register clears the rest of it, which is why [`Narrowed::writes`]
/// is here rather than the pass working the range out from the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Narrowed {
    /// The opcode that takes the constant.
    pub name: &'static str,
    /// The opcode that takes the same constant in fewer bytes.
    pub into: &'static str,
    /// How many bits of the register the narrower opcode writes. The rest of the register is
    /// cleared rather than left alone, so the two say the same thing for a number that is not
    /// negative and fits in this many bits, and disagree for every other number.
    pub writes: u32,
}

/// One comparison against a constant, and the shorter instruction that asks it against zero.
///
/// The shorter one reads the register it is given and reads it again instead of the constant, so it
/// names one register where the first names a register and a number, and the bytes it saves are the
/// bytes the number was written in. It says the same thing only for zero: a comparison of a register
/// against zero and a bitwise and of the register with itself leave the same sign, the same zero and
/// the same parity, and both leave the carry and the overflow clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tested {
    /// The opcode that takes the constant.
    pub name: &'static str,
    /// The opcode that asks the same question of the register alone. It writes the condition state
    /// exactly as the first one does, which is why nothing about where the state is live is a
    /// question this rewrite has to ask.
    pub into: &'static str,
}
