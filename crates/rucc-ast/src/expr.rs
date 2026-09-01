//! Expressions.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.2.
//!
//! Nothing here is desugared. `a[i]` is a subscript and not `*(a + i)`, `a += b` is a compound
//! assignment and not `a = a + b`, and a cast keeps the type the source wrote rather than the
//! conversion it turns into. That is what makes a diagnostic able to quote the program back to
//! the person who wrote it, and it is what makes `--emit=ast` worth looking at. The rewriting
//! happens when the IR is built, in `spec/08-ir.md`.

use rucc_base::Symbol;

use crate::ast::{CharId, DesignatorList, ExprList, FloatId, GenericList, IntId, StrId};
use crate::decl::TypeNameId;
use crate::init::InitId;
use crate::stmt::StmtId;

/// An expression in the expression arena.
pub type ExprId = rucc_base::Idx<Expr>;

/// One expression node.
///
/// Sixteen bytes, which is what the widest variant needs and what the whole arena therefore
/// costs per node. Anything that would not fit is an index into a side table on
/// [`Ast`](crate::Ast), which is why a call holds a range and not a vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expr {
    /// A parse that did not work out.
    ///
    /// Poisoned, per section 6.8: semantic analysis says nothing about a node that is already
    /// the result of a diagnostic, which is the mechanism that stops one syntax error becoming
    /// forty type errors.
    Error,
    /// An identifier, before anything has looked it up.
    ///
    /// The parser already knows whether the name is a typedef name, because it had to know to
    /// parse the surrounding text at all, but it does not resolve it to a declaration. That is
    /// semantic analysis, which has the scopes and the linkage rules.
    Name(Symbol),
    /// An integer constant, in the constant table on [`Ast`](crate::Ast).
    Int(IntId),
    /// A floating constant.
    Float(FloatId),
    /// A character constant.
    Char(CharId),
    /// A string literal, with the adjacent ones already joined onto it by phase 7.
    Str(StrId),
    /// `true` or `false`, which C23 made constants rather than macros.
    Bool(bool),
    /// `nullptr`.
    Nullptr,
    /// `base[index]`, in the order it was written, which is not always the pointer first.
    Index {
        /// The left operand.
        base: ExprId,
        /// The operand in the brackets.
        index: ExprId,
    },
    /// `callee(args)`.
    Call {
        /// What is being called, which is an expression and not necessarily a name.
        callee: ExprId,
        /// The arguments, in order.
        args: ExprList,
    },
    /// `base.name` or `base->name`.
    Member {
        /// The left operand.
        base: ExprId,
        /// The member name, which lives in its own namespace and is not looked up here.
        name: Symbol,
        /// Whether it was written with an arrow.
        arrow: bool,
    },
    /// A prefix or postfix operator on one operand.
    Unary {
        /// Which operator.
        op: UnaryOp,
        /// The operand.
        operand: ExprId,
    },
    /// A binary operator, including the ones that do not evaluate both sides.
    Binary {
        /// Which operator.
        op: BinaryOp,
        /// The left operand.
        lhs: ExprId,
        /// The right operand.
        rhs: ExprId,
    },
    /// `lhs = rhs`, or a compound assignment with the operator kept as written.
    Assign {
        /// The operator in a compound assignment, and `None` for a plain one.
        op: Option<BinaryOp>,
        /// The left operand.
        lhs: ExprId,
        /// The right operand.
        rhs: ExprId,
    },
    /// `cond ? then : otherwise`, where `then` is absent in GNU's `cond ?: otherwise`.
    Cond {
        /// The condition.
        cond: ExprId,
        /// The second operand, absent when the middle was left out.
        then: Option<ExprId>,
        /// The third operand.
        otherwise: ExprId,
    },
    /// `lhs, rhs`.
    ///
    /// Its own node rather than a [`BinaryOp`], because the comma operator is a sequence point
    /// with a discarded left side and shares nothing with arithmetic but its spelling.
    Comma {
        /// The operand whose value is thrown away.
        lhs: ExprId,
        /// The operand whose value the expression has.
        rhs: ExprId,
    },
    /// `(ty)operand`.
    Cast {
        /// The type name in the parentheses.
        ty: TypeNameId,
        /// What is being converted.
        operand: ExprId,
    },
    /// `(ty){ ... }`, which is an object and not a conversion.
    CompoundLiteral {
        /// The type name in the parentheses.
        ty: TypeNameId,
        /// The braced initializer.
        init: InitId,
    },
    /// `sizeof operand`, written without parentheses around a type.
    SizeofExpr(ExprId),
    /// `sizeof (ty)`.
    SizeofType(TypeNameId),
    /// `alignof operand`, which is GNU's `__alignof__` since ISO C only has the type form.
    AlignofExpr(ExprId),
    /// `alignof (ty)`.
    AlignofType(TypeNameId),
    /// `_Generic(control, ...)`.
    Generic {
        /// The controlling expression, which is never evaluated.
        control: ExprId,
        /// The associations, in the order they were written, including the default one.
        assocs: GenericList,
    },
    /// `({ ... })`, GNU's statement expression, whose value is its last expression statement.
    StmtExpr(StmtId),
    /// `&&label`, GNU's label address.
    LabelAddr(Symbol),
    /// `__builtin_offsetof(ty, path)`, where `path` is a member and not an expression.
    Offsetof {
        /// The type being measured.
        ty: TypeNameId,
        /// The member path, which is a designator list because `a.b[3].c` is legal here.
        path: DesignatorList,
    },
    /// `__builtin_choose_expr(cond, then, otherwise)`.
    ///
    /// The whole reason this exists is that the branch not chosen is never type checked, so it
    /// has to survive to semantic analysis as itself rather than as a conditional.
    ChooseExpr {
        /// The condition, which must be a constant expression.
        cond: ExprId,
        /// The branch taken when the condition is nonzero.
        then: ExprId,
        /// The other branch.
        otherwise: ExprId,
    },
    /// `__builtin_types_compatible_p(a, b)`.
    TypesCompatible {
        /// The first type.
        a: TypeNameId,
        /// The second type.
        b: TypeNameId,
    },
    /// `__builtin_va_arg(list, ty)`.
    VaArg {
        /// The argument list.
        list: ExprId,
        /// The type being fetched.
        ty: TypeNameId,
    },
    /// `__extension__ operand`, which turns the pedantic diagnostics off inside it.
    Extension(ExprId),
}

