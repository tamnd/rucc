//! `__builtin_tgmath`, the call each macro of glibc's `<tgmath.h>` is made of.
//!
//! glibc writes `sqrt(x)` as `__builtin_tgmath(sqrtf, sqrt, sqrtl, csqrtf, csqrt, csqrtl, x)`
//! whenever the compiler says it is gcc 8 or later, which is every compiler that says it is gcc.
//! The builtin picks one of the functions by the type of the arguments and calls it, so each
//! macro argument is written once. Without it, `#include <tgmath.h>` stops at the first macro with
//! an implicit declaration.
//!
//! # The rule
//!
//! This is gcc's rule, from `c_parser_postfix_expression`, with gcc 13's C2X semantics, which is
//! what glibc expects of a compiler that says it is gcc 13 or later.
//!
//! - The first function's parameter count says how many of the arguments are arguments. The rest,
//!   at the front, are the functions, and there are at least two of them.
//! - Each function is prototyped, not variadic, and has that many parameters.
//! - A position (the return or a parameter) varies if the functions do not all have the same type
//!   there. A position that varies has a real or complex floating type in each function.
//! - The first parameter that varies, and that varies to complex types if any position does,
//!   gives each function its type T. No two functions have the same T.
//! - At each position that varies, the type is T where the position goes complex, and the real
//!   type of T where it does not.
//! - The arguments at the positions that vary give a common type U by the usual arithmetic
//!   conversions. An integer counts as `double`, or as `_Float32x` when one of those arguments has
//!   a `_FloatNx` type, or as `_Decimal64` when the arguments or the functions are decimal. U is
//!   complex if an argument is complex or every T is.
//! - The call goes to the function whose T is U.
//! - If no function has that T and the return type is the same real floating type in each, the
//!   macro is one of the narrowing ones, and the call goes to the first function whose T has at
//!   least the range and the precision of U.
//!
//! The messages are gcc's, word for word.

use rucc_ast as ast;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{FloatKind, FunctionType, TypeId, TypeKind, float_format, pointee};

use crate::check::Checker;
use crate::eval::bare;
use crate::expr::ExprId;

/// The name, which is a row of `features.toml` with no signature because no one prototype fits.
const NAME: &str = "__builtin_tgmath";

/// Whether this is the name, which is what the test in `generic.rs` asks of every family whose
/// rows have no signature.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    name == NAME
}

/// A type in a function's signature, as the rule compares them.
///
/// The floating types are by kind, since a qualifier or a typedef does not make a new one. Every
/// other type is its unqualified canonical form, which only has to compare equal to itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Real(FloatKind),
    Complex(FloatKind),
    Other(TypeId),
}

impl Shape {
    /// The real type a complex one is made of, and the type itself for every other.
    fn real(self) -> Shape {
        match self {
            Shape::Complex(kind) => Shape::Real(kind),
            other => other,
        }
    }

    /// The kind of a real or complex floating type.
    fn kind(self) -> Option<FloatKind> {
        match self {
            Shape::Real(kind) | Shape::Complex(kind) => Some(kind),
            Shape::Other(_) => None,
        }
    }
}

/// How the type at one position changes from one function to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Varies {
    /// The same type in each function.
    Not,
    /// A different real floating type in each.
    Real,
    /// A different floating type in each, and some of them complex.
    Complex,
}

/// What an argument at a position that varies is, as far as the common type goes.
#[derive(Debug, Clone, Copy)]
enum Given {
    /// An integer, which counts as a floating type the functions decide. True for `_Complex int`.
    Integer(bool),
    /// A real or a complex floating type.
    Float(Shape),
}

