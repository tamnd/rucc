//! The arena-allocated AST and its printer.
//!
//! Design: `spec/06-lexer-and-parser.md`. Layer rank 6, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! The tree is here: the three arenas, the side tables, the nodes for every expression,
//! statement and declaration this compiler intends to parse, the declarator representation that
//! the type system reads, and the [`Printer`] that writes any of it back out as C. What fills
//! the tree in is [`rucc-parse`](https://docs.rs/rucc-parse).
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.
//!
//! # What shape the tree is
//!
//! Flat vectors and four-byte indices, per `spec/03-architecture.md` section 3.3. An [`Ast`]
//! owns everything in one translation unit, [`Expr`], [`Stmt`] and [`Decl`] live in three
//! arenas of their own, and anything that would make a node bigger than it needs to be lives in
//! a side table with an index in the node. Spans are in a vector parallel to each arena rather
//! than in the node, because almost nothing that walks the tree reads them.
//!
//! ```
//! use rucc_ast::{Ast, BinaryOp, Expr};
//! use rucc_diag::Span;
//!
//! let mut ast = Ast::new();
//! let left = ast.expr(Expr::Bool(true), Span::new(0, 4));
//! let right = ast.expr(Expr::Bool(false), Span::new(8, 13));
//! let both = ast.expr(Expr::Binary { op: BinaryOp::LogAnd, lhs: left, rhs: right },
//!                     Span::new(0, 13));
//!
//! assert_eq!(ast[left], Expr::Bool(true));
//! assert_eq!(ast.expr_span(both), Span::new(0, 13));
//! assert_eq!(ast.counts().exprs, 3);
//! ```
//!
//! # What the tree does not do
//!
//! It is not desugared and it never will be. `a[i]` is a subscript, `a += b` is a compound
//! assignment, a `for` loop is a `for` loop, and a `switch` is a statement with cases in it
//! rather than a table. Rewriting any of that here would make every diagnostic after this point
//! talk about a program nobody wrote. The rewriting happens once, at IR construction, in
//! `spec/08-ir.md`.
//!
//! It is also untyped. A [`Expr::Name`] is an identifier and not a declaration, a
//! [`TypeSpec::Typedef`] is the name a typedef was written with and not the type behind it, and
//! nothing here holds a type from `rucc-types`. Names are resolved and types are assigned by
//! `rucc-sema`, which produces the typed tree that everything downstream reads.
//!
//! The one place the tree does more than record what was written is [`Builtin`], which holds
//! the type keywords as the multiset they were written in and turns them into a type with
//! [`Builtin::resolve`]. That is a table rather than a judgement, it is the same table in every
//! dialect, and it is much easier to get right with a test next to it than spread across the
//! parser.

#![doc(html_root_url = "https://docs.rs/rucc-ast/0.2.21")]

mod asm;
mod ast;
mod attr;
mod decl;
mod expr;
mod init;
mod print;
mod spec;
mod stmt;

pub use crate::asm::{Asm, AsmId, AsmOperand, AsmQuals};
pub use crate::ast::{
    AsmOperandList, Ast, AttrArgList, AttrList, CharId, Counts, DeclList, DeclRef, DerivedList,
    DesignatorList, EnumeratorList, ExprList, ExprRef, FloatId, GenericList, InitDeclaratorList,
    InitItemList, IntId, MemberList, ParamList, StmtList, StmtRef, StrId, StrList, StrRef,
    SymbolList,
};
pub use crate::attr::{AttrArg, AttrSyntax, Attribute};
pub use crate::decl::{
    ArraySize, Decl, DeclId, Declarator, DeclaratorId, Derived, Enumerator, Field, InitDeclarator,
    MAX_DECLARATOR_DEPTH, Member, Param, ParamKind, TypeName, TypeNameId,
};
pub use crate::expr::{BinaryOp, Expr, ExprId, GenericAssoc, UnaryOp};
pub use crate::init::{Designator, Init, InitId, InitItem};
pub use crate::print::{Printer, print};
pub use crate::spec::{
    AlignSpec, Basic, Builtin, BuiltinError, BuiltinSet, Complexity, DeclSpecs, DeclSpecsId,
    Deduction, FuncSpecs, Quals, RecordKind, Scalar, StorageClass, TypeSpec, TypeofArg,
};
pub use crate::stmt::{ForInit, Stmt, StmtId};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M2";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
