//! `__builtin_complex`, a complex value made of two halves exactly as they were written.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! C11 added `CMPLX`, `CMPLXF` and `CMPLXL` to `complex.h` because the obvious way of writing a
//! complex value, `x + y * I`, is arithmetic and gets some values wrong. `0.0 * INFINITY` is a nan,
//! so `CMPLX(0.0, INFINITY)` written that way has a nan for a real half, and `0.0 + -0.0` is a
//! positive zero, so a negative zero never makes it into the imaginary half at all. The standard
//! asks for the halves as they are, and glibc's header gets that from gcc by defining all three
//! macros as a call to this builtin whenever the compiler is gcc 4.7 or later, which is every
//! compiler that says it is gcc. So a program that uses `CMPLX` reaches this name whether or not
//! it has ever heard of it, and a compiler without it stops at an implicit declaration.
//!
//! # What gcc asks of the operands
//!
//! Both have to be of the same real binary floating type, after lvalue conversion and before any
//! other: `__builtin_complex(1.0f, 2.0)` is an error rather than a `_Complex double`, and an
//! integer is refused rather than converted. That is why glibc's macros cast each argument to the
//! type they want first. The decimal types are not binary and are refused too, since gcc has no
//! complex type made of them. The answer is the complex type whose halves have that type, which
//! for the `_FloatN` types is a complex type of their own, as it is in gcc.
//!
//! The messages are gcc 16.2.0's, word for word.
//!
//! # Constants
//!
//! C11 says `CMPLX` of two constants is usable in a static initializer, so the node folds when
//! both halves do. That is in `eval.rs`, next to the other builtins that fold, and it is the
//! pair of constants as they are with nothing computed.

use rucc_ast as ast;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::TypeKind;

use crate::check::Checker;
use crate::eval::bare;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The name, which is a row of `features.toml` with no signature because no one prototype fits.
const NAME: &str = "__builtin_complex";

/// Whether this is the name, which is what the test in `generic.rs` asks of every family whose
/// rows have no signature.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    name == NAME
}

impl Checker<'_> {
    /// Answers a call to `__builtin_complex`, if that is the name.
    ///
    /// Answers nothing for every other name. The caller has already made sure the program did not
    /// declare the name itself, in which case what it declared is what the call is checked
    /// against.
    pub(super) fn parts_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        if self.text(name) != NAME {
            return None;
        }
        Some(self.parts_call(args, span))
    }

    /// The call itself, once the name has been recognised.
    fn parts_call(&mut self, args: ast::ExprList, span: Span) -> ExprId {
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        // Every argument is checked whatever else is wrong with the call, because a mistake
        // inside one of them is worth hearing about even when there is one too many of them.
        let args: Vec<ExprId> = written
            .into_iter()
            .map(|arg| {
                let arg = self.expr(arg);
                self.value(arg)
            })
            .collect();
        if args.len() != 2 {
            self.report(
                Diagnostic::error(format!("wrong number of arguments to '{NAME}'"), span)
                    .with_code("E0511"),
            );
            return self.poison(span);
        }
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return self.poison(span);
        }
        let (real, imag) = (args[0], args[1]);
        // gcc says the same thing whichever of the two is wrong and does not say which, so this
        // does not either.
        let kinds = [real, imag].map(|arg| match bare(&self.types, self.tast[arg].ty) {
            TypeKind::Float(kind) if !kind.is_decimal() => Some(kind),
            _ => None,
        });
        let [Some(kind), Some(other)] = kinds else {
            self.report(
                Diagnostic::error(
                    format!("'{NAME}' operand not of real binary floating-point type"),
                    span,
                )
                .with_code("E0685"),
            );
            return self.poison(span);
        };
        // The same type and not only the same format: `double` and `_Float64` are the same bits
        // on every target here and are still two types to gcc, and so two types here.
        if kind != other {
            self.report(
                Diagnostic::error(format!("'{NAME}' operands of different types"), span)
                    .with_code("E0685"),
            );
            return self.poison(span);
        }
        let ty = self.types.complex_float(kind);
        self.tast.expr(Expr::new(ExprKind::Complex { real, imag }, ty, Category::Rvalue), span)
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The name has to be a row of the roster, or `__has_builtin` says no and a header that asks
    /// takes its fallback path. A row with a signature would be declared and called rather than
    /// answered, and there is no function anywhere to call.
    #[test]
    fn the_name_is_a_row_of_the_table_and_is_answered_not_called() {
        let Some(feature) = rucc_gnu::lookup(Kind::Builtin, NAME) else {
            panic!("{NAME} is answered here and is not in features.toml");
        };
        assert_eq!(feature.status, Status::Implemented);
        assert!(feature.signature.is_empty(), "{NAME} is answered and not called");
    }
}