impl Checker<'_> {
    /// Answers a call to `__builtin_tgmath`, if that is the name.
    ///
    /// Answers nothing for every other name. The caller has already made sure the program did not
    /// declare the name itself.
    pub(super) fn tgmath_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        if self.text(name) != NAME {
            return None;
        }
        Some(self.tgmath_call(args, span))
    }

    /// The call itself, once the name has been recognised.
    fn tgmath_call(&mut self, args: ast::ExprList, span: Span) -> ExprId {
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        // The name a function was written with, which the call to it keeps. A call to `sqrtf`
        // made here has to be the same call as `sqrtf(x)` written out, and some library calls
        // are only answered inline when the name says which function it is.
        let names: Vec<Option<Symbol>> = written
            .iter()
            .map(|&arg| match self.ast[arg] {
                ast::Expr::Name(name) => Some(name),
                _ => None,
            })
            .collect();
        let checked: Vec<ExprId> = written
            .into_iter()
            .map(|arg| {
                let arg = self.expr(arg);
                self.value(arg)
            })
            .collect();
        if checked.len() < 3 {
            self.refuse_tgmath(format!("too few arguments to '{NAME}'"), "E0511", span);
            return self.poison(span);
        }
        if checked.iter().any(|&arg| self.is_poisoned(arg)) {
            return self.poison(span);
        }
        let Some(first) = self.tgmath_function(checked[0], 1) else { return self.poison(span) };
        let count = first.params.len();
        if checked.len() < count + 2 {
            self.refuse_tgmath(format!("too few arguments to '{NAME}'"), "E0511", span);
            return self.poison(span);
        }
        let functions = checked.len() - count;
        let mut signatures = vec![first];
        for (index, &function) in checked.iter().enumerate().take(functions).skip(1) {
            let Some(signature) = self.tgmath_function(function, index + 1) else {
                return self.poison(span);
            };
            if signature.params.len() != count {
                let what =
                    format!("argument {} of '{NAME}' has wrong number of arguments", index + 1);
                self.refuse_tgmath(what, "E0511", span);
                return self.poison(span);
            }
            signatures.push(signature);
        }
        // Each function as the shapes of its return type and then of its parameters.
        let shapes: Vec<Vec<Shape>> = signatures
            .iter()
            .map(|signature| {
                std::iter::once(signature.ret)
                    .chain(signature.params.iter().copied())
                    .map(|ty| self.shape(ty))
                    .collect()
            })
            .collect();
        let Some(varies) = self.tgmath_variation(&shapes, span) else {
            return self.poison(span);
        };
        let Some(generic) = self.tgmath_generic(&shapes, &varies, span) else {
            return self.poison(span);
        };
        let given = &checked[functions..];
        let Some(wanted) = self.tgmath_common(&varies, &generic, given, span) else {
            return self.poison(span);
        };
        let Some(chosen) = self.tgmath_choose(&shapes, &generic, wanted) else {
            let what = "no matching function for type-generic call".to_owned();
            self.refuse_tgmath(what, "E0685", span);
            return self.poison(span);
        };
        self.finish_call(checked[chosen], names[chosen], given.to_vec(), span)
    }

    /// The signature of argument `position`, which has to be a prototyped function with a fixed
    /// number of parameters and at least one, having said so if it is not.
    fn tgmath_function(&mut self, function: ExprId, position: usize) -> Option<FunctionType> {
        let signature = pointee(&self.types, self.tast[function].ty)
            .map(|target| self.types.canonical(target))
            .and_then(|target| match self.types.kind(target) {
                TypeKind::Function(id) => Some(self.types.signature(id).clone()),
                _ => None,
            });
        let at = self.tast.expr_span(function);
        let what = match &signature {
            None => "is not a function pointer",
            Some(signature) if !signature.prototyped => "is unprototyped",
            Some(signature) if signature.variadic => "has variable arguments",
            Some(signature) if signature.params.is_empty() => "has no arguments",
            Some(_) => return signature,
        };
        self.refuse_tgmath(format!("argument {position} of '{NAME}' {what}"), "E0685", at);
        None
    }

    /// How each position varies, the return first, having said so if one varies in a way no
    /// set of type generic functions does or if none varies at all.
    fn tgmath_variation(&mut self, shapes: &[Vec<Shape>], span: Span) -> Option<Vec<Varies>> {
        let mut varies = Vec::with_capacity(shapes[0].len());
        for position in 0..shapes[0].len() {
            let first = shapes[0][position];
            if shapes.iter().all(|shape| shape[position] == first) {
                varies.push(Varies::Not);
                continue;
            }
            if let Some(function) = shapes.iter().position(|shape| shape[position].kind().is_none())
            {
                let what = if position == 0 {
                    format!(
                        "invalid type-generic return type for argument {} of '{NAME}'",
                        function + 1
                    )
                } else {
                    format!(
                        "invalid type-generic type for argument {position} of argument {} of '{NAME}'",
                        function + 1
                    )
                };
                self.refuse_tgmath(what, "E0685", span);
                return None;
            }
            let complex = shapes.iter().any(|shape| matches!(shape[position], Shape::Complex(_)));
            varies.push(if complex { Varies::Complex } else { Varies::Real });
        }
        let most = varies.iter().copied().max().unwrap_or(Varies::Not);
        if most == Varies::Not {
            let what = format!("function arguments of '{NAME}' all have the same type");
            self.refuse_tgmath(what, "E0685", span);
            return None;
        }
        Some(varies)
    }

    /// The type T of each function, read off the first parameter that varies the most, having
    /// said so if the functions do not vary together the way the rule needs.
    fn tgmath_generic(
        &mut self,
        shapes: &[Vec<Shape>],
        varies: &[Varies],
        span: Span,
    ) -> Option<Vec<Shape>> {
        let most = varies.iter().copied().max().unwrap_or(Varies::Not);
        let Some(position) = (1..varies.len()).find(|&position| varies[position] == most) else {
            let what = format!("function arguments of '{NAME}' lack type-generic parameter");
            self.refuse_tgmath(what, "E0685", span);
            return None;
        };
        let generic: Vec<Shape> = shapes.iter().map(|shape| shape[position]).collect();
        for (function, &shape) in generic.iter().enumerate() {
            if generic[..function].contains(&shape) {
                let what = format!(
                    "duplicate type-generic parameter type for function argument {} of '{NAME}'",
                    function + 1
                );
                self.refuse_tgmath(what, "E0685", span);
                return None;
            }
        }
        for (function, shape) in shapes.iter().enumerate() {
            let t = generic[function];
            for (position, &at) in shape.iter().enumerate() {
                let fits = match varies[position] {
                    Varies::Not => true,
                    Varies::Real => at == t.real(),
                    Varies::Complex => at == t,
                };
                if fits {
                    continue;
                }
                let what = if position == 0 {
                    format!("bad return type for function argument {} of '{NAME}'", function + 1)
                } else {
                    format!(
                        "bad type for argument {position} of function argument {} of '{NAME}'",
                        function + 1
                    )
                };
                self.refuse_tgmath(what, "E0685", span);
                return None;
            }
        }
        Some(generic)
    }

    /// The common type U of the arguments at the positions that vary, having said so if one of
    /// them is not something the functions can take.
    fn tgmath_common(
        &mut self,
        varies: &[Varies],
        generic: &[Shape],
        given: &[ExprId],
        span: Span,
    ) -> Option<Shape> {
        let decimal = |shape: &Shape| shape.kind().is_some_and(FloatKind::is_decimal);
        let all_complex = generic.iter().all(|shape| matches!(shape, Shape::Complex(_)));
        let all_decimal = generic.iter().all(decimal);
        let all_binary = !generic.iter().any(decimal);
        let mut complex = all_complex;
        let mut binary = false;
        let mut int_decimal = all_decimal;
        let mut int_floatnx = false;
        let mut kinds = Vec::new();
        for (index, &arg) in given.iter().enumerate() {
            let position = index + 1;
            if varies[position] == Varies::Not {
                continue;
            }
            let Some(arg) = self.tgmath_given(arg) else {
                let what = format!("invalid type of argument {position} of type-generic function");
                self.refuse_tgmath(what, "E0685", span);
                return None;
            };
            let refused = match arg {
                Given::Float(Shape::Real(kind)) if kind.is_decimal() => {
                    int_decimal = true;
                    if all_complex {
                        Some(format!(
                            "decimal floating-point argument {position} to complex-only type-generic function"
                        ))
                    } else if all_binary {
                        Some(format!(
                            "decimal floating-point argument {position} to binary-only type-generic function"
                        ))
                    } else if complex {
                        Some(both("complex"))
                    } else if binary {
                        Some(both("binary"))
                    } else {
                        None
                    }
                }
                Given::Float(Shape::Complex(_)) | Given::Integer(true) => {
                    complex = true;
                    if matches!(arg, Given::Float(_)) {
                        binary = true;
                    }
                    if all_decimal {
                        Some(format!(
                            "complex argument {position} to decimal-only type-generic function"
                        ))
                    } else if int_decimal {
                        Some(both("complex"))
                    } else {
                        None
                    }
                }
                Given::Float(_) => {
                    binary = true;
                    if all_decimal {
                        Some(format!(
                            "binary argument {position} to decimal-only type-generic function"
                        ))
                    } else if int_decimal {
                        Some(both("binary"))
                    } else {
                        None
                    }
                }
                Given::Integer(false) => None,
            };
            if let Some(what) = refused {
                self.refuse_tgmath(what, "E0685", span);
                return None;
            }
            if let Given::Float(shape) = arg {
                if matches!(shape.kind(), Some(FloatKind::Float32x | FloatKind::Float64x)) {
                    int_floatnx = true;
                }
            }
            kinds.push(arg);
        }
        // Only now, because whether an integer is a `double` depends on every other argument.
        let integer = if int_floatnx {
            FloatKind::Float32x
        } else if int_decimal {
            FloatKind::Decimal64
        } else {
            FloatKind::Double
        };
        let mut common: Option<TypeId> = None;
        for arg in kinds {
            let kind = match arg {
                Given::Integer(_) => integer,
                Given::Float(shape) => shape.kind()?,
            };
            let ty = self.types.float(kind);
            common = match common {
                None => Some(ty),
                Some(so_far) => {
                    rucc_types::usual_arithmetic(&mut self.types, so_far, ty, self.cx.target)
                }
            };
        }
        let TypeKind::Float(kind) = bare(&self.types, common?) else { return None };
        Some(if complex { Shape::Complex(kind) } else { Shape::Real(kind) })
    }

    /// The function the call goes to: the one whose T is U, or for a narrowing macro the first
    /// whose T holds every value of U.
    fn tgmath_choose(
        &self,
        shapes: &[Vec<Shape>],
        generic: &[Shape],
        wanted: Shape,
    ) -> Option<usize> {
        if let Some(exact) = generic.iter().position(|&shape| shape == wanted) {
            return Some(exact);
        }
        // A narrowing macro, such as `fadd`, returns the same type whichever function it calls.
        let ret = shapes[0][0];
        if !matches!(ret, Shape::Real(_)) || shapes.iter().any(|shape| shape[0] != ret) {
            return None;
        }
        let want = wanted.kind()?;
        generic.iter().position(|&shape| {
            let same_family = matches!(
                (shape, wanted),
                (Shape::Real(_), Shape::Real(_)) | (Shape::Complex(_), Shape::Complex(_))
            );
            same_family && shape.kind().is_some_and(|have| self.holds(have, want))
        })
    }

    /// Whether every value of `want` is a value of `have`, which is gcc's comparison of the two
    /// formats: the same radix, and at least the precision, the largest exponent and the
    /// smallest subnormal.
    fn holds(&self, have: FloatKind, want: FloatKind) -> bool {
        if have.is_decimal() != want.is_decimal() {
            return false;
        }
        let have = float_format(have, self.cx.target);
        let want = float_format(want, self.cx.target);
        if have == want {
            return true;
        }
        // The decimal formats each hold the narrower ones, and the double-double has no exponent
        // range to compare, so it only holds itself.
        if have.decimal().is_some() && want.decimal().is_some() {
            return have.width() >= want.width();
        }
        if !have.is_ieee() || !want.is_ieee() {
            return false;
        }
        let (hp, wp) = (i64::from(have.precision()), i64::from(want.precision()));
        hp >= wp
            && have.max_exponent() >= want.max_exponent()
            && i64::from(want.min_exponent()) - wp >= i64::from(have.min_exponent()) - hp
    }

    /// What an argument is to the common type, or nothing if it is not arithmetic.
    fn tgmath_given(&self, arg: ExprId) -> Option<Given> {
        let ty = self.tast[arg].ty;
        match bare(&self.types, ty) {
            TypeKind::Float(kind) => Some(Given::Float(Shape::Real(kind))),
            TypeKind::Complex(part) => match bare(&self.types, part) {
                TypeKind::Float(kind) => Some(Given::Float(Shape::Complex(kind))),
                _ if rucc_types::is_integer(&self.types, part) => Some(Given::Integer(true)),
                _ => None,
            },
            _ if rucc_types::is_integer(&self.types, ty) => Some(Given::Integer(false)),
            _ => None,
        }
    }

    /// A type in a signature as the rule compares it.
    fn shape(&mut self, ty: TypeId) -> Shape {
        match bare(&self.types, ty) {
            TypeKind::Float(kind) => Shape::Real(kind),
            TypeKind::Complex(part) => match bare(&self.types, part) {
                TypeKind::Float(kind) => Shape::Complex(kind),
                _ => self.other(ty),
            },
            _ => self.other(ty),
        }
    }

    /// A type that is not floating, as the rule compares it.
    fn other(&mut self, ty: TypeId) -> Shape {
        let canonical = self.types.canonical(ty);
        Shape::Other(self.types.unqualified(canonical))
    }

    /// Reports an error in gcc's words.
    fn refuse_tgmath(&mut self, what: String, code: &'static str, span: Span) {
        self.report(Diagnostic::error(what, span).with_code(code));
    }
}

/// The message for a decimal argument beside a complex or a binary one, which gcc words the same
/// way whichever came first.
fn both(other: &str) -> String {
    format!("both {other} and decimal floating-point arguments to type-generic function")
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The name has to be a row of the roster, or `__has_builtin` says no. A row with a signature
    /// would be declared and called, and there is no function anywhere to call.
    #[test]
    fn tgmath_is_a_row_of_the_table_and_is_answered_not_called() {
        let Some(feature) = rucc_gnu::lookup(Kind::Builtin, NAME) else {
            panic!("{NAME} is answered here and is not in features.toml");
        };
        assert_eq!(feature.status, Status::Implemented);
        assert!(feature.signature.is_empty(), "{NAME} is answered and not called");
    }
}
