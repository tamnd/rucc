//! Inline assembly, after checking.
//!
//! Design: `spec/13-gnu-compat.md` and `spec/11-asm-objects-debug.md`, which owns what a
//! constraint means. Nothing here looks inside a template.
//!
//! The shape is the one the parser produced, with the operands still in source order, because
//! the template refers to them by the position they were written in and renumbering them would
//! make `%1` name something the program did not write. What checking adds is the type of every
//! operand, the labels resolved to the ones the function declares, and one answer per operand
//! that the walk to the IR would otherwise have to work out for itself: whether the operand
//! travels as a value or as the address of an object.
//!
//! That last answer is here rather than in the walk because two passes need it and they have to
//! agree. The walk builds the address of a memory operand, and the scan that runs before it
//! decides which locals need a stack slot, so an operand the walk takes the address of has to be
//! an operand the scan already knew about. One field read twice is how they agree.

use rucc_ast::AsmQuals;
use rucc_base::{Idx, IdxRange, Symbol};

use crate::expr::ExprId;
use crate::tast::StrId;

/// An assembly statement, in the side table.
pub type AsmId = Idx<Asm>;

/// The table of references to string literals, which is what a clobber list is a run of.
#[derive(Debug)]
pub struct StrRef;

/// A run of string literals.
pub type StrList = IdxRange<StrRef>;

/// The table of references to labels, which is what an `asm goto` label list is a run of.
#[derive(Debug)]
pub struct LabelRef;

/// A run of labels.
pub type LabelList = IdxRange<LabelRef>;

/// A run of operands.
pub type AsmOperandList = IdxRange<AsmOperand>;

/// One `asm` statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asm {
    /// The template, which is passed to the assembler with the operands substituted into it.
    pub template: StrId,
    /// The output operands, which are numbered from zero.
    pub outputs: AsmOperandList,
    /// The input operands, which are numbered after the outputs.
    pub inputs: AsmOperandList,
    /// The clobber list.
    pub clobbers: StrList,
    /// The labels of an `asm goto`, empty for everything else.
    pub labels: LabelList,
    /// The qualifiers, with `volatile` set for a statement that implies it.
    pub quals: AsmQuals,
}

/// One operand of an assembly statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsmOperand {
    /// The `[name]` it was written with, which is what `%[name]` in the template refers to.
    pub name: Option<Symbol>,
    /// The constraint, as written, including the `=` or `+` of an output.
    pub constraint: StrId,
    /// The operand itself, which is an lvalue for an output and for anything in memory, and a
    /// value everywhere else.
    pub value: ExprId,
    /// Whether the assembly is given the address of an object rather than a value.
    ///
    /// True for a constraint that allows nothing but memory and for a structure or a union,
    /// which is not something a register holds.
    pub memory: bool,
}
