//! `__builtin_apply_args` and `__builtin_apply`, which pass the arguments a function was called
//! with on to another function without knowing what they are.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The first answers the address of a block holding every argument the function it is in was
//! called with: every register an argument can arrive in, as it was on the way in, and where the
//! arguments that came in memory are. The second calls a function with what is in such a block, and
//! answers the address of a block holding every register a value can come back in. Neither knows
//! the types of the arguments, which is the point of them: a function that forwards a call it
//! cannot see the prototype of writes the pair.
//!
//! Both are answered rather than called because there is nothing to call. What the first reads is
//! the registers at the top of the function, which is a thing only the back end can write, and the
//! second is a call whose arguments are a block rather than values.
//!
//! # Why the size has to be a constant
//!
//! The third argument of `__builtin_apply` is how many bytes of the arguments that came in memory
//! go with the call. They are copied into the area at the bottom of the frame that every call
//! passes its arguments in memory through, and how big that area is has to be known when the frame
//! is laid out. gcc takes a size that is only known when the program runs and makes room for it by
//! moving the stack pointer, which is a thing no program writes: the size is always the most bytes
//! the functions it forwards to could want, written as a number.
//!
//! # What is not here
//!
//! `__builtin_return`, which gives back what a `__builtin_apply` came back with as the answer of the
//! function it is in, whatever that function was declared to return.

use rucc_ast as ast;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{is_function, is_integer, is_pointer, pointee};

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The one that saves the arguments.
const APPLY_ARGS: &str = "__builtin_apply_args";

/// The one that passes them on.
const APPLY: &str = "__builtin_apply";

/// The code the operands of `__builtin_apply` are refused under.
const CODE: &str = "E0716";

/// The most bytes of arguments in memory a call can pass on, which is far more than any function
/// takes and small enough that the area they are copied to is an offset in a frame.
const LARGEST: i128 = 1 << 16;

/// Whether the roster has a row for this name, which is what the test next door asks.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    name == APPLY_ARGS || name == APPLY
}

impl Checker<'_> {
    /// Answers a call to one of the two, if the name is one of them.
    ///
    /// The caller has already made sure the program did not declare the name itself, in which
    /// case what it declared is what the call is checked against.
    pub(super) fn apply_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        let spelled = self.text(name);
        if spelled != APPLY_ARGS && spelled != APPLY {
            return None;
        }
        let saves = spelled == APPLY_ARGS;
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
        let (name, wants) = if saves { (APPLY_ARGS, 0) } else { (APPLY, 3) };
        if args.len() != wants {
            let how = if args.len() < wants { "few" } else { "many" };
            self.report(
                Diagnostic::error(format!("too {how} arguments to function '{name}'"), span)
                    .with_code("E0511"),
            );
            return Some(self.poison(span));
        }
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return Some(self.poison(span));
        }
        let void = self.types.void();
        let ty = self.types.pointer(void);
        if saves {
            let node = ExprKind::ApplyArgs;
            return Some(self.tast.expr(Expr::new(node, ty, Category::Rvalue), span));
        }
        let (function, block, size) = (args[0], args[1], args[2]);
        let called = self.tast[function].ty;
        let calls = pointee(&self.types, called).is_some_and(|to| is_function(&self.types, to));
        if !calls {
            return Some(self.wrong_apply(1, "the address of a function", function));
        }
        if !is_pointer(&self.types, self.tast[block].ty) {
            return Some(self.wrong_apply(2, "the block `__builtin_apply_args` answered", block));
        }
        if !is_integer(&self.types, self.tast[size].ty) {
            return Some(self.wrong_apply(3, "a number of bytes", size));
        }
        let Some(size) = self.apply_size(size) else {
            return Some(self.poison(span));
        };
        let node = ExprKind::Apply { function, args: block, size };
        Some(self.tast.expr(Expr::new(node, ty, Category::Rvalue), span))
    }

    /// An operand of `__builtin_apply` that is not what that place takes.
    fn wrong_apply(&mut self, at: usize, what: &str, arg: ExprId) -> ExprId {
        let span = self.tast.expr_span(arg);
        let message = format!("argument {at} of '{APPLY}' is {what}");
        self.report(Diagnostic::error(message, span).with_code(CODE));
        self.poison(span)
    }

    /// How many bytes of arguments in memory the call passes on, as a number the frame can be laid
    /// out with, or nothing for one that has been reported.
    fn apply_size(&mut self, arg: ExprId) -> Option<u32> {
        let span = self.tast.expr_span(arg);
        let Ok(value) = self.eval().integer(arg) else {
            let what = format!("the size given to '{APPLY}' is a constant");
            let note = "the arguments are copied into the area at the bottom of the frame, and \
                        how big that is has to be known when the frame is laid out";
            self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
            return None;
        };
        if !(0..=LARGEST).contains(&value) {
            let what = format!("the size given to '{APPLY}' is from 0 to {LARGEST}");
            self.report(Diagnostic::error(what, span).with_code(CODE));
            return None;
        }
        u32::try_from(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Both names have to be rows of the roster, or they are builtins `__has_builtin` has never
    /// heard of. A row with a signature would be called rather than answered, and there is nothing
    /// to call.
    #[test]
    fn both_names_are_rows_of_the_table_and_are_answered_not_called() {
        for name in [APPLY_ARGS, APPLY] {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert!(feature.signature.is_empty(), "{name} is answered and not called");
        }
    }
}
