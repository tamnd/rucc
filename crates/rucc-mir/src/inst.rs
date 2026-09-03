//! What one machine instruction is, and what an operand is.
//!
//! Design: `spec/10-backend.md` section 10.1.
//!
//! An instruction is an opcode, a run of operands, and the three things an opcode may carry
//! besides its operands: an immediate, a memory addressing mode, and a symbol. Twenty-four
//! bytes, all of it either a small number or an index into a table the function owns, so
//! walking a function is walking one dense array and nothing in it is separately freed.
//!
//! An operand is a register, the class it is drawn from, whether the instruction reads or
//! writes it, and any constraint on where it may live. That is what the allocator reads and it
//! is all the allocator reads, which is the point: the opcode is a name to everything except
//! the encoder, and the allocator never has to know what any particular target's instructions
//! mean.
//!
//! # Where the other pieces are
//!
//! [`Role`] and [`Constraint`] are in `rucc-target`, and this crate re-exports them. A target
//! says what its instructions do to their operands before there is any machine IR to say it in,
//! and both the selector that builds the IR and the encoder that reads it need the answer, so
//! the two of them live below both.
//!
//! Successors are on the block rather than on the terminator, in the order the terminator's own
//! arms run. That is regalloc2's arrangement, which `spec/10-backend.md` section 10.4 says the
//! allocator interface follows, and it keeps a branch's arguments out of the operand vector
//! where they would otherwise be uses the allocator has to be told to treat differently.
//!
//! The source location is a parallel array in the function, reached by [`crate::Func::span`],
//! for the same reason `rucc-ir` puts it there: it is read when a diagnostic is being made and
//! at no other time, so it does not belong on the row that every pass walks.

use rucc_base::{Idx, IdxRange, Symbol};
use rucc_target::{Constraint, PhysReg, RegClass, Role};

/// One instruction, in the function that owns it.
pub type Inst = Idx<InstData>;
/// One basic block, in the function that owns it.
pub type Block = Idx<BlockData>;
/// A run of operands, which is what an instruction's operand vector is.
pub type OperandList = IdxRange<Operand>;
/// One immediate, in the function's immediate table.
pub type ImmRef = Idx<Imm>;
/// One addressing mode, in the function's table of them.
pub type MemRef = Idx<Amode>;

/// Which instruction this is.
///
/// A name rather than a variant of an enum. `spec/10-backend.md` section 10.8 says no pipeline
/// crate holds target-specific code, and an enum of every x86-64 opcode in the crate every
/// target's MIR passes through is exactly that. The opcodes a target has are data: they come out
/// of its rule set, which is what the selector was compiled from, and this crate never asks what
/// one of them means. The encoder does, against the same description the rules were written
/// against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Opcode(Symbol);

impl Opcode {
    /// The opcode of that name.
    #[must_use]
    pub const fn new(name: Symbol) -> Self {
        Self(name)
    }

    /// Its name, which needs the interner it was made with to read.
    #[must_use]
    pub const fn name(self) -> Symbol {
        self.0
    }
}

/// A register, either one the allocator has still to place or one it has placed.
///
/// The two are one type and four bytes because every operand holds one and because a pass that
/// runs both before and after allocation should not be two passes. Which of the two it is, is
/// the top bit, so a virtual register is its own number and nothing has to be masked to compare
/// two of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Reg(u32);

impl Reg {
    /// The bit that says the rest is a physical register rather than a virtual one.
    const PHYSICAL: u32 = 1 << 31;

    /// The virtual register with that number.
    ///
    /// # Panics
    ///
    /// Panics if the number is two billion or more, which no function reaches.
    #[must_use]
    pub const fn virtual_reg(number: u32) -> Self {
        assert!(number < Self::PHYSICAL, "a function with two billion virtual registers");
        Self(number)
    }

    /// The physical register, once one has been chosen.
    #[must_use]
    pub const fn physical(reg: PhysReg) -> Self {
        Self(Self::PHYSICAL | reg.number() as u32)
    }

    /// Whether the allocator has still to place it.
    #[must_use]
    pub const fn is_virtual(self) -> bool {
        self.0 & Self::PHYSICAL == 0
    }

