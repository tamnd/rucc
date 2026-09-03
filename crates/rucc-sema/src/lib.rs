//! Type checking, conversions, initialization, constant evaluation, and the typed AST.
//!
//! Design: `spec/07-types-and-semantics.md`. Layer rank 7, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! The typed tree is here: the arenas, the nodes for every typed expression and statement, the
//! declarations with their linkage and storage duration, and the flattened initializers. So are
//! the two things the checking rests on, which are the [`Scopes`] a name is resolved against and
//! the [`Conv`] that writes the conversions the language performs without being asked.
//!
//! The [`Checker`] fills the tree in. Expressions are done, which is every operator of 6.5: the
//! ones that name a type, being the cast, `sizeof`, `alignof`, `offsetof`, `_Generic`, `va_arg`
//! and the two `__builtin` forms that take a type name; the compound literal and GNU's cast to a
//! union, which build an object rather than producing a value; and GNU's statement expression and
//! label address. Declarations are done as well: what kind of thing a name is, who else
//! can see it, how long it lives, how much of a definition it is, and what a second declaration of
//! the same name does to the first. Statements are done, and with them the function definition and
//! the walk over a whole translation unit: a body is one scope with its parameters, the labels are
//! resolved over the function rather than in order, each `switch` collects its cases into one
//! table, and `break`, `continue` and `return` are checked against what encloses them. What waits
//! on a control flow graph is reachability, which is where `control reaches end of non-void
//! function` lives.
//!
//! The [`Eval`] that folds a checked expression to a constant is here too, over the arithmetic
//! operators and over the addresses, so `&x`, `&s.field + 3` and a string literal each fold to
//! the object and the offset that a static initializer needs and an object file relocates. That
//! is what a case label, an enumerator, an array bound, a bit-field width and the initializer of
//! an object that exists before the program runs are each going to ask for. So is the type builder, which turns a specifier list and a declarator
//! into a [`TypeId`](rucc_types::TypeId): pointers, arrays including the variable length ones,
//! prototypes, tags referred to and declared, the members of a `struct` or a `union` laid out
//! with their bit-fields, the enumerators of an `enum` with the C23 rules about what they are
//! kept in, and everything a declarator is allowed and not allowed to say about each.
//!
//! Initialization is here, which is the walk that turns an initializer into the list of what
//! goes at which offset: brace elision, designation including the GNU forms, a string literal
//! filling a character array, an array taking its length from what was written into it, and the
//! bit-fields and flexible array members that make an offset more than a number, and each
//! element of an object with static storage duration is required to be a constant expression,
//! which for a pointer means an address and for a `constexpr` object means a number. The unnamed
//! object a compound literal builds is here as well, and it lives as long as the block it was
//! written in or as long as the program where it was written outside one.
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

#![doc(html_root_url = "https://docs.rs/rucc-sema/0.3.1")]

mod asm;
mod check;
mod convert;
mod decl;
mod eval;
mod expr;
mod print;
mod scope;
mod stmt;
mod tast;

pub use crate::asm::{
    Asm, AsmId, AsmOperand, AsmOperandList, LabelList, LabelRef, StrList, StrRef,
};
pub use crate::check::{Checked, Checker, Context, library_name};
pub use crate::convert::Conv;
pub use crate::decl::{
    Decl, DeclId, DeclKind, DeclList, DeclRef, Definition, InitEntry, InitList, Linkage,
    StorageDuration,
};
pub use crate::eval::{Eval, NotConstant};
pub use crate::expr::{Category, Conversion, Expr, ExprId, ExprKind, ExprList, ExprRef};
pub use crate::print::{Printer, print};
pub use crate::scope::{Binding, Scopes, Tag, TagKind};
pub use crate::stmt::{Case, CaseId, CaseList, Stmt, StmtId, StmtList, StmtRef};
pub use crate::tast::{Address, Base, Const, ConstId, Counts, Label, LabelId, StrId, Tast};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M2";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
