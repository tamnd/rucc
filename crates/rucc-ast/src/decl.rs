//! Declarations and declarators.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.5.
//!
//! # Which way a declarator reads
//!
//! C declarations are inside-out. In `int (*f[3])(char)` the name is `f` and the type is built
//! by reading outward from it: array of three, pointer to, function taking `char`, returning
//! the `int` that the specifiers named. So a [`Declarator`] is a name plus a list of
//! [`Derived`] steps in exactly that spoken order, and the type is built by folding the list
//! from its end back to its start onto the specifier type. The parser produces the list by
//! pushing while it descends into the parentheses and reversing on the way out, which is the
//! standard trick and the reason the list is flat rather than a tree.
//!
//! An abstract declarator, the kind with no name in it, is the same structure with
//! [`Declarator::name`] absent. Sharing the representation is deliberate: the two grammars are
//! the same grammar with one production made optional, and compilers that write them twice end
//! up accepting different things in a cast than in a parameter.

use rucc_base::Symbol;
use rucc_diag::Span;

use crate::asm::AsmId;
use crate::ast::{AttrList, DeclList, DerivedList, InitDeclaratorList, ParamList, StrId};
use crate::expr::ExprId;
use crate::init::InitId;
use crate::spec::{DeclSpecsId, Quals};
use crate::stmt::StmtId;

/// How deeply declarators may nest before the parser gives up.
///
/// A cap rather than a recursion limit, because the thing that overflows is the stack and the
/// input that overflows it is three lines of generated code. GCC has a limit in the same spirit
/// and a different number.
pub const MAX_DECLARATOR_DEPTH: usize = 200;

/// A declaration in the declaration arena.
pub type DeclId = rucc_base::Idx<Decl>;

/// One declaration.
///
/// A declaration, not a declarator: `int a, *b = 0;` is one [`Decl::Var`] holding one specifier
/// list and two [`InitDeclarator`]s. Keeping the grouping means a diagnostic can point at the
/// specifiers that both declarators share, and it means the printer puts the source back
/// together the way it was written.
///
/// Twenty-four bytes, set by [`Decl::Function`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decl {
    /// A parse that did not work out. Poisoned, as [`Expr::Error`](crate::Expr::Error) is.
    Error,
    /// Specifiers and zero or more declarators.
    ///
    /// Zero is not a mistake: `struct S { int x; };` declares a tag and nothing else, and
    /// `int;` is the same shape with a diagnostic attached.
    Var {
        /// What the declaration says before the first declarator.
        specs: DeclSpecsId,
        /// The declarators, each with its own initializer.
        declarators: InitDeclaratorList,
    },
    /// A function definition, which is the one declaration with a body.
    ///
    /// Attributes written after the declarator go into `specs` rather than onto the declarator,
    /// because a definition has exactly one declarator and everything on it appertains to the
    /// same declaration however it was written.
    Function {
        /// The specifiers.
        specs: DeclSpecsId,
        /// The declarator, whose outermost derivation is the function one.
        declarator: DeclaratorId,
        /// The declarations between the parameter list and the body in an old-style
        /// definition, and empty for a modern one.
        params: DeclList,
        /// The compound statement.
        body: StmtId,
    },
    /// `static_assert(cond)` or `static_assert(cond, "message")`.
    StaticAssert {
        /// The condition, which must be a constant expression.
        cond: ExprId,
        /// The message, absent in the one-argument form C23 added.
        message: Option<StrId>,
    },
    /// `asm("...")` at file scope.
    Asm(AsmId),
    /// `[[...]];` on its own, which C23 allows and which appertains to nothing.
    Attributes(AttrList),
}

/// One declarator of a declaration, with whatever follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitDeclarator {
    /// The declarator.
    pub declarator: DeclaratorId,
    /// The initializer, if there was one.
    pub init: Option<InitId>,
    /// An `asm("name")` label, which is how a declaration is given an assembler name that is
    /// not its identifier. Common in the C library headers and in the kernel.
    pub asm_label: Option<StrId>,
    /// Attributes written after this declarator, which appertain to it alone and not to the
    /// declarators beside it.
    pub attrs: AttrList,
    /// From the start of the declarator to the end of the initializer.
    pub span: Span,
}

/// A declarator, in the side table.
pub type DeclaratorId = rucc_base::Idx<Declarator>;

/// A name and the steps that build its type outward from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Declarator {
    /// The name, absent in an abstract declarator.
    pub name: Option<Symbol>,
    /// Just the name, for the diagnostic that points at it rather than at the whole thing.
    pub name_span: Span,
    /// The derivations, from the name outward, folded from the end onto the specifier type.
    pub derived: DerivedList,
    /// The whole declarator.
    pub span: Span,
}

impl Declarator {
    /// Whether this declarator has no name, which is what makes it abstract.
    #[must_use]
    pub const fn is_abstract(&self) -> bool {
        self.name.is_none()
    }
}

