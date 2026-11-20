//! The `__builtin_elementwise_*` builtins of clang that do integer operations on each lane.
//!
//! Design: `Spec/2131/platform/wasm/19-simd.md`, the section about the header.
//!
//! `__builtin_elementwise_min`, `max`, `add_sat`, `sub_sat` and `popcount` take an integer or a
//! vector of integers, and do the operation on each lane by itself. clang's `<wasm_simd128.h>`
//! writes `wasm_i8x16_min`, `wasm_u16x8_add_sat` and `wasm_i8x16_popcnt` with them, and other
//! SIMD libraries use them when `__has_builtin` says yes. gcc has none of them.
//!
//! # What clang asks of the operands
//!
//! The rules and the words are those of clang 23. Each operand is an integer or a vector of
//! integers, and the two operands have the same type. There is no promotion, so `char` lanes stay
//! `char` lanes, and the answer has the type of the operands. The signedness of the type says
//! whether a lane is read as signed or as unsigned.
//!
//! # What is not here
//!
//! Floats. clang 23 takes a float in `min` and `max` and says that the builtin is deprecated
//! there, and it has `minnum` and the others for floats. rucc refuses a float with the words that
//! clang uses for `add_sat`. The other `__builtin_elementwise_*` builtins are not here either.

use rucc_ast as ast;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{compatible, element, is_integer};

use crate::check::Checker;
use crate::expr::{Category, Elementwise, Expr, ExprId, ExprKind};

/// Every name in the family, with its operation and its number of operands.
const FAMILY: &[(&str, Elementwise, usize)] = &[
    ("__builtin_elementwise_min", Elementwise::Min, 2),
    ("__builtin_elementwise_max", Elementwise::Max, 2),
    ("__builtin_elementwise_add_sat", Elementwise::AddSat, 2),
    ("__builtin_elementwise_sub_sat", Elementwise::SubSat, 2),
    ("__builtin_elementwise_popcount", Elementwise::Popcount, 1),
];

/// Whether the roster has a row for this name, which is what the test next door asks.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    FAMILY.iter().any(|&(spelled, ..)| spelled == name)
}

impl Checker<'_> {
    /// Answers a call to one of the elementwise builtins, if the name is one.
    ///
    /// The caller has already made sure the program did not declare the name itself, in which
    /// case what it declared is what the call is checked against.
    pub(super) fn elementwise_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        let spelled = self.text(name);
        let &(_, op, wanted) = FAMILY.iter().find(|&&(row, ..)| row == spelled)?;
        Some(self.elementwise_call(op, wanted, args, span))
    }

    /// The call itself, once the name has been recognised.
    fn elementwise_call(
        &mut self,
        op: Elementwise,
        wanted: usize,
        args: ast::ExprList,
        span: Span,
    ) -> ExprId {
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
        if args.len() != wanted {
            let how = if args.len() < wanted { "few" } else { "many" };
            let have = args.len();
            self.report(
                Diagnostic::error(
                    format!("too {how} arguments to function call, expected {wanted}, have {have}"),
                    span,
                )
                .with_code("E0511"),
            );
            return self.poison(span);
        }
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return self.poison(span);
        }
        for (at, &arg) in args.iter().enumerate() {
            let ty = self.tast[arg].ty;
            let lane = element(&self.types, ty).unwrap_or(ty);
            if !is_integer(&self.types, lane) {
                let nth = if at == 0 { "1st" } else { "2nd" };
                let was = self.spell(ty);
                self.report(
                    Diagnostic::error(
                        format!(
                            "{nth} argument must be a scalar or vector of integer types (was \
                             '{was}')"
                        ),
                        self.tast.expr_span(arg),
                    )
                    .with_code("E0685"),
                );
                return self.poison(span);
            }
        }
        let ty = self.tast[args[0]].ty;
        if let Some(&rhs) = args.get(1) {
            let other = self.tast[rhs].ty;
            let left = self.types.unqualified(ty);
            let right = self.types.unqualified(other);
            if !compatible(&self.types, left, right) {
                let (a, b) = (self.spell(ty), self.spell(other));
                self.report(
                    Diagnostic::error(
                        format!("arguments are of different types ('{a}' vs '{b}')"),
                        self.tast.expr_span(args[0]),
                    )
                    .with_code("E0685"),
                );
                return self.poison(span);
            }
        }
        let ty = self.types.unqualified(ty);
        let kind = ExprKind::Elementwise { op, lhs: args[0], rhs: args.get(1).copied() };
        self.tast.expr(Expr::new(kind, ty, Category::Rvalue), span)
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every name here is a row of the roster with no signature, so that `__has_builtin` says
    /// yes and the call is answered here and not called.
    #[test]
    fn every_name_in_the_family_is_a_row_of_the_table_and_is_answered_not_called() {
        for &(name, ..) in FAMILY {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert!(feature.signature.is_empty(), "{name} is answered and not called");
        }
    }
}