    /// Its number as a virtual register, or `None` once it is a physical one.
    #[must_use]
    pub const fn number(self) -> Option<u32> {
        if self.is_virtual() { Some(self.0) } else { None }
    }

    /// The physical register it is, or `None` while it is still virtual.
    ///
    /// Which class the register is in is on the operand rather than here, because an operand
    /// carries its class already and a second copy of it is a thing that can disagree.
    #[must_use]
    pub const fn phys(self) -> Option<PhysReg> {
        if self.is_virtual() { None } else { Some(PhysReg::new((self.0 & 0xff) as u8)) }
    }
}

/// One operand of one instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operand {
    /// The register, virtual until the allocator has run.
    pub reg: Reg,
    /// The class it is drawn from.
    pub class: RegClass,
    /// Whether the instruction reads it or writes it.
    pub role: Role,
    /// Where it is allowed to live.
    pub constraint: Constraint,
}

impl Operand {
    /// An operand the instruction reads.
    #[must_use]
    pub const fn read(reg: Reg, class: RegClass) -> Self {
        Self { reg, class, role: Role::Use, constraint: Constraint::Reg }
    }

    /// An operand the instruction writes as it finishes.
    #[must_use]
    pub const fn write(reg: Reg, class: RegClass) -> Self {
        Self { reg, class, role: Role::Def, constraint: Constraint::Reg }
    }

    /// An operand the instruction writes before it has finished reading.
    #[must_use]
    pub const fn write_early(reg: Reg, class: RegClass) -> Self {
        Self { reg, class, role: Role::EarlyDef, constraint: Constraint::Reg }
    }

    /// The same operand, constrained.
    #[must_use]
    pub const fn with(mut self, constraint: Constraint) -> Self {
        self.constraint = constraint;
        self
    }
}

/// One immediate.
///
/// Signed and sixty-four bits, which every immediate field of every target we have is narrower
/// than. What fits in the field the encoder is about to write is the encoder's question, and it
/// is one it can only answer per opcode, so nothing here tries to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Imm(pub i64);

/// A memory addressing mode, as the instruction holds it.
///
/// The registers are the indices of the operands holding them rather than the registers
/// themselves, because an address register is a register the allocator has to see and rewrite,
/// and the only thing it looks at is the operand vector. [`Mem`] is the same thing written the
/// way a caller writes it, and [`crate::InstBuilder::mem`] turns one into the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Amode {
    /// The operand holding the base register.
    pub base: Option<u8>,
    /// The operand holding the index register.
    pub index: Option<u8>,
    /// What the index is multiplied by, which is 1 when there is no index.
    pub scale: u8,
    /// The constant added to the address.
    pub disp: i32,
    /// The symbol the address is relative to, for an access to a global.
    pub symbol: Option<Symbol>,
}

impl Amode {
    /// The addressing mode naming no register and no symbol, at offset zero.
    pub const NOTHING: Self = Self { base: None, index: None, scale: 1, disp: 0, symbol: None };
}

/// A memory addressing mode as a caller writes one down.
///
/// The difference from [`Amode`] is that the registers are here rather than in the operand
/// vector, which is what [`crate::InstBuilder::mem`] fixes. Keeping the two apart is what lets
/// the operand indices in an [`Amode`] be an invariant of the builder rather than something
/// every caller has to get right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mem {
    /// The base register, which the instruction reads.
    pub base: Option<Operand>,
    /// The index register, which the instruction reads.
    pub index: Option<Operand>,
    /// What the index is multiplied by.
    pub scale: u8,
    /// The constant added to the address.
    pub disp: i32,
    /// The symbol the address is relative to.
    pub symbol: Option<Symbol>,
}

impl Mem {
    /// The address in that register.
    #[must_use]
    pub const fn at(base: Operand) -> Self {
        Self { base: Some(base), index: None, scale: 1, disp: 0, symbol: None }
    }

    /// The address of that symbol.
    #[must_use]
    pub const fn of(symbol: Symbol) -> Self {
        Self { base: None, index: None, scale: 1, disp: 0, symbol: Some(symbol) }
    }

    /// The same address with an index register scaled by that much.
    #[must_use]
    pub const fn indexed(mut self, index: Operand, scale: u8) -> Self {
        self.index = Some(index);
        self.scale = scale;
        self
    }

