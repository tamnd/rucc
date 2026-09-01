//! Typed expressions.
//!
//! Design: `spec/07-types-and-semantics.md` sections 7.2 and 7.12.
//!
//! Every node here has a type and a value category, and every conversion the language performs
//! without being asked is a [`Conversion`] node written into the tree. That is the whole point
//! of the typed tree: nothing downstream is allowed to work out that an `int` and a `long` must
//! have met somewhere, because if the two operands of an addition do not already have the same
//! type then semantic analysis has a bug and the verifier is entitled to say so.
//!
//! The operators are [`rucc_ast::UnaryOp`] and [`rucc_ast::BinaryOp`], the same ones the parser
//! read, rather than a second set with the same names. What the typed tree adds is not different
//! operators, it is knowing what they are applied to.

use rucc_ast::{BinaryOp, UnaryOp};
use rucc_base::{Idx, IdxRange};
use rucc_types::TypeId;

use crate::decl::DeclId;
use crate::stmt::StmtId;
use crate::tast::{ConstId, LabelId, StrId};

/// One typed expression in the arena.
pub type ExprId = Idx<Expr>;

/// The table of references to expressions, which is what a call's arguments are a run of.
#[derive(Debug)]
pub struct ExprRef;

/// A run of expressions.
pub type ExprList = IdxRange<ExprRef>;

/// An expression, its type, and what may be done with it.
///
/// Twenty four bytes: the kind, the type it has, and the category it is in. The type is in the
/// node rather than in a table beside it, which is the opposite of what the untyped tree does
/// with spans, because everything that walks this tree reads the type at every node and almost
/// nothing reads the span at any node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expr {
    /// What the expression is.
    pub kind: ExprKind,
    /// The type it has, after every conversion that applies to it.
    pub ty: TypeId,
    /// What may be done with it.
    pub category: Category,
}

impl Expr {
    /// An expression of the given kind, type and category.
    #[must_use]
    pub const fn new(kind: ExprKind, ty: TypeId, category: Category) -> Expr {
        Expr { kind, ty, category }
    }
}

/// What may be done with an expression, which C decides rather than the programmer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// A value. It has no address and nothing may be assigned to it.
    Rvalue,
    /// An object. It has an address, it may be assigned to when it is not `const`, and reading
    /// it is a [`Conversion::Lvalue`] rather than something a reader has to remember.
    Lvalue,
    /// A bit-field, which is an lvalue whose address cannot be taken and whose assignment
    /// truncates to the declared width. Kept apart from an ordinary lvalue because the two
    /// rules above are the ones a compiler forgets.
    Bitfield,
    /// A function designator, which is not an lvalue and which decays to a pointer everywhere
    /// except under `sizeof` and `&`.
    Function,
}

