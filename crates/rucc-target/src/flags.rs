//! What each instruction leaves in the condition state, and which comparisons ask the same thing.
//!
//! Design: `spec/optimizer/37-machine-level-optimization.md` section 37.4.
//!
//! A comparison on this kind of machine computes nothing. What it does is set a few bits nobody
//! named, and the instruction behind it reads them. So a comparison whose bits are already the
//! bits that are there is an instruction that could not be observed to have run, and taking it out
//! is the whole of this. There are two ways for the bits to already be there. The same comparison
//! was made a few instructions ago and nothing has disturbed it since, which is what a program
//! that asks whether something is zero and then whether it is not comes out as. Or the comparison
//! is against zero and the value it is about was worked out by arithmetic, which set the same bits
//! on its way past.
//!
//! It is here rather than in the pass for the reason [`crate::FrameInsts`] and
//! [`crate::BranchInsts`] are here. The pass is in a pipeline crate and `spec/10-backend.md`
//! section 10.8 says a pipeline crate holds no target-specific code, so what the pass knows about
//! a machine arrives as a description rather than as a name it says out loud.
//!
//! # Why the second case is not every condition
//!
//! A comparison against zero leaves more than the answer to it. `cmpl $0, %eax` says whether the
//! register is zero, and it says the register's sign, and it says that nothing carried and nothing
//! overflowed, because subtracting zero from a number cannot do either. An instruction that merely
//! happens to have written the register agrees about some of that and not all of it. `andl` agrees
//! about all of it: the machine clears carry and overflow after one, and sets the zero and sign
//! bits from what it wrote, which is what the comparison would have set them from. `subl` agrees
//! about the zero bit and about nothing else, because a subtraction that overflowed says so and
//! the comparison would have said it did not, and a condition built out of the sign and the
//! overflow together then reads two bits that no longer belong to each other.
//!
//! So a [`Zeroing`] says which of the three groups of conditions it is good for, [`Reads`] is
//! which group a condition belongs to, and a [`Reader`] is an instruction that names one. The zero
//! group is the one every entry is good for, since every instruction here that writes the
//! condition state at all sets the zero bit from what it wrote.
//!
//! The first case needs none of that. Two instructions that made the same comparison of the same
//! values left the same bits, all of them, so what may read them is not a question.
//!
//! # What is not a [`Zeroing`]
//!
//! A shift, because a shift by zero leaves the condition state exactly as it found it, and the
//! count is in a register often enough that the compiler cannot tell. A multiply, because the
//! machine leaves the zero bit undefined after one. An increment and a decrement, because they
//! leave carry alone rather than clearing it. None of those is a shape a program runs into often,
//! and each of them is a way to be quietly wrong, so the table says nothing about them and the
//! pass believes the table.

/// What a pass has to know about a machine to find a comparison the machine has already made.
#[derive(Debug, Clone, Copy)]
pub struct FlagInsts {
    /// What a rule file and the machine IR put in front of this target's opcodes, such as `x64.`.
    pub prefix: &'static str,
    /// How many bits of the operand at that index the instruction of that name uses.
    ///
    /// The same question [`crate::BitInsts`] asks and the same answer, because two instructions
    /// that agree about a register's low half and not about the rest of it have not made the same
    /// comparison. `None` means the description does not name that operand, which the pass reads
    /// as not knowing and so as not the same.
    pub width: fn(&str, u8) -> Option<u32>,
    /// Whether the instruction of that name leaves the condition state other than it found it.
    ///
    /// True for a name this target does not have, since an instruction nothing knows anything
    /// about is one that may have done anything. It is the answer that makes the pass find less
    /// rather than the one that makes it wrong.
    pub writes: fn(&str) -> bool,
    /// Every instruction that makes a comparison, whether or not it keeps the answer.
    pub compares: &'static [Compare],
    /// Every instruction that reads the condition state and names which part of it.
    pub readers: &'static [Reader],
    /// Every instruction that leaves what a comparison of what it wrote against zero would leave.
    pub zeroing: &'static [Zeroing],
}

/// One comparison, and what is left of it when the machine has already made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compare {
    /// The opcode.
    pub name: &'static str,
    /// What it asks, which is the same string for every condition at one width.
    ///
    /// Two instructions have made the same comparison when this agrees and the registers and
    /// constants they read agree. The condition on the front of each of them is not part of it:
    /// the machine is asked the question once and the two conditions read two answers to it, which
    /// is exactly the case the pass is here for.
    pub asks: &'static str,
    /// What is left of it when the answer is already in the condition state.
    ///
    /// [`None`] is nothing at all, which is the entry for a comparison that keeps no answer,
    /// because the instruction behind it is reading the condition state and the condition state is
    /// already right. Anything else is an opcode that writes the same answer to the same register
    /// and makes no comparison, which is what a comparison that keeps a byte becomes.
    pub kept: Option<&'static str>,
}

/// One instruction that reads the condition state, and which part of it it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reader {
    /// The opcode.
    ///
    /// A comparison that keeps a byte is one of these as well as a [`Compare`], and what it reads
    /// is what it just set rather than what it found. That is the same entry either way, since
    /// what the entry says is which part of the condition state the condition names.
    pub name: &'static str,
    /// Which part of it.
    pub reads: Reads,
}

/// Which part of what a comparison against zero left a condition is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reads {
    /// Whether the value was zero, and nothing else.
    Zero,
    /// Where the value sits against zero as a signed number, which is the sign and the overflow.
    Signed,
    /// Where it sits as an unsigned number, which is the carry, alone or with the zero.
    Unsigned,
}

/// One instruction that leaves behind the comparison of what it wrote against zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zeroing {
    /// The opcode.
    pub name: &'static str,
    /// Whether a signed comparison against zero reads what it left and gets the right answer.
    pub signed: bool,
    /// Whether an unsigned one does.
    pub unsigned: bool,
}

impl FlagInsts {
    /// The entry for the instruction of that name, if it makes a comparison.
    #[must_use]
    pub fn compare(&self, name: &str) -> Option<&'static Compare> {
        self.compares.iter().find(|entry| entry.name == name)
    }

    /// Which part of the condition state the instruction of that name reads, if it reads any.
    #[must_use]
    pub fn reads(&self, name: &str) -> Option<Reads> {
        self.readers.iter().find(|entry| entry.name == name).map(|entry| entry.reads)
    }

    /// The entry for the instruction of that name, if it leaves a comparison against zero.
    #[must_use]
    pub fn zeroed(&self, name: &str) -> Option<&'static Zeroing> {
        self.zeroing.iter().find(|entry| entry.name == name)
    }
}

impl Zeroing {
    /// Whether a condition about that part of the condition state may read what it left.
    #[must_use]
    pub const fn covers(&self, reads: Reads) -> bool {
        match reads {
            Reads::Zero => true,
            Reads::Signed => self.signed,
            Reads::Unsigned => self.unsigned,
        }
    }
}
