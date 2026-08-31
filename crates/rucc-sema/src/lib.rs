//! Type checking, conversions, initialization, constant evaluation, and the typed AST.
//!
//! Design: `spec/07-types-and-semantics.md`. Layer rank 7, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! The typed tree is here: the arenas, the nodes for every typed expression and statement, the
//! declarations with their linkage and storage duration, and the flattened initializers. What
//! fills it in, which is the checking itself, is being written on top of it.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.
//!
//! # What a typed tree is for
//!
//! Every expression carries a [`TypeId`](rucc_types::TypeId), every conversion the language
//! performs without being asked is a [`Conversion`] node, every constant that can be folded has
//! been, and every use of a name points at the [`Decl`] it resolved to. Nothing downstream
//! derives any of that a second time. If the two operands of an addition in this tree do not
//! already have the same type then semantic analysis has a bug, and the verifier in
//! `spec/08-ir.md` is written to say so rather than to paper over it.
//!
//! That rule is worth stating as a cost, because it is one. A tree with explicit conversions is
//! larger than one without, and `(long)a + (long)b` is three nodes where the source has one
//! operator. What it buys is that the walk to the IR has no judgement left in it: it reads what
//! is there. Every compiler that leaves the conversions implicit ends up with two places that
//! know the conversion rules, and the second one is always slightly wrong.
//!
//! ```
//! use rucc_ast::BinaryOp;
//! use rucc_diag::Span;
//! use rucc_sema::{Category, Const, Expr, ExprKind, Tast};
//! use rucc_types::{IntKind, Types};
//!
//! let types = Types::new();
//! let int = types.int(IntKind::Int);
//! let mut tast = Tast::new();
//!
//! let one = tast.add_const(Const::Int(1));
//! let left = tast.expr(Expr::new(ExprKind::Const(one), int, Category::Rvalue), Span::DUMMY);
//! let right = tast.expr(Expr::new(ExprKind::Const(one), int, Category::Rvalue), Span::DUMMY);
//! let sum = ExprKind::Binary { op: BinaryOp::Add, lhs: left, rhs: right };
//! let sum = tast.expr(Expr::new(sum, int, Category::Rvalue), Span::DUMMY);
//!
//! assert_eq!(tast[sum].ty, int);
//! assert_eq!(tast.counts().exprs, 3);
//! ```
//!
//! # What is not in the tree
//!
//! A `typedef` is not, because it is a name for a type and the type table keeps it as sugar. An
//! enumerator is not, because it is a constant and the expressions that used it hold the value.
//! A tag is not, for the same reason. What is left is the objects and the functions, which are
//! what has to exist at run time and what the walk to the IR wants a list of.

#![doc(html_root_url = "https://docs.rs/rucc-sema/0.2.4")]

mod decl;
mod expr;
mod print;
mod stmt;
mod tast;

pub use crate::decl::{
    Decl, DeclId, DeclKind, DeclList, DeclRef, Definition, InitEntry, InitList, Linkage,
    StorageDuration,
};
pub use crate::expr::{Category, Conversion, Expr, ExprId, ExprKind, ExprList, ExprRef};
pub use crate::print::{Printer, print};
pub use crate::stmt::{Case, CaseId, CaseList, Stmt, StmtId, StmtList, StmtRef};
pub use crate::tast::{Const, ConstId, Counts, Label, LabelId, StrId, Tast};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M2";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
