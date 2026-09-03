//! The floating point classification builtins, which are questions rather than calls.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! `isnan`, `isgreater`, `signbit` and the rest of the family are macros in `math.h` that expand
//! to exactly these builtins, so there is no function of any of those names for a call to reach.
//! A compiler that lowered `__builtin_isnan` to a call to `isnan` would be emitting a reference
//! to something no library defines. What each one is instead is a comparison, which is why the
//! whole family is answered here and none of it reaches the IR as a call.
//!
//! # Why they are type generic
//!
//! `isgreater(a, b)` promotes its two operands against each other the way the standard's macro
//! does, so a `float` against a `long double` is compared as a `long double` and the answer is
//! an `int` either way. There is no one signature for that, so these have no `signature` in
//! `features.toml` and are recognised before the callee is looked up, the same as the atomics in
//! the file next door.
//!
//! The names ending in `f` and `l` are not type generic. gcc gives `__builtin_isinff` a `float`
//! parameter and `__builtin_isinfl` a `long double` one, and the difference is visible:
//! `__builtin_isinff(1e300)` is one, because the argument is converted first and the value does
//! not fit in a `float`, and `__builtin_isinf(1e300)` is zero.
//!
//! # What is a comparison and what is not
//!
//! Four of the family are operators C already has. `isgreater` is `>`, `isgreaterequal` is `>=`,
//! `isless` is `<` and `islessequal` is `<=`, and each becomes the ordinary comparison node. The
//! difference the standard draws between them and the operators is that the macro does not raise
//! the invalid operation exception on a quiet NaN, and this compiler does not model floating
//! point exceptions at all, so there is nothing left for a separate node to carry.
//!
//! The other two of the pairs do not have an operator. `isunordered` is true when either operand
//! is a NaN, and `islessgreater` is `a < b || a > b`, which is not `a != b` because that is true
//! of a NaN. Neither can be written as the operators without naming an operand twice, and naming
//! it twice would evaluate it twice, which is wrong for `isunordered(f(), g())`. So those two and
//! the four that ask about one value are [`ExprKind::Classify`], a node of their own.

use rucc_ast as ast;
use rucc_ast::BinaryOp;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{FloatKind, TypeId, is_real_floating};

use crate::check::Checker;
use crate::expr::{Category, Classify, Expr, ExprId, ExprKind};

/// What one name of the family asks, and of what.
#[derive(Debug, Clone, Copy)]
struct Question {
    /// The name, spelled the way the program writes it.
    name: &'static str,
    /// What it asks.
    asks: Asks,
    /// The type the argument is converted to first, and nothing for the type generic spellings,
    /// which ask at whatever precision they were handed.
    at: Option<FloatKind>,
}

/// What one of them turns into.
#[derive(Debug, Clone, Copy)]
enum Asks {
    /// An operator C already has, which is what the node becomes.
    Operator(BinaryOp),
    /// One of the questions C has no operator for.
    Node(Classify),
}

impl Asks {
    /// Whether the question is about a pair of values rather than about one.
    const fn is_pair(self) -> bool {
        match self {
            Asks::Operator(_) => true,
            Asks::Node(op) => op.is_pair(),
        }
    }
}

/// A type generic row, which is every question about a pair and the unsuffixed spelling of every
/// question about one value.
const fn any(name: &'static str, asks: Asks) -> Question {
    Question { name, asks, at: None }
}

/// A row whose name ends in a width, which converts its argument to that width first.
const fn at(name: &'static str, asks: Asks, at: FloatKind) -> Question {
    Question { name, asks, at: Some(at) }
}

/// Every name in the family.
///
/// The names are also rows of `features.toml`, which is the roster, and the test at the bottom of
/// the file next door is what keeps the two from drifting.
const FAMILY: &[Question] = &[
    any("__builtin_isgreater", Asks::Operator(BinaryOp::Gt)),
    any("__builtin_isgreaterequal", Asks::Operator(BinaryOp::Ge)),
    any("__builtin_isless", Asks::Operator(BinaryOp::Lt)),
    any("__builtin_islessequal", Asks::Operator(BinaryOp::Le)),
    any("__builtin_islessgreater", Asks::Node(Classify::LessGreater)),
    any("__builtin_isunordered", Asks::Node(Classify::Unordered)),
    any("__builtin_isnan", Asks::Node(Classify::Nan)),
    at("__builtin_isnanf", Asks::Node(Classify::Nan), FloatKind::Float),
    at("__builtin_isnanl", Asks::Node(Classify::Nan), FloatKind::LongDouble),
    any("__builtin_isinf", Asks::Node(Classify::Infinite)),
    at("__builtin_isinff", Asks::Node(Classify::Infinite), FloatKind::Float),
    at("__builtin_isinfl", Asks::Node(Classify::Infinite), FloatKind::LongDouble),
    any("__builtin_isfinite", Asks::Node(Classify::Finite)),
    // The BSD spelling of the same question, which gcc has as well and which is not type generic
    // in any of its three forms.
    at("__builtin_finite", Asks::Node(Classify::Finite), FloatKind::Double),
    at("__builtin_finitef", Asks::Node(Classify::Finite), FloatKind::Float),
    at("__builtin_finitel", Asks::Node(Classify::Finite), FloatKind::LongDouble),
    any("__builtin_signbit", Asks::Node(Classify::SignBit)),
    at("__builtin_signbitf", Asks::Node(Classify::SignBit), FloatKind::Float),
    at("__builtin_signbitl", Asks::Node(Classify::SignBit), FloatKind::LongDouble),
];

