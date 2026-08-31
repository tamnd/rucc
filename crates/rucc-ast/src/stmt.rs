//! Statements.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.2.
//!
//! Like the expressions, these are not desugared. A `for` loop stays a `for` loop instead of
//! becoming a `while`, a `switch` keeps its cases as a tree instead of a jump table, and a
//! `do` loop is not a `while` with the test moved. All of that happens when the IR is built.
//!
//! A declaration inside a block is [`Stmt::Decl`], holding the whole declaration and so all of
//! its declarators, which keeps `int a, b;` one statement instead of two and keeps the shape
//! the same at block scope as at file scope.

use rucc_base::Symbol;

use crate::asm::AsmId;
use crate::ast::{AttrList, StmtList, SymbolList};
use crate::decl::DeclId;
use crate::expr::ExprId;

/// A statement in the statement arena.
pub type StmtId = rucc_base::Idx<Stmt>;

/// One statement node.
///
/// Twenty bytes, set by [`Stmt::For`], which is the only variant with four operands and which
/// gets away with it because [`ForInit`] has spare tag values for the statement's own tag to
/// live in. Splitting the loop's clauses into a side table would buy four bytes a statement and
/// cost an indirection on the most common loop in C, so they stay inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stmt {
    /// A parse that did not work out. Poisoned, as [`Expr::Error`](crate::Expr::Error) is.
    Error,
    /// `;`.
    Empty,
    /// An expression evaluated for its effect.
    Expr(ExprId),
    /// A declaration at block scope, with all of its declarators.
    Decl(DeclId),
    /// `{ ... }`, which is also a scope.
    Compound(StmtList),
    /// `if (cond) then else otherwise`.
    If {
        /// The controlling expression.
        cond: ExprId,
        /// The statement taken when it is nonzero.
        then: StmtId,
        /// The `else` branch, if there was one.
        otherwise: Option<StmtId>,
    },
    /// `switch (scrutinee) body`.
    Switch {
        /// The controlling expression.
        scrutinee: ExprId,
        /// The body, whose cases are found by walking it.
        body: StmtId,
    },
    /// `while (cond) body`.
    While {
        /// The controlling expression, tested before each iteration.
        cond: ExprId,
        /// The body.
        body: StmtId,
    },
    /// `do body while (cond);`.
    DoWhile {
        /// The body, which runs at least once.
        body: StmtId,
        /// The controlling expression, tested after each iteration.
        cond: ExprId,
    },
    /// `for (init; cond; step) body`.
    For {
        /// The first clause, which may declare something and so may open a scope.
        init: ForInit,
        /// The controlling expression, absent when the clause was left empty, which means the
        /// loop runs forever.
        cond: Option<ExprId>,
        /// The third clause, evaluated after each iteration.
        step: Option<ExprId>,
        /// The body.
        body: StmtId,
    },
    /// `goto name;`.
    Goto(Symbol),
    /// `goto *expr;`, GNU's computed goto, which Postgres and the kernel both use.
    GotoExpr(ExprId),
    /// `continue;`.
    Continue,
    /// `break;`.
    Break,
    /// `return expr;`, or `return;`.
    Return(Option<ExprId>),
    /// `name: body`.
    Label {
        /// The label, which lives in a namespace of its own and is scoped to the function.
        name: Symbol,
        /// The statement it labels, absent when the label is the last thing in a block, which
        /// C23 allows and which everybody wrote as `name: ;` before it.
        body: Option<StmtId>,
        /// Attributes on the label, such as `[[gnu::hot]]`.
        attrs: AttrList,
    },
    /// `case lo: body`, or GNU's `case lo ... hi: body`.
    Case {
        /// The value, or the first value of a range.
        lo: ExprId,
        /// The last value of a GNU case range, which is included.
        hi: Option<ExprId>,
        /// The statement it labels, absent for the same reason as on a label.
        body: Option<StmtId>,
    },
    /// `default: body`.
    Default {
        /// The statement it labels.
        body: Option<StmtId>,
    },
    /// `__label__ a, b;`, GNU's block-local labels, which a macro needs so that two expansions
    /// in one function do not collide.
    LocalLabels(SymbolList),
    /// An `asm` statement.
    Asm(AsmId),
}

/// The first clause of a `for` statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForInit {
    /// Nothing was written.
    None,
    /// An expression.
    Expr(ExprId),
    /// A declaration, which C99 allowed and which scopes to the loop.
    Decl(DeclId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_statement_is_twenty_bytes() {
        assert_eq!(size_of::<Stmt>(), 20);
    }

    #[test]
    fn a_statement_id_is_four_bytes_even_when_optional() {
        assert_eq!(size_of::<StmtId>(), 4);
        assert_eq!(size_of::<Option<StmtId>>(), 4);
    }

    #[test]
    fn a_for_clause_is_eight_bytes() {
        assert_eq!(size_of::<ForInit>(), 8);
    }
}