/// An operator with one operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// `+`, which is not a no-op: it promotes.
    Plus,
    /// `-`.
    Minus,
    /// `!`.
    Not,
    /// `~`.
    BitNot,
    /// `*`.
    Deref,
    /// `&`.
    AddrOf,
    /// `++x`.
    PreInc,
    /// `--x`.
    PreDec,
    /// `x++`.
    PostInc,
    /// `x--`.
    PostDec,
    /// `__real__ x`, GNU.
    Real,
    /// `__imag__ x`, GNU.
    Imag,
}

impl UnaryOp {
    /// The spelling, for the printer and for diagnostics.
    #[must_use]
    pub const fn spelling(self) -> &'static str {
        match self {
            UnaryOp::Plus => "+",
            UnaryOp::Minus => "-",
            UnaryOp::Not => "!",
            UnaryOp::BitNot => "~",
            UnaryOp::Deref => "*",
            UnaryOp::AddrOf => "&",
            UnaryOp::PreInc | UnaryOp::PostInc => "++",
            UnaryOp::PreDec | UnaryOp::PostDec => "--",
            UnaryOp::Real => "__real__",
            UnaryOp::Imag => "__imag__",
        }
    }

    /// Whether the operator is written after its operand.
    #[must_use]
    pub const fn is_postfix(self) -> bool {
        matches!(self, UnaryOp::PostInc | UnaryOp::PostDec)
    }
}