/// Whether the roster has a row for this name, which is what the test next door asks.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    FAMILY.iter().any(|question| question.name == name)
}

impl Checker<'_> {
    /// Answers a call to one of the classification builtins, if the name is one.
    ///
    /// Answers nothing when the name is not in the table. The caller has already made sure the
    /// program did not declare the name itself, in which case what it declared is what the call
    /// is checked against and this has no business overriding it.
    pub(super) fn classify_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        let spelled = self.text(name);
        let question = *FAMILY.iter().find(|question| question.name == spelled)?;
        Some(self.classify_call(question, args, span))
    }

    /// The call itself, once the name has been recognised.
    fn classify_call(&mut self, question: Question, args: ast::ExprList, span: Span) -> ExprId {
        let spelled = question.name;
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        // Every argument is checked whatever else is wrong with the call, because a mistake
        // inside one of them is worth hearing about even when there are three of them.
        let args: Vec<ExprId> = written
            .into_iter()
            .map(|arg| {
                let arg = self.expr(arg);
                self.value(arg)
            })
            .collect();
        let wanted = if question.asks.is_pair() { 2 } else { 1 };
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
        // The argument the name names, before anything is asked about it, so that
        // `__builtin_isinff(1e300)` is one. Nothing is said when the conversion is not possible,
        // because the check below is what says it and says it in gcc's words.
        let converted: Vec<ExprId> = match question.at {
            None => args,
            Some(kind) => {
                let ty = self.types.float(kind);
                args.into_iter().map(|arg| self.convert_argument(arg, ty)).collect()
            }
        };
        if !converted.iter().all(|&arg| is_real_floating(&self.types, self.tast[arg].ty)) {
            // gcc's wording, which is plural for the questions about a pair however many of the
            // two are wrong.
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
        match question.asks {
            Asks::Operator(op) => self.comparison(op, converted[0], converted[1], span),
            Asks::Node(op) if op.is_pair() => {
                // Both sides in one type, which is what the standard's macro promises and what
                // the IR's comparison needs: there is no comparing an `f32` with an `f64`.
                let (lhs, rhs) = self
                    .conv()
                    .usual_arithmetic(converted[0], converted[1])
                    .expect("two floating point operands");
                self.classify_node(op, lhs, Some(rhs), span)
            }
            Asks::Node(op) => self.classify_node(op, converted[0], None, span),
        }
    }

    /// One argument converted to the type the name says, when the name says one.
    ///
    /// An argument that is not arithmetic at all stays as it was, so that what is reported about
    /// it is that it is not a floating point value rather than that it will not convert.
    fn convert_argument(&mut self, arg: ExprId, ty: TypeId) -> ExprId {
        if rucc_types::is_arithmetic(&self.types, self.tast[arg].ty) {
            return self.conv().to_type(arg, ty);
        }
        arg
    }

    /// The node itself, whose value is an `int` for every question in the family.
    fn classify_node(
        &mut self,
        op: Classify,
        lhs: ExprId,
        rhs: Option<ExprId>,
        span: Span,
    ) -> ExprId {
        let ty = self.int();
        self.tast.expr(Expr::new(ExprKind::Classify { op, lhs, rhs }, ty, Category::Rvalue), span)
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every name here has to be a row of the roster, or it is a builtin `__has_builtin` has
    /// never heard of and a header that asks takes its fallback path for no reason. The other
    /// half of the agreement, that no row without a signature is left with nothing to answer for
    /// it, is checked in the file next door.
    #[test]
    fn every_name_in_the_family_is_a_row_of_the_table_and_says_it_is_done() {
        for question in FAMILY {
            let feature = rucc_gnu::lookup(Kind::Builtin, question.name);
            let Some(feature) = feature else {
                panic!("{} is answered here and is not in features.toml", question.name);
            };
            assert_eq!(feature.status, Status::Implemented, "{}", question.name);
            assert!(feature.signature.is_empty(), "{} is answered and not called", question.name);
        }
    }

    /// Nothing that asks about a pair names a type, because gcc has no width bearing spelling of
    /// any of the six. A row that had one would convert both operands to it and then run the
    /// usual arithmetic conversions over two values that already agree, which is harmless and
    /// would still be a row saying something that is not so.
    #[test]
    fn nothing_that_asks_about_a_pair_names_a_type() {
        for question in FAMILY {
            assert!(!question.asks.is_pair() || question.at.is_none(), "{}", question.name);
        }
    }

    /// The names are what the lookup searches, so a name written twice is a second row nothing
    /// can ever reach.
    #[test]
    fn no_name_is_in_the_table_twice() {
        let mut names: Vec<&str> = FAMILY.iter().map(|question| question.name).collect();
        names.sort_unstable();
        let all = names.len();
        names.dedup();
        assert_eq!(names.len(), all, "a name is in the table twice");
    }
}