    /// The same address, that many bytes along.
    #[must_use]
    pub const fn plus(mut self, disp: i32) -> Self {
        self.disp = disp;
        self
    }
}

/// One arm of a terminator: where it goes, and what it takes with it.
///
/// The arguments are the values the target block's parameters arrive as, so this is the edge on
/// which a phi would otherwise sit. After allocation the parameters are physical registers and
/// these arguments have become the moves that write them, which is the point at which MIR stops
/// being in SSA form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockCall {
    /// The block it goes to.
    pub block: Block,
    /// What its parameters arrive as, one for one.
    pub args: Vec<Reg>,
}

impl BlockCall {
    /// A jump to that block carrying nothing.
    #[must_use]
    pub const fn to(block: Block) -> Self {
        Self { block, args: Vec::new() }
    }

    /// A jump to that block carrying those registers.
    #[must_use]
    pub fn with(block: Block, args: Vec<Reg>) -> Self {
        Self { block, args }
    }
}

/// One parameter of a block: the register the value arrives in, and its class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Param {
    /// What the value arrives as, virtual until the allocator has run.
    pub reg: Reg,
    /// The class it is drawn from.
    pub class: RegClass,
}

/// One instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstData {
    /// Which instruction this is.
    pub opcode: Opcode,
    /// Its operands, defs first and then uses, with the registers a memory operand names last.
    /// The order is what the printer and the parser agree on, and [`crate::InstBuilder`] is
    /// what keeps it.
    pub operands: OperandList,
    /// Its immediate, if it has one.
    pub imm: Option<ImmRef>,
    /// Its memory operand, if it has one.
    pub mem: Option<MemRef>,
    /// The symbol it names, which is the callee of a direct call and the target of a direct
    /// jump to another function.
    pub symbol: Option<Symbol>,
}

impl InstData {
    /// An instruction with that opcode and nothing else.
    #[must_use]
    pub const fn new(opcode: Opcode) -> Self {
        Self { opcode, operands: OperandList::EMPTY, imm: None, mem: None, symbol: None }
    }
}

/// Where an instruction sits: which block it is in, and what is either side of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct InstLayout {
    pub(crate) block: Option<Block>,
    pub(crate) prev: Option<Inst>,
    pub(crate) next: Option<Inst>,
}

/// One block: what arrives in it, what is in it, and where it goes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockData {
    /// The values that arrive in it, which are the function's arguments in the entry block.
    pub params: Vec<Param>,
    /// Where its terminator goes, in the order the terminator's arms run.
    pub succs: Vec<BlockCall>,
    pub(crate) first_inst: Option<Inst>,
    pub(crate) last_inst: Option<Inst>,
    pub(crate) prev: Option<Block>,
    pub(crate) next: Option<Block>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instruction_is_the_size_the_design_says() {
        assert_eq!(size_of::<InstData>(), 24);
        assert_eq!(size_of::<Operand>(), 8);
    }

    #[test]
    fn a_virtual_register_is_its_own_number() {
        let reg = Reg::virtual_reg(7);
        assert!(reg.is_virtual());
        assert_eq!(reg.number(), Some(7));
        assert_eq!(reg.phys(), None);
    }

    #[test]
    fn a_physical_register_is_not_a_virtual_one_of_the_same_number() {
        let reg = Reg::physical(PhysReg::new(7));
        assert!(!reg.is_virtual());
        assert_eq!(reg.number(), None);
        assert_eq!(reg.phys(), Some(PhysReg::new(7)));
        assert_ne!(reg, Reg::virtual_reg(7));
    }

    #[test]
    fn an_operand_keeps_what_it_was_constrained_to() {
        let class = RegClass::new(0);
        let plain = Operand::write(Reg::virtual_reg(1), class);
        assert_eq!(plain.role, Role::Def);
        assert_eq!(plain.constraint, Constraint::Reg);
        let tied = plain.with(Constraint::Reuse(1));
        assert_eq!(tied.constraint, Constraint::Reuse(1));
        assert_eq!(tied.reg, plain.reg);
        assert!(tied.role.is_def());
        assert!(!Operand::read(Reg::virtual_reg(1), class).role.is_def());
    }
}
