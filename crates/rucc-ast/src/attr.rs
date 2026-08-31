//! Attributes, in both spellings.
//!
//! Design: `spec/06-lexer-and-parser.md` sections 6.6 and 6.7.
//!
//! C23's `[[deprecated]]` and GCC's `__attribute__((deprecated))` mean the same thing and are
//! written in different places, so the syntax each one was written in is kept on the node. The
//! placement rules differ between the two and GCC's are not always what its documentation says,
//! so a diagnostic about where an attribute may go has to know which spelling it is talking
//! about. Nothing here decides what an attribute *means*; that is `spec/13-gnu-extensions.md`
//! and it happens in semantic analysis.

use rucc_base::Symbol;
use rucc_diag::Span;

use crate::ast::AttrArgList;
use crate::expr::ExprId;

/// One attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attribute {
    /// The namespace in `[[gnu::packed]]`, and `None` for an attribute written without one.
    pub namespace: Option<Symbol>,
    /// The name, with GCC's optional pair of leading and trailing underscores already off it,
    /// so that `__packed__` and `packed` are the same symbol here.
    pub name: Symbol,
    /// The arguments, which are not all expressions.
    pub args: AttrArgList,
    /// Which spelling it was written in.
    pub syntax: AttrSyntax,
    /// The whole attribute, from its name to its closing parenthesis.
    pub span: Span,
}

/// Which spelling an attribute was written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrSyntax {
    /// `[[name]]`, the C23 one.
    Standard,
    /// `__attribute__((name))`, the GNU one.
    Gnu,
    /// `__declspec(name)`, which the Windows headers are full of.
    Declspec,
}

/// One argument of an attribute.
///
/// Most attribute arguments are expressions, but a few take a bare identifier that must not be
/// looked up as one: the `printf` in `format(printf, 1, 2)` names an archetype and the `DI` in
/// `mode(DI)` names a machine mode, and neither is a variable. Treating them as expressions is
/// how a compiler ends up reporting an undeclared identifier inside an attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrArg {
    /// An identifier, kept as one.
    Ident(Symbol),
    /// An expression.
    Expr(ExprId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_attribute_argument_is_eight_bytes() {
        assert_eq!(size_of::<AttrArg>(), 8);
    }
}
