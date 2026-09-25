//! `__builtin_shuffle`, which builds a vector out of the lanes of one or two others.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The call is `__builtin_shuffle(a, m)` or `__builtin_shuffle(a, b, m)`. Lane `i` of the answer
//! is lane `m[i]` of `a`, or of `a` and `b` laid end to end in the second form, and the answer
//! has the type `a` has. gcc takes each index modulo the number of lanes there are to pick from,
//! so only the low bits of a mask lane count, and that is done where the lanes are read rather
//! than here.
//!
//! It is answered rather than called because there is no function behind it. The answer is a
//! vector, and gcc expands it in every program on every target.
//!
//! # What gcc asks of the operands
//!
//! The rules are gcc's, checked in the order it checks them and with its words, so that a
//! program that gcc refuses is refused here for the same reason. The mask is a vector of
//! integers. The sources are vectors, and in the three operand form they have the same type. The
//! mask has as many lanes as a source, and its lanes are as wide as a source's, which is what
//! lets `__builtin_shuffle` of a `double` vector take a `long long` mask and not an `int` one.
//!
//! # What is not here
//!
//! `__builtin_shufflevector`, whose indices are constants written in the call and whose answer
//! may have a different number of lanes from its operands. gcc 12 took it from clang and it is
//! its own piece of work.

use rucc_ast as ast;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{compatible, is_integer, layout};

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The one name in the family.
const SHUFFLE: &str = "__builtin_shuffle";

/// Whether the roster has a row for this name, which is what the test next door asks.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    name == SHUFFLE
}

impl Checker<'_> {
    /// Answers a call to `__builtin_shuffle`, if the name is that.
    ///
    /// The caller has already made sure the program did not declare the name itself, in which
    /// case what it declared is what the call is checked against.
    pub(super) fn shuffle_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        (self.text(name) == SHUFFLE).then(|| self.shuffle_call(args, span))
    }

    /// The call itself, once the name has been recognised.
    fn shuffle_call(&mut self, args: ast::ExprList, span: Span) -> ExprId {
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
        if !matches!(args.len(), 2 | 3) {
            let how = if args.len() < 2 { "few" } else { "many" };
            self.report(
                Diagnostic::error(format!("too {how} arguments to function '{SHUFFLE}'"), span)
                    .with_code("E0511"),
            );
            return self.poison(span);
        }
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return self.poison(span);
        }
        let (sources, mask) = args.split_at(args.len() - 1);
        let mask = mask[0];
        let lhs = sources[0];
        let rhs = sources.get(1).copied();
        let indices = self.tast[mask].ty;
        // An array has an element too, which is why the vector is asked about on its own.
        let picks = rucc_types::element(&self.types, indices)
            .filter(|&lane| is_integer(&self.types, lane))
            .filter(|_| rucc_types::is_vector(&self.types, indices));
        let Some(picks) = picks else {
            return self.wrong_shuffle("last argument must be an integer vector", span);
        };
        if !sources.iter().all(|&source| rucc_types::is_vector(&self.types, self.tast[source].ty)) {
            return self.wrong_shuffle("arguments must be vectors", span);
        }
        let ty = self.types.unqualified(self.tast[lhs].ty);
        if let Some(rhs) = rhs {
            let other = self.types.unqualified(self.tast[rhs].ty);
            if !compatible(&self.types, ty, other) {
                return self.wrong_shuffle("argument vectors must be of the same type", span);
            }
        }
        if rucc_types::lanes(&self.types, ty) != rucc_types::lanes(&self.types, indices) {
            return self.wrong_shuffle(
                "number of elements of the argument vector(s) and the mask vector should be the \
                 same",
                span,
            );
        }
        let lane = rucc_types::element(&self.types, ty).expect("a vector");
        let width = |ty| layout(&self.types, ty, self.cx.target).map_or(0, |layout| layout.size);
        if width(lane) != width(picks) {
            return self.wrong_shuffle(
                "argument vector(s) inner type must have the same size as inner type of the mask",
                span,
            );
        }
        let node = ExprKind::Shuffle { lhs, rhs, mask };
        self.tast.expr(Expr::new(node, ty, Category::Rvalue), span)
    }

    /// One of gcc's messages about the operands, which all start with the name.
    fn wrong_shuffle(&mut self, what: &str, span: Span) -> ExprId {
        self.report(Diagnostic::error(format!("'{SHUFFLE}' {what}"), span).with_code("E0715"));
        self.poison(span)
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The name has to be a row of the roster, or it is a builtin `__has_builtin` has never heard
    /// of. A row with a signature would be called rather than answered, and there is nothing to
    /// call.
    #[test]
    fn the_name_is_a_row_of_the_table_and_is_answered_not_called() {
        let Some(feature) = rucc_gnu::lookup(Kind::Builtin, SHUFFLE) else {
            panic!("{SHUFFLE} is answered here and is not in features.toml");
        };
        assert_eq!(feature.status, Status::Implemented);
        assert!(feature.signature.is_empty(), "{SHUFFLE} is answered and not called");
    }
}
