//! What an instruction does with an operand, and where the operand is allowed to live.
//!
//! Design: `spec/10-backend.md` sections 10.1 and 10.4.
//!
//! These are the two facts the register allocator reads about an operand and they are the whole
//! of what it reads. The opcode is a name to everything except the encoder, so an allocator
//! never has to know what any particular target's instructions mean, and a target says what its
//! instructions do to their operands by describing them here.
//!
//! They live in this crate rather than in `rucc-mir` because a target's instruction description
//! is data and it is written down before there is any machine IR to put it in. `rucc-mir`
//! re-exports both, so the machine IR is still where a pass reads them from.
//!
//! The vocabulary is regalloc2's, which `spec/10-backend.md` section 10.4 says the allocator
//! interface follows.

use crate::regs::{PhysReg, RegClass};

/// What an instruction does with an operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// Reads it.
    Use,
    /// Writes it, at the point the instruction finishes, so it may share a register with an
    /// operand the instruction reads.
    Def,
    /// Writes it before the instruction has finished reading, so it may not share a register
    /// with anything the instruction reads. This is what a target says about an instruction
    /// that clobbers its destination partway through.
    EarlyDef,
}

impl Role {
    /// Whether it writes the operand, early or late.
    #[must_use]
    pub const fn is_def(self) -> bool {
        matches!(self, Role::Def | Role::EarlyDef)
    }
}

/// Where an operand is allowed to live.
///
/// [`Constraint::Reg`] is the default rather than [`Constraint::Any`] because a machine
/// instruction wants its operands in registers unless it has said otherwise, and a default that
/// permits a stack slot would turn every rule that forgot to say so into a spill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Constraint {
    /// Any register of its class.
    Reg,
    /// A register or a stack slot, whichever the allocator prefers.
    Any,
    /// A stack slot, which is what an operand too large for a register asks for.
    Stack,
    /// That register and no other, which is how a call says where an argument goes and how a
    /// division says where its dividend goes.
    Fixed(PhysReg),
    /// The same register as the operand at that index, which is what a two-address form on
    /// x86-64 needs: the destination is the first source, and the allocator is the one that has
    /// to make that true.
    Reuse(u8),
}

/// One operand of one instruction, as a target's description of that instruction writes it.
///
/// The difference from an operand in the machine IR is the register: there is none here,
/// because a description is about every instruction of that opcode and a register belongs to
/// one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperandDesc {
    /// The class the operand is drawn from.
    pub class: RegClass,
    /// Whether the instruction reads it or writes it.
    pub role: Role,
    /// Where it is allowed to live.
    pub constraint: Constraint,
}

impl OperandDesc {
    /// An operand the instruction reads, in any register of its class.
    #[must_use]
    pub const fn read(class: RegClass) -> Self {
        Self { class, role: Role::Use, constraint: Constraint::Reg }
    }

    /// An operand the instruction writes as it finishes.
    #[must_use]
    pub const fn write(class: RegClass) -> Self {
        Self { class, role: Role::Def, constraint: Constraint::Reg }
    }

    /// An operand the instruction writes before it has finished reading, which is what a
    /// register the instruction destroys on its way through is.
    #[must_use]
    pub const fn write_early(class: RegClass) -> Self {
        Self { class, role: Role::EarlyDef, constraint: Constraint::Reg }
    }

    /// The same operand, constrained.
    #[must_use]
    pub const fn with(mut self, constraint: Constraint) -> Self {
        self.constraint = constraint;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_role_that_writes_says_so_however_early_it_writes() {
        assert!(Role::Def.is_def());
        assert!(Role::EarlyDef.is_def());
        assert!(!Role::Use.is_def());
    }

    #[test]
    fn a_described_operand_keeps_what_it_was_constrained_to() {
        let class = RegClass::new(0);
        let plain = OperandDesc::write(class);
        assert_eq!(plain.constraint, Constraint::Reg);
        let tied = plain.with(Constraint::Reuse(1));
        assert_eq!(tied.constraint, Constraint::Reuse(1));
        assert_eq!(tied.role, Role::Def);
        assert_eq!(OperandDesc::read(class).role, Role::Use);
        assert_eq!(OperandDesc::write_early(class).role, Role::EarlyDef);
    }
}
