//! `abs`, `labs` and `llabs`, which are the magnitude of an integer and not a call.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! These are the first of the family the design calls the library calls with known semantics, and
//! they are the family where the plain name is enough. `abs` is reserved to the implementation by
//! C23 7.1.3, so a program that writes it means the `abs` the library promises, and a compiler
//! that knows what that one does may write the four instructions instead of the call. Every C
//! compiler does, which is why `gcc.c-torture/execute/20021127-1.c` defines `llabs` to abort and
//! expects the call not to reach it.
//!
//! # Why the declaration is looked at and not only the name
//!
//! The prefixed spellings are decided by the name, the way the rest of `check/builtin.rs` decides
//! them, because the prefix is what says the name belongs to the implementation. The plain ones
//! cannot be: `abs` is only the library's `abs` where nothing else has taken it. So the callee has
//! to be a function with external linkage whose type is exactly the one the library gives that
//! name, which is what a program that means its own thing by the name does not have. A `static
//! long long llabs(long long)` is that program, measured against gcc 16.2.0, which calls it.
//!
//! `-fno-builtin`, `-fno-builtin-<name>` and `-ffreestanding` are the other half of the same
//! question and are answered in [`Context::means_the_library`]. A freestanding program has no C
//! library, so there is no promise about the name for the compiler to rely on.
//!
//! # Why this is the magnitude and not a fold
//!
//! Nothing here needs the argument to be a constant. gcc expands the call inline at `-O0` and so
//! does this, because the point is not that `llabs(-1)` is one, it is that the call does not
//! happen. A program that defines the name and calls it is a program where folding the constant
//! case and calling in the rest would still be wrong.
//!
//! What the magnitude of the most negative value is, is the value itself, here and in gcc. C says
//! the result is undefined when it cannot be represented, and both compilers reach that answer by
//! doing the arithmetic rather than by deciding anything.

use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_types::{IntKind, TypeKind};

use crate::check::Checker;
use crate::decl::{DeclKind, Linkage};
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// One name of the family, and the type the library gives it.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// The plain name, which is the one the library defines and the one a program usually writes.
    name: &'static str,
    /// The prefixed spelling, which is a row of `features.toml` and means this whatever the
    /// program has done with the plain name.
    builtin: &'static str,
    /// The parameter type and the result type, which are the same type. `intmax_t` is not here
    /// because it is a different type on two targets and `imaxabs` is not a row of the table.
    at: IntKind,
}

/// Every name in the family.
///
/// The prefixed spellings are rows of `features.toml` carrying the type this checks against, and
/// the test at the bottom of this file is what keeps the two from drifting apart.
const FAMILY: &[Row] = &[
    Row { name: "abs", builtin: "__builtin_abs", at: IntKind::Int },
    Row { name: "labs", builtin: "__builtin_labs", at: IntKind::Long },
    Row { name: "llabs", builtin: "__builtin_llabs", at: IntKind::LongLong },
];

impl Checker<'_> {
    /// The magnitude a call to one of the absolute value functions is, if the call is one.
    ///
    /// Answers nothing for every other call in the program, which is nearly every call, so the
    /// tests that cost a byte go first.
    ///
    /// Taken after the call has been checked rather than before it, for the reason
    /// `check/builtin/expect.rs` gives at more length: the prototype is what reports the argument
    /// count, converts the argument to the parameter type and refuses a structure handed to it,
    /// and all of that would have to be written again here to gain nothing.
    pub(in crate::check) fn abs_builtin_value(
        &mut self,
        callee: ExprId,
        function: Option<Symbol>,
        args: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        let row = *FAMILY
            .iter()
            .find(|row| row.name == spelled || row.builtin == spelled)
            .filter(|_| self.cx.means_the_library(spelled))?;
        if !self.callee_is_the_library_one(callee, row) {
            return None;
        }
        let &operand = args.first()?;
        if self.is_poisoned(operand) {
            return Some(self.poison(span));
        }
        let ty = self.types.int(row.at);
        Some(self.tast.expr(Expr::new(ExprKind::Abs { operand }, ty, Category::Rvalue), span))
    }

    /// Whether what is being called is a declaration of the library function this name is, rather
    /// than something else the program gave the name to.
    ///
    /// Three questions. It has to be a function and not a pointer some object holds, because a
    /// call through a pointer reaches whatever the pointer holds and no declaration decides that.
    /// It has to have external linkage, because a `static` one is the program's own function and
    /// the name outside the file is somebody else's problem. And its type has to be the one the
    /// library gives the name, spelled with a prototype, because a program that declared `long
    /// long llabs()` has not said what it takes and one that declared `int llabs(int)` has said
    /// something else.
    fn callee_is_the_library_one(&self, callee: ExprId, row: Row) -> bool {
        let mut node = callee;
        // The callee arrives having decayed from a function to a pointer to one, which is a
        // conversion the language performed rather than anything the program wrote.
        while let ExprKind::Convert { operand, .. } = self.tast[node].kind {
            node = operand;
        }
        let ExprKind::Decl(decl) = self.tast[node].kind else {
            return false;
        };
        let decl = &self.tast[decl];
        if decl.kind != DeclKind::Function || decl.linkage != Linkage::External {
            return false;
        }
        let TypeKind::Function(id) = self.types.kind(self.types.canonical(decl.ty)) else {
            return false;
        };
        let signature = self.types.signature(id);
        let wanted = self.types.int(row.at);
        signature.prototyped
            && !signature.variadic
            && self.types.canonical(signature.ret) == wanted
            && signature.params.len() == 1
            && self.types.canonical(signature.params[0]) == wanted
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every prefixed spelling here has to be a row of the table carrying the type this checks a
    /// declaration against, or the two lists say different things about what `llabs` takes.
    #[test]
    fn every_prefixed_spelling_is_a_row_of_the_table_with_the_type_this_expects() {
        for row in FAMILY {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, row.builtin) else {
                panic!("{} is answered here and is not in features.toml", row.builtin);
            };
            assert_eq!(feature.status, Status::Implemented, "{}", row.builtin);
            let written = match row.at {
                IntKind::Int => "int(int)",
                IntKind::Long => "long(long)",
                IntKind::LongLong => "long long(long long)",
                other => panic!("{other:?} is not one of the three widths this family has"),
            };
            assert_eq!(feature.signature, written, "{}", row.builtin);
        }
    }

    /// The prefixed spelling of each is the plain one with the prefix on it, which is what makes
    /// the two spellings one row rather than two.
    #[test]
    fn the_prefixed_spelling_is_the_plain_name_with_the_prefix_on_it() {
        for row in FAMILY {
            assert_eq!(row.builtin.strip_prefix("__builtin_"), Some(row.name));
        }
    }
}