/// An operator with two operands.
///
/// The comma operator is not here; it is [`Expr::Comma`]. Assignment is not here either, and
/// the operator part of a compound assignment reuses this enum, which is why the values that
/// cannot appear in one ([`BinaryOp::LogAnd`] and the comparisons) are simply never built by
/// the parser rather than being a second enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// `*`.
    Mul,
    /// `/`.
    Div,
    /// `%`.
    Rem,
    /// `+`.
    Add,
    /// `-`.
    Sub,
    /// `<<`.
    Shl,
    /// `>>`.
    Shr,
    /// `<`.
    Lt,
    /// `>`.
    Gt,
    /// `<=`.
    Le,
    /// `>=`.
    Ge,
    /// `==`.
    Eq,
    /// `!=`.
    Ne,
    /// `&`.
    BitAnd,
    /// `^`.
    BitXor,
    /// `|`.
    BitOr,
    /// `&&`, which does not evaluate its right operand unless it has to.
    LogAnd,
    /// `||`, likewise.
    LogOr,
}

impl BinaryOp {
    /// The spelling, for the printer and for diagnostics.
    #[must_use]
    pub const fn spelling(self) -> &'static str {
        match self {
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "%",
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
            BinaryOp::Lt => "<",
            BinaryOp::Gt => ">",
            BinaryOp::Le => "<=",
            BinaryOp::Ge => ">=",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitXor => "^",
            BinaryOp::BitOr => "|",
            BinaryOp::LogAnd => "&&",
            BinaryOp::LogOr => "||",
        }
    }

    /// Whether the operator sequences its left operand before its right, which only the two
    /// short-circuiting ones do.
    #[must_use]
    pub const fn is_short_circuit(self) -> bool {
        matches!(self, BinaryOp::LogAnd | BinaryOp::LogOr)
    }

    /// Whether the operator is one of the six relational and equality ones.
    ///
    /// What the six have in common is the answer rather than the operands: it is one bit, and
    /// C makes it an `int` holding zero or one. Everything that has to treat them alike asks
    /// this rather than spelling the six out again.
    #[must_use]
    pub const fn is_comparison(self) -> bool {
        matches!(
            self,
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge | BinaryOp::Eq | BinaryOp::Ne
        )
    }
}

/// One arm of a `_Generic` selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenericAssoc {
    /// The type this arm matches, and `None` for the `default` arm.
    pub ty: Option<TypeNameId>,
    /// The expression the arm gives, which is only evaluated if the arm is chosen.
    pub value: ExprId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expression_is_sixteen_bytes() {
        // The arena is the biggest array in the frontend and the one every pass walks. If this
        // fails, a variant grew and the fix is a side table, not a bigger node.
        assert_eq!(size_of::<Expr>(), 16);
    }

    #[test]
    fn an_expression_id_is_four_bytes_even_when_optional() {
        assert_eq!(size_of::<ExprId>(), 4);
        assert_eq!(size_of::<Option<ExprId>>(), 4);
    }

    #[test]
    fn postfix_increment_is_the_only_kind_that_is_postfix() {
        assert!(UnaryOp::PostInc.is_postfix());
        assert!(UnaryOp::PostDec.is_postfix());
        assert!(!UnaryOp::PreInc.is_postfix());
        assert_eq!(UnaryOp::PostInc.spelling(), UnaryOp::PreInc.spelling());
    }

    #[test]
    fn the_six_relational_and_equality_operators_are_the_comparisons() {
        let all =
            [BinaryOp::Lt, BinaryOp::Gt, BinaryOp::Le, BinaryOp::Ge, BinaryOp::Eq, BinaryOp::Ne];
        assert!(all.iter().all(|op| op.is_comparison()));
        assert!(!BinaryOp::Add.is_comparison());
        assert!(!BinaryOp::LogAnd.is_comparison());
    }

    #[test]
    fn only_the_logical_operators_short_circuit() {
        assert!(BinaryOp::LogAnd.is_short_circuit());
        assert!(BinaryOp::LogOr.is_short_circuit());
        assert!(!BinaryOp::BitAnd.is_short_circuit());
    }
}
