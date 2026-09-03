//! `__builtin_fabs` and `__builtin_copysign`, which are the sign bit and nothing else.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! These two are functions of `math.h` and a program can call them, which is what makes them
//! different from the classification family next door. What makes them different from the library
//! builtins is where the function lives. Everything the library family calls is in the C library,
//! which is on the link line of every program there is, so a call to `abort` that nothing folds
//! still links. `copysign` is in the math library, which is not, and a program that only ever
//! wrote `__builtin_copysign` had no reason to ask for `-lm`, so leaving the call behind turns a
//! program gcc links into one that does not.
//!
//! There is nothing to call anyway. `fabs(x)` is `x` with its sign bit clear and `copysign(x, y)`
//! is `x` with `y`'s sign bit, and neither one rounds, raises anything or has a case it cannot
//! answer. gcc emits no call for either on any target, and neither does this.
//!
//! # Why the bits and not the value
//!
//! `fabs` of a nan is that nan with its sign bit clear, payload and all, and `copysign` of one is
//! that nan with the other operand's sign. That is not what a rewriting into arithmetic would
//! give: `x < 0 ? -x : x` is false for a nan and wrong for a negative zero, whose sign bit is set
//! and which compares equal to a positive one. So the operation is described over the bits, here
//! and in the walk to the IR, and `copysign1.c` in the torture suite is the test that notices,
//! because it compares its answers with `memcmp` rather than with `==`.
//!
//! # What is not here
//!
//! `__builtin_fmax` and the rest of the family that has to look at the values. Those are a
//! comparison and a choice rather than a mask, their nan rules are the library's rather than the
//! hardware's, and the link line question is real for them, so they are their own piece of work.

use rucc_ast as ast;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{FloatKind, TypeId, is_real_floating};

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind, Sign};

/// One name of the family, and what it does.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// The name, spelled the way the program writes it.
    name: &'static str,
    /// Where the sign of the answer comes from.
    op: Sign,
    /// The type the operands are converted to, which is also the type of the answer. Unlike the
    /// classification family none of these is type generic: gcc gives `__builtin_fabs` a `double`
    /// parameter and a `double` result, and the spellings ending in a width their own.
    at: FloatKind,
}

/// Every name in the family.
///
/// The names are also rows of `features.toml`, and the test at the bottom of this file is what
/// keeps the two from drifting.
const FAMILY: &[Row] = &[
    Row { name: "__builtin_fabs", op: Sign::Clear, at: FloatKind::Double },
    Row { name: "__builtin_fabsf", op: Sign::Clear, at: FloatKind::Float },
    Row { name: "__builtin_fabsl", op: Sign::Clear, at: FloatKind::LongDouble },
    Row { name: "__builtin_copysign", op: Sign::Of, at: FloatKind::Double },
    Row { name: "__builtin_copysignf", op: Sign::Of, at: FloatKind::Float },
    Row { name: "__builtin_copysignl", op: Sign::Of, at: FloatKind::LongDouble },
];

/// Whether the roster has a row for this name, which is what the test next door asks.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    FAMILY.iter().any(|row| row.name == name)
}

impl Checker<'_> {
    /// Answers a call to one of the sign builtins, if the name is one.
    ///
    /// Answers nothing when the name is not in the table. The caller has already made sure the
    /// program did not declare the name itself, in which case what it declared is what the call
    /// is checked against and this has no business overriding it.
    pub(super) fn sign_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        let spelled = self.text(name);
        let row = *FAMILY.iter().find(|row| row.name == spelled)?;
        Some(self.sign_call(row, args, span))
    }

    /// The call itself, once the name has been recognised.
    fn sign_call(&mut self, row: Row, args: ast::ExprList, span: Span) -> ExprId {
        let spelled = row.name;
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
        let wanted = if row.op.is_pair() { 2 } else { 1 };
        if args.len() != wanted {
            let how = if args.len() < wanted { "few" } else { "many" };
            self.report(
                Diagnostic::error(format!("too {how} arguments to function '{spelled}'"), span)
                    .with_code("E0511"),
            );
            return self.poison(span);
        }
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return self.poison(span);
        }
        let ty = self.types.float(row.at);
        let converted: Vec<ExprId> =
            args.into_iter().map(|arg| self.convert_operand(arg, ty)).collect();
        if !converted.iter().all(|&arg| is_real_floating(&self.types, self.tast[arg].ty)) {
            // gcc's wording, which is plural for `copysign` however many of the two are wrong.
            let s = if wanted == 2 { "s" } else { "" };
            self.report(
                Diagnostic::error(
                    format!("non-floating-point argument{s} in call to function '{spelled}'"),
                    span,
                )
                .with_code("E0685"),
            );
            return self.poison(span);
        }
        let rhs = converted.get(1).copied();
        self.tast.expr(
            Expr::new(ExprKind::Sign { op: row.op, lhs: converted[0], rhs }, ty, Category::Rvalue),
            span,
        )
    }

    /// One argument converted to the type the name says.
    ///
    /// An argument that is not arithmetic at all stays as it was, so that what is reported about
    /// it is that it is not a floating point value rather than that it will not convert.
    fn convert_operand(&mut self, arg: ExprId, ty: TypeId) -> ExprId {
        if rucc_types::is_arithmetic(&self.types, self.tast[arg].ty) {
            return self.conv().to_type(arg, ty);
        }
        arg
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every name here has to be a row of the roster, or it is a builtin `__has_builtin` has
    /// never heard of and a header that asks takes its fallback path for no reason. A row with a
    /// signature would be called rather than answered, which for these is the link error the
    /// whole file exists to avoid.
    #[test]
    fn every_name_in_the_family_is_a_row_of_the_table_and_is_answered_not_called() {
        for row in FAMILY {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, row.name) else {
                panic!("{} is answered here and is not in features.toml", row.name);
            };
            assert_eq!(feature.status, Status::Implemented, "{}", row.name);
            assert!(feature.signature.is_empty(), "{} is answered and not called", row.name);
        }
    }

    /// The names are what the lookup searches, so a name written twice is a second row nothing
    /// can ever reach.
    #[test]
    fn no_name_is_in_the_table_twice() {
        let mut names: Vec<&str> = FAMILY.iter().map(|row| row.name).collect();
        names.sort_unstable();
        let all = names.len();
        names.dedup();
        assert_eq!(names.len(), all, "a name is in the table twice");
    }
}
