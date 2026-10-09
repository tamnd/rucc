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
//! # `__builtin_shufflevector`
//!
//! The call is `__builtin_shufflevector(a, b, i0, i1, ...)`. Each index is an integer constant,
//! and lane `k` of the answer is lane `ik` of `a` and `b` laid end to end. An index of -1 is a lane
//! whose value is not specified. The answer is a vector of the lane type of `a` with one lane for
//! each index. gcc 12 took it from clang, and the rules and the words here are those of gcc 16:
//! the two operands are vectors with the same lane type, but they can have a different number of
//! lanes, and the number of indices is a power of two.
//!
//! # `__builtin_wasm_shuffle_i8x16`
//!
//! The wasm builtin of clang has a prototype and is a call, which the wasm back end writes as one
//! `i8x16.shuffle`. Its 16 lanes are immediates of the instruction, so each one is an integer
//! constant expression. clang folds each lane here, and so does rucc, so `(c) * 4 + 1` in the
//! macros of `<wasm_simd128.h>` is a constant at `-O0` too. A lane that is not a constant is an
//! error in the words of clang, at the start of the call, as in clang.
//!
//! gcc names an index that is wrong as it was written. Here an integer constant is named by its
//! value and a plain name by the name, as gcc does, and another expression is not named. gcc says
//! `wrong number of arguments to '__builtin_shuffle'` when there are fewer than three arguments,
//! and so does rucc.

use rucc_ast as ast;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{IntKind, compatible, is_integer, layout};

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::tast::Const;

/// The name with a mask that is a vector.
const SHUFFLE: &str = "__builtin_shuffle";

/// The name with indices that are constants.
const SHUFFLEVECTOR: &str = "__builtin_shufflevector";

/// The wasm builtin whose lanes are constants.
const WASM_SHUFFLE: &str = "__builtin_wasm_shuffle_i8x16";

/// Whether the roster has a row for this name, which is what the test next door asks.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    name == SHUFFLE || name == SHUFFLEVECTOR
}

impl Checker<'_> {
    /// Folds each lane of a call to `__builtin_wasm_shuffle_i8x16` to a constant, after the
    /// arguments were converted to the types of the prototype.
    pub(in crate::check) fn wasm_shuffle_lanes(
        &mut self,
        function: Option<Symbol>,
        args: &mut [ExprId],
        span: Span,
    ) {
        if function.is_none_or(|name| self.text(name) != WASM_SHUFFLE) {
            return;
        }
        let int = self.types.int(IntKind::Int);
        for arg in args.iter_mut().skip(2) {
            if self.is_poisoned(*arg) {
                continue;
            }
            match self.eval_integer(*arg) {
                Ok(lane) => *arg = self.constant(Const::Int(lane), int, self.tast.expr_span(*arg)),
                Err(_) => {
                    let what = format!("argument to '{WASM_SHUFFLE}' must be a constant integer");
                    self.report(Diagnostic::error(what, span).with_code("E0715"));
                }
            }
        }
    }

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
        match self.text(name) {
            SHUFFLE => Some(self.shuffle_call(args, span)),
            SHUFFLEVECTOR => Some(self.shufflevector_call(args, span)),
            _ => None,
        }
    }

    /// `__builtin_shufflevector(a, b, i0, i1, ...)`, checked in the order of gcc 16.
    fn shufflevector_call(&mut self, args: ast::ExprList, span: Span) -> ExprId {
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        let named: Vec<Option<Symbol>> = written
            .iter()
            .map(|&arg| match self.ast[arg] {
                ast::Expr::Name(name) => Some(name),
                _ => None,
            })
            .collect();
        let args: Vec<ExprId> = written
            .into_iter()
            .map(|arg| {
                let arg = self.expr(arg);
                self.value(arg)
            })
            .collect();
        if args.len() < 3 {
            self.report(
                Diagnostic::error(format!("wrong number of arguments to '{SHUFFLE}'"), span)
                    .with_code("E0511"),
            );
            return self.poison(span);
        }
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return self.poison(span);
        }
        let (lhs, rhs) = (args[0], args[1]);
        let (left, right) = (self.tast[lhs].ty, self.tast[rhs].ty);
        if !rucc_types::is_vector(&self.types, left) || !rucc_types::is_vector(&self.types, right) {
            return self.wrong_shufflevector("arguments must be vectors", span);
        }
        let lane = rucc_types::element(&self.types, left).expect("a vector");
        let other = rucc_types::element(&self.types, right).expect("a vector");
        let (a, b) = (self.types.unqualified(lane), self.types.unqualified(other));
        if !compatible(&self.types, a, b) {
            return self
                .wrong_shufflevector("argument vectors must have the same element type", span);
        }
        let count = args.len() - 2;
        if !count.is_power_of_two() {
            return self.wrong_shufflevector(
                "must specify a result with a power of two number of elements",
                span,
            );
        }
        let total = rucc_types::lanes(&self.types, left).unwrap_or(0)
            + rucc_types::lanes(&self.types, right).unwrap_or(0);
        let int = self.types.int(IntKind::Int);
        let mut picks = Vec::with_capacity(count);
        for (&arg, name) in args[2..].iter().zip(&named[2..]) {
            let value = self.eval_integer(arg).ok();
            match value {
                Some(index) if index == -1 || (0..i128::from(total)).contains(&index) => {
                    picks.push(self.constant(Const::Int(index), int, span));
                }
                _ => {
                    let spelled = match (value, name) {
                        (Some(index), _) => format!("'{index}' "),
                        (None, Some(name)) => format!("'{}' ", self.text(*name)),
                        (None, None) => String::new(),
                    };
                    let what = format!("invalid element index {spelled}to '{SHUFFLEVECTOR}'");
                    self.report(Diagnostic::error(what, span).with_code("E0715"));
                    return self.poison(span);
                }
            }
        }
        let operands = self.tast.add_expr_refs(&[&[lhs, rhs][..], &picks].concat());
        let ty = self.types.vector(lane, u32::try_from(count).unwrap_or(u32::MAX));
        let node = ExprKind::ShuffleVector { operands };
        self.tast.expr(Expr::new(node, ty, Category::Rvalue), span)
    }

    /// One of gcc's messages about the operands of `__builtin_shufflevector`, which all start
    /// with the name.
    fn wrong_shufflevector(&mut self, what: &str, span: Span) -> ExprId {
        self.report(
            Diagnostic::error(format!("'{SHUFFLEVECTOR}' {what}"), span).with_code("E0715"),
        );
        self.poison(span)
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
        for name in [SHUFFLE, SHUFFLEVECTOR] {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented);
            assert!(feature.signature.is_empty(), "{name} is answered and not called");
        }
    }
}
