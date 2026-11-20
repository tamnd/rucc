//! Initializers.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.2 and `spec/07-types-and-semantics.md`.
//!
//! An initializer is either an expression or a braced list of initializers, and a braced list
//! may say where each of its elements goes. Nothing here works out where they actually go: the
//! rules for walking a partly-designated brace list over a struct containing an array of unions
//! are semantic and they live in `spec/07-types-and-semantics.md`. What is kept here is exactly
//! what was written, brace for brace, because the diagnostics for getting it wrong have to
//! quote it.

use rucc_base::Symbol;
use rucc_diag::Span;

use crate::ast::{DesignatorList, InitItemList};
use crate::expr::ExprId;

/// An initializer, in the side table.
pub type InitId = rucc_base::Idx<Init>;

/// What an object is initialized with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Init {
    /// A single expression, as in `int x = 1;`.
    Expr(ExprId),
    /// A braced list, which may be empty. `{}` is C23 and `{ 0 }` is how everybody used to
    /// write it, and they are not the same spelling even though they mean the same thing.
    List(InitItemList),
}

/// One element of a braced initializer list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitItem {
    /// The designators before the `=`, and an empty list when the element just follows the one
    /// before it.
    pub designators: DesignatorList,
    /// The initializer for this element, which may itself be a braced list.
    pub init: InitId,
    /// From the first designator to the end of the initializer.
    pub span: Span,
}

/// One step of a designation, or of a `__builtin_offsetof` member path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Designator {
    /// `.name`.
    Field(Symbol),
    /// `[index]`.
    Index(ExprId),
    /// `[lo ... hi]`, GNU's range, which initializes a run of elements with one value.
    Range {
        /// The first index.
        lo: ExprId,
        /// The last index, which is included.
        hi: ExprId,
    },
    /// `name:`, the form GCC had before C99 and still accepts, with a warning under
    /// `-pedantic`. Kept apart from [`Designator::Field`] so the printer puts back what was
    /// written and so the diagnostic can point at the right thing.
    ObsoleteField(Symbol),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_initializer_is_twelve_bytes() {
        assert_eq!(size_of::<Init>(), 12);
    }

    #[test]
    fn a_designator_is_twelve_bytes() {
        // Set by the GNU range, which is the only one with two operands.
        assert_eq!(size_of::<Designator>(), 12);
    }
}
