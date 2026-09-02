//! Typed statements.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.14.
//!
//! The shapes are the ones the parser produced, because a `for` loop that has become a `while`
//! loop by the time anything reports on it is a `for` loop nobody can be told about. What is
//! different is that the conditions have been converted to `bool`, the labels and the `goto`s
//! that reach them have been resolved to each other, and a `switch` carries the table of cases
//! that the walk to the IR would otherwise have to collect by searching its body.

use rucc_base::{Idx, IdxRange};

use crate::asm::AsmId;
use crate::decl::DeclList;
use crate::expr::ExprId;
use crate::tast::LabelId;

/// One `case` of a `switch`, in the case table.
pub type CaseId = Idx<Case>;

/// One typed statement in the arena.
pub type StmtId = Idx<Stmt>;

/// The table of references to statements, which is what a block is a run of.
#[derive(Debug)]
pub struct StmtRef;

/// A run of statements.
pub type StmtList = IdxRange<StmtRef>;

/// A run of the cases of one `switch`.
pub type CaseList = IdxRange<Case>;

/// A statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stmt {
    /// A statement that was already the subject of a diagnostic.
    Error,
    /// `;`, which is a statement and does nothing.
    Empty,
    /// An expression evaluated for its effects, whose value is discarded.
    Expr(ExprId),
    /// `{ ... }`, which is also a scope, though the scope has been resolved by now.
    Block(StmtList),
    /// The declarations of one declaration statement, which introduce objects into the block.
    Decls(DeclList),
    /// `if (cond) then else otherwise`.
    If {
        /// The condition, converted to `bool`.
        cond: ExprId,
        /// The statement taken when it is true.
        then: StmtId,
        /// The statement taken when it is false, if there is one.
        otherwise: Option<StmtId>,
    },
    /// `while (cond) body`.
    While {
        /// The condition, converted to `bool`.
        cond: ExprId,
        /// The body.
        body: StmtId,
    },
    /// `do body while (cond);`.
    DoWhile {
        /// The body, which runs before the condition is first evaluated.
        body: StmtId,
        /// The condition, converted to `bool`.
        cond: ExprId,
    },
    /// `for (init; cond; step) body`.
    For {
        /// The initial clause, which is a declaration or an expression or nothing.
        init: Option<StmtId>,
        /// The condition, converted to `bool`, or nothing for `for (;;)`.
        cond: Option<ExprId>,
        /// The step, evaluated for its effects.
        step: Option<ExprId>,
        /// The body.
        body: StmtId,
    },
    /// `switch (cond) body`.
    Switch {
        /// The controlling expression, after its integer promotion.
        cond: ExprId,
        /// The body, which holds the labelled statements themselves.
        body: StmtId,
        /// Where each value goes, collected here so that the walk to the IR builds a jump
        /// table from a table rather than by searching the body for labels.
        cases: CaseList,
        /// Where a value that matches nothing goes, absent when there is no `default`, which
        /// is the same statement the [`Stmt::Default`] in the body labels.
        default: Option<StmtId>,
    },
    /// `case value:`, which is a label on the statement it precedes.
    ///
    /// The value is in the case table rather than here, because two `i128` bounds would make
    /// every statement in the arena the size of the widest one.
    Case {
        /// Which entry of the enclosing `switch`'s table, so that reaching a case gives its
        /// value without searching the table for the statement that is already in hand.
        case: CaseId,
        /// The statement it labels.
        body: StmtId,
    },
    /// `default:`, which is a label and not a case, since it has no value to be in a table.
    ///
    /// It is here as well as in the enclosing `switch` because the two say different things:
    /// this says where in the body it was written, and the field on the `switch` is the entry
    /// of the jump table. A reader that only had the field could not say where `default:` went.
    Default {
        /// The statement it labels.
        body: StmtId,
    },
    /// `name: body`, with the label resolved.
    Label {
        /// Which label.
        label: LabelId,
        /// The statement it labels.
        body: StmtId,
    },
    /// `goto name;`, with the label resolved.
    Goto(LabelId),
    /// `goto *expr;`, GNU's computed goto, whose target is a label address.
    IndirectGoto(ExprId),
    /// `asm(...)`, whose operands and labels are in the assembly table.
    Asm(AsmId),
    /// `break;`, which leaves the innermost loop or `switch`.
    Break,
    /// `continue;`, which starts the next iteration of the innermost loop.
    Continue,
    /// `return;` or `return expr;`, with the value already converted to the return type.
    Return(Option<ExprId>),
}

/// One `case` of a `switch`, after its value has been folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Case {
    /// The lowest value that reaches this case.
    ///
    /// The value is held in the promoted type of the controlling expression, converted there
    /// once, so that the comparison the IR emits is between two values of one type.
    pub low: i128,
    /// The highest value that reaches it, which equals `low` unless this is GNU's `case 1 ... 9`.
    pub high: i128,
    /// The statement it labels.
    pub body: StmtId,
}