/// What an expression is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprKind {
    /// A node that was already the subject of a diagnostic.
    ///
    /// Poisoned, in the sense of `spec/06-lexer-and-parser.md` section 6.8: nothing is reported
    /// about one of these, which is what stops one bad declaration becoming forty bad uses.
    Error,
    /// A constant, in the value table. Every constant that could be folded already has been.
    Const(ConstId),
    /// A string literal, which is an array of characters with static storage duration.
    Str(StrId),
    /// A use of a declared object or function.
    Decl(DeclId),
    /// `base.field` or, after the pointer has been dereferenced, `base->field`.
    Member {
        /// The object the field is in.
        base: ExprId,
        /// Which field, as an index into the record's field list rather than as a name, since
        /// the lookup happened here and nothing after this should repeat it.
        field: u32,
    },
    /// `base[index]`, with the pointer operand first however it was written.
    ///
    /// Kept as a subscript rather than rewritten into `*(base + index)` because the rewriting
    /// has exactly one home, which is the walk to the IR, and because a diagnostic about a
    /// subscript should talk about a subscript.
    Subscript {
        /// The pointer, which has already decayed if it was an array.
        base: ExprId,
        /// The integer.
        index: ExprId,
    },
    /// `callee(args)`, with the arguments already converted to the parameter types.
    Call {
        /// The function, which is a pointer to a function after its decay.
        callee: ExprId,
        /// The arguments, in order, each converted to what the prototype asks for and each
        /// promoted where the prototype does not say.
        args: ExprList,
    },
    /// A prefix or postfix operator on one operand.
    Unary {
        /// Which operator.
        op: UnaryOp,
        /// What it applies to.
        operand: ExprId,
    },
    /// A binary operator on two operands of the same type, except for the shifts and the
    /// pointer arithmetic, where the two sides legitimately differ.
    Binary {
        /// Which operator.
        op: BinaryOp,
        /// The left side.
        lhs: ExprId,
        /// The right side.
        rhs: ExprId,
    },
    /// `lhs = rhs`, or a compound assignment with the operator kept as written.
    Assign {
        /// The operator of a compound assignment, absent for a plain one.
        op: Option<BinaryOp>,
        /// The type the operation is performed in, which is the node's own type for a plain
        /// assignment and for most compound ones.
        ///
        /// It is here because `a op= b` is not `a = a op b` with the conversions left out, and
        /// the difference is not academic: in `int i = 5; i /= 0.5;` the division happens in
        /// `double` and the answer is ten, and a compiler that converts the right side to `int`
        /// first divides by zero. The left side is an lvalue and cannot carry a conversion node
        /// of its own, so the type it is read into is written here instead, which is what clang
        /// calls the computation type and for the same reason.
        computation: TypeId,
        /// What is assigned to, which is an lvalue.
        lhs: ExprId,
        /// What is assigned.
        rhs: ExprId,
    },
    /// `cond ? then : otherwise`, with both arms already converted to the common type.
    Cond {
        /// The condition, converted to `bool`.
        cond: ExprId,
        /// The arm taken when it is true. GNU's `cond ?: otherwise` has this equal to the
        /// condition before its conversion, so the value is computed once.
        then: ExprId,
        /// The arm taken when it is false.
        otherwise: ExprId,
    },
    /// `lhs, rhs`, whose value is the right side and whose left side is evaluated and dropped.
    Comma {
        /// Evaluated first, for its effects.
        lhs: ExprId,
        /// The value.
        rhs: ExprId,
    },
    /// A cast the program wrote. The type is the node's type.
    Cast(ExprId),
    /// A conversion the language performed. The type is the node's type.
    Convert {
        /// Which conversion, so that a reader and the verifier can both tell what happened
        /// rather than comparing the two types and guessing.
        kind: Conversion,
        /// What was converted.
        operand: ExprId,
    },
    /// `(T){ ... }`, which is an unnamed object with an initializer and not a conversion.
    CompoundLiteral(DeclId),
    /// `({ ... })`, GNU's statement expression, whose value is its last expression statement.
    StmtExpr(StmtId),
    /// `&&label`, GNU's label address.
    LabelAddr(LabelId),
}

/// A conversion the language performs without being asked.
///
/// Each of these is a node in the tree rather than a difference between two types that a later
/// pass notices. The IR builder is entitled to assume it never has to insert one, and the
/// verifier in `spec/08-ir.md` checks that assumption on every function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conversion {
    /// Reading an object, which drops the qualifiers and turns an lvalue into a value.
    Lvalue,
    /// An array becoming a pointer to its first element.
    ArrayDecay,
    /// A function becoming a pointer to itself.
    FunctionDecay,
    /// One arithmetic type to another. The integer promotions, the usual arithmetic
    /// conversions, and the conversions an assignment or an argument performs are all this.
    Arithmetic,
    /// A pointer to another pointer type, which includes both directions of `void *`.
    Pointer,
    /// A scalar to `bool`, which is a comparison against zero rather than a truncation, and
    /// which is why it is not [`Conversion::Arithmetic`].
    Bool,
    /// A null pointer constant becoming a pointer, which is not the same as converting the
    /// integer zero, because the constant may have any integer type and `(void *)0` is one.
    NullPointer,
    /// A value being discarded, which is what a cast to `void` and an expression statement do.
    Void,
}

impl Conversion {
    /// How the conversion is written in the typed tree's textual form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Conversion::Lvalue => "lvalue",
            Conversion::ArrayDecay => "array-decay",
            Conversion::FunctionDecay => "function-decay",
            Conversion::Arithmetic => "arithmetic",
            Conversion::Pointer => "pointer",
            Conversion::Bool => "bool",
            Conversion::NullPointer => "null-pointer",
            Conversion::Void => "void",
        }
    }
}