/// One step in a declarator, taking a type to another type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derived {
    /// `*`, with the qualifiers that go on the pointer rather than on what it points at.
    Pointer {
        /// The qualifiers written after the star.
        quals: Quals,
        /// Attributes written after the star, which GCC allows there.
        attrs: AttrList,
    },
    /// `[...]`.
    Array {
        /// What was between the brackets.
        size: ArraySize,
        /// The qualifiers in `int a[const 4]`, which are only legal on a parameter and which
        /// qualify the pointer the parameter becomes.
        quals: Quals,
        /// Whether `static` was written, as in `int a[static 4]`, which promises the caller
        /// passes at least that many elements. It changes no ABI and it is worth keeping,
        /// because it licenses a diagnostic and it can inform alias analysis.
        has_static: bool,
    },
    /// `(...)`.
    Function {
        /// The parameters.
        params: ParamList,
        /// Whether the list ended with an ellipsis.
        variadic: bool,
        /// Which of the four forms the list was written in.
        kind: ParamKind,
    },
}

/// What was written between a declarator's brackets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArraySize {
    /// Nothing, as in `int a[]`, which is an incomplete type or a parameter depending on
    /// where it is written.
    Unspecified,
    /// `*`, which is a variably-modified type in a prototype whose size is not named.
    Star,
    /// An expression, which is a constant in an ordinary array and anything at all in a VLA.
    Expr(ExprId),
}

/// Which of the four shapes a function declarator's parameter list has.
///
/// The distinction between [`ParamKind::Void`] and [`ParamKind::Empty`] is not pedantry. Before
/// C23, `int f()` says nothing about the parameters and `int f(void)` says there are none; in
/// C23 they mean the same thing. Projects moving to `gnu23` hit exactly this, so the two are
/// told apart here and diagnosed where it matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// A list of parameter declarations, named or abstract.
    Prototype,
    /// `(void)`.
    Void,
    /// `()`.
    Empty,
    /// A list of bare identifiers, which is an old-style definition's parameter list.
    Identifiers,
}

/// One parameter of a function declarator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Param {
    /// The specifiers, absent in an old-style identifier list where the name stands alone and
    /// its type comes from a declaration after the parenthesis.
    pub specs: Option<DeclSpecsId>,
    /// The declarator, which is abstract when the parameter has no name.
    pub declarator: DeclaratorId,
    /// Attributes written on the parameter.
    pub attrs: AttrList,
    /// The whole parameter.
    pub span: Span,
}

/// One entry in a struct or union member list.
///
/// A member list is a list of declarations, and since C23 a static assertion is allowed to be
/// one of them. Keeping the assertion in the list rather than in a second list beside it is what
/// preserves the order the members were written in, which the printer needs and which a
/// diagnostic about the member after the assertion needs too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Member {
    /// A member declaration.
    Field(Field),
    /// `static_assert(cond)` or `static_assert(cond, "message")` among the members.
    StaticAssert {
        /// The condition, which must be a constant expression.
        cond: ExprId,
        /// The message, absent in the one-argument form.
        message: Option<StrId>,
        /// The whole assertion, semicolon included.
        span: Span,
    },
}

/// One member of a struct or a union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// The specifiers, shared by the members declared together.
    pub specs: DeclSpecsId,
    /// The declarator, absent for an anonymous struct or union member and for an unnamed
    /// bit-field.
    pub declarator: Option<DeclaratorId>,
    /// The width, for a bit-field.
    pub bits: Option<ExprId>,
    /// Attributes on this member.
    pub attrs: AttrList,
    /// The whole member.
    pub span: Span,
}

/// One enumerator of an enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enumerator {
    /// The name, which goes into the ordinary identifier namespace and not the tag one.
    pub name: Symbol,
    /// The `= value`, if there was one.
    pub value: Option<ExprId>,
    /// Attributes on this enumerator, which C23 allows.
    pub attrs: AttrList,
    /// The whole enumerator.
    pub span: Span,
}

/// A type name, in the side table.
pub type TypeNameId = rucc_base::Idx<TypeName>;

/// A type written where a type is expected rather than where a declaration is.
///
/// The operand of a cast, of `sizeof`, of `_Generic`, of a compound literal. Specifiers plus an
/// abstract declarator, which is a declarator like any other with no name in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeName {
    /// The specifiers.
    pub specs: DeclSpecsId,
    /// The abstract declarator, which is empty when the type is just its specifiers.
    pub declarator: DeclaratorId,
    /// The whole type name.
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declaration_is_twenty_four_bytes() {
        // Set by the function definition, which is the only one carrying four fields. If this
        // fails, something grew and the fix is a side table.
        assert_eq!(size_of::<Decl>(), 24);
    }

    #[test]
    fn a_declaration_id_is_four_bytes_even_when_optional() {
        assert_eq!(size_of::<DeclId>(), 4);
        assert_eq!(size_of::<Option<DeclId>>(), 4);
    }

    #[test]
    fn a_declarator_with_no_name_is_abstract() {
        let d = Declarator {
            name: None,
            name_span: Span::DUMMY,
            derived: DerivedList::EMPTY,
            span: Span::DUMMY,
        };
        assert!(d.is_abstract());
    }
}
