//! Inline assembly, in GCC's full form.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.7. The constraint language itself is
//! target-specific and is `spec/11-target-support.md`; nothing here looks inside a constraint
//! string.
//!
//! The kernel needs all of this, including `asm goto` with outputs, which GCC only gained in
//! 11 and which the kernel started using as soon as it had it. A statement is kept as written,
//! with its operands in source order, because the template refers to them by position.

use rucc_base::Symbol;
use rucc_diag::Span;

use crate::ast::{AsmOperandList, StrId, StrList, SymbolList};
use crate::expr::ExprId;

/// An assembly statement, in the side table.
pub type AsmId = rucc_base::Idx<Asm>;

/// An `asm` statement or a file-scope `asm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asm {
    /// The template, with adjacent literals already joined by phase 7.
    pub template: StrId,
    /// The output operands.
    pub outputs: AsmOperandList,
    /// The input operands, which are numbered after the outputs.
    pub inputs: AsmOperandList,
    /// The clobber list.
    pub clobbers: StrList,
    /// The labels of an `asm goto`, and empty otherwise.
    pub labels: SymbolList,
    /// The qualifiers written between `asm` and the parenthesis.
    pub quals: AsmQuals,
    /// The whole statement.
    pub span: Span,
}

/// One operand of an assembly statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsmOperand {
    /// The `[name]` in front of the constraint, which lets the template say `%[name]` instead
    /// of counting operands.
    pub name: Option<Symbol>,
    /// The constraint string, which is read by the target and not here.
    pub constraint: StrId,
    /// The expression, which is an lvalue for an output.
    pub value: ExprId,
    /// The whole operand.
    pub span: Span,
}

/// The qualifiers on an assembly statement, as a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AsmQuals(u8);

impl AsmQuals {
    /// None of them.
    pub const NONE: AsmQuals = AsmQuals(0);
    /// `volatile`, which is implied for a statement with no outputs.
    pub const VOLATILE: AsmQuals = AsmQuals(1);
    /// `inline`, which tells the inliner to cost the statement as small.
    pub const INLINE: AsmQuals = AsmQuals(2);
    /// `goto`, which is what makes the label list legal.
    pub const GOTO: AsmQuals = AsmQuals(4);

    /// Whether every qualifier in `other` is set here.
    #[inline]
    #[must_use]
    pub const fn has(self, other: AsmQuals) -> bool {
        self.0 & other.0 == other.0
    }

    /// This set with `other` added.
    #[inline]
    #[must_use]
    pub const fn with(self, other: AsmQuals) -> AsmQuals {
        AsmQuals(self.0 | other.0)
    }

    /// Whether none of them was written.
    #[inline]
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualifier_sets_add_up() {
        let q = AsmQuals::NONE.with(AsmQuals::VOLATILE).with(AsmQuals::GOTO);
        assert!(q.has(AsmQuals::VOLATILE));
        assert!(q.has(AsmQuals::GOTO));
        assert!(!q.has(AsmQuals::INLINE));
        assert!(AsmQuals::NONE.is_none());
    }
}
