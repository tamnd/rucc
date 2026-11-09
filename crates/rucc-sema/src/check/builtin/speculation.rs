//! `__builtin_speculation_safe_value`, a value that a processor running ahead of a branch it has
//! not yet decided does not see.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! A bounds check is a branch, and a processor that guesses the branch goes the safe way can load
//! through an index that is out of bounds before it finds out it guessed wrong. The load leaves a
//! trace in the cache that a program can measure, and that is Spectre variant 1. gcc 9 added this
//! builtin to stop it: `__builtin_speculation_safe_value (i)` is `i`, and nothing after it runs
//! until the branches before it are decided. gcc says `__HAVE_SPECULATION_SAFE_VALUE` on the
//! targets that have a barrier for it, and a program tests the macro before it calls the builtin.
//!
//! # What gcc makes of it
//!
//! The value, and then the barrier, which is `lfence` on x86 and `isb` and `dsb sy` on AArch64.
//! The call becomes [`ExprKind::Speculation`], which the lowering turns into the same two. The
//! second argument is what gcc calls the fail value, the value a processor that guessed wrong is
//! to see. A barrier makes it see nothing at all, so gcc only works it out for what it does, and
//! so does this. A target with no barrier gets the value alone and a warning, in gcc's words.
//!
//! # What gcc asks of the operands
//!
//! The value is a pointer or an integer type of one, two, four, eight or sixteen bytes. `_Bool`,
//! an enumeration and a `_BitInt` are integers to C and are not to this builtin, and gcc refuses
//! them in other words than a type that is not an integer at all. The fail value has the type of
//! the value, where gcc's test is that a conversion between the two would change nothing: two
//! pointers, or two integers of the same width and signedness. `0` with a `long` value is
//! refused for that reason. The messages are gcc 15's.

use rucc_ast as ast;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{TypeId, TypeKind, integer_info, is_pointer, layout};

use crate::check::Checker;
use crate::eval::bare;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The name, which is a row of `features.toml` with no signature because its type is the type of
/// what it is handed.
const NAME: &str = "__builtin_speculation_safe_value";

/// The code a call gcc refuses is refused under.
const CODE: &str = "E0874";

/// The code of the warning a target with no barrier gets.
const NO_BARRIER: &str = "W0338";

/// Whether this is the name, which is what the test in `generic.rs` asks of every family whose
/// rows have no signature.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    name == NAME
}

/// Whether gcc has a speculation barrier for this machine, spelled as the target tuple spells it.
/// That is when gcc says `__HAVE_SPECULATION_SAFE_VALUE`, and when the builtin is more than its
/// value. rucc-pp's list of the same machines is the one the macro follows.
fn has_barrier(arch: &str) -> bool {
    matches!(arch, "x86_64" | "i686" | "aarch64" | "arm" | "powerpc64")
}

impl Checker<'_> {
    /// Answers a call to `__builtin_speculation_safe_value`, if that is the name.
    ///
    /// Answers nothing for every other name. The caller has already made sure the program did not
    /// declare the name itself.
    pub(super) fn speculation_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        if self.text(name) != NAME {
            return None;
        }
        Some(self.speculation_call(args, span))
    }

    /// The call itself, once the name has been recognised.
    fn speculation_call(&mut self, args: ast::ExprList, span: Span) -> ExprId {
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        let args: Vec<ExprId> = written
            .into_iter()
            .map(|arg| {
                let arg = self.expr(arg);
                self.value(arg)
            })
            .collect();
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return self.poison(span);
        }
        let (value, fail) = match args[..] {
            [] => {
                let what = format!("too few arguments to function '{NAME}'");
                self.report(Diagnostic::error(what, span).with_code("E0511"));
                return self.poison(span);
            }
            [value] => (value, None),
            [value, fail] => (value, Some(fail)),
            _ => {
                let what = format!("too many arguments to function '{NAME}'");
                self.report(Diagnostic::error(what, span).with_code("E0511"));
                return self.poison(span);
            }
        };
        let ty = self.tast[value].ty;
        if let Some(what) = self.speculation_refusal(ty) {
            self.report(Diagnostic::error(what, span).with_code(CODE));
            return self.poison(span);
        }
        if let Some(fail) = fail {
            let other = self.tast[fail].ty;
            if !self.no_conversion(ty, other) {
                let what = "both arguments must be compatible";
                self.report(Diagnostic::error(what, span).with_code(CODE));
                return self.poison(span);
            }
        }
        let value = if has_barrier(self.cx.target.tuple.arch().as_str()) {
            self.tast.expr(Expr::new(ExprKind::Speculation { value }, ty, Category::Rvalue), span)
        } else {
            // The value as it is, which is what gcc answers with there.
            let what = "this target does not define a speculation barrier; your program will \
                        still execute correctly, but incorrect speculation may not be prevented";
            self.report(Diagnostic::warning(what, span).with_code(NO_BARRIER));
            value
        };
        // The fail value is worked out first, for what it does, and its value is dropped.
        match fail {
            Some(fail) => {
                let comma = ExprKind::Comma { lhs: fail, rhs: value };
                self.tast.expr(Expr::new(comma, ty, Category::Rvalue), span)
            }
            None => value,
        }
    }

    /// What gcc says about a value of this type, or nothing when it takes one.
    fn speculation_refusal(&self, ty: TypeId) -> Option<String> {
        if is_pointer(&self.types, ty) {
            return None;
        }
        let size = layout(&self.types, ty, self.cx.target).ok().map(|it| it.size);
        let sized = matches!(size, Some(1 | 2 | 4 | 8 | 16));
        let integral = integer_info(&self.types, ty, self.cx.target).is_some();
        if !integral || !sized {
            let shown = self.spell(ty);
            return Some(format!(
                "operand type '{shown}' is incompatible with argument 1 of '{NAME}'"
            ));
        }
        // An integer to C that is not one of gcc's integer types.
        if !matches!(bare(&self.types, ty), TypeKind::Int(_)) {
            return Some(
                "expecting argument of type pointer or of type integer for argument 1".to_owned(),
            );
        }
        None
    }

    /// Whether a conversion from `from` to `to` would change nothing, which is gcc's test of the
    /// fail value: two pointers, or two integers of the same width and signedness.
    fn no_conversion(&self, to: TypeId, from: TypeId) -> bool {
        if is_pointer(&self.types, to) || is_pointer(&self.types, from) {
            return is_pointer(&self.types, to) && is_pointer(&self.types, from);
        }
        let target = self.cx.target;
        match (integer_info(&self.types, to, target), integer_info(&self.types, from, target)) {
            (Some(to), Some(from)) => to.signed == from.signed && to.width == from.width,
            _ => false,
        }
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

    /// gcc has a barrier on x86, Arm and POWER, and none on RISC-V, s390x or WebAssembly.
    #[test]
    fn the_barrier_is_on_the_machines_gcc_has_one_for() {
        for arch in ["x86_64", "i686", "aarch64", "arm", "powerpc64"] {
            assert!(has_barrier(arch), "{arch}");
        }
        for arch in ["riscv64", "riscv32", "s390x", "wasm32", "loongarch64"] {
            assert!(!has_barrier(arch), "{arch}");
        }
    }
}
