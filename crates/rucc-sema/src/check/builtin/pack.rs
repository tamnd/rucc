//! `__builtin_va_arg_pack` and `__builtin_va_arg_pack_len`, which forward a function's anonymous
//! arguments to another variadic call.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! These are not functions and have no value. gcc accepts them only inside a function it is about
//! to inline, and what it does with them it does while inlining: the pack becomes the arguments
//! written at the call site and the length becomes how many of them there were. Neither means
//! anything in a function that was compiled on its own, and gcc refuses them there.
//!
//! This compiler does not inline, so the pack cannot be filled in and the length cannot be counted.
//! What it can do is accept them in the one place where nothing has to be filled in, which is a
//! variadic definition that nothing is emitted for. GNU's `extern inline` gives exactly that: the
//! definition is read, checked and then dropped, and the calls in it never reach the IR, so a call
//! to a builtin with no meaning is a call nothing ever has to give a meaning to. That is also the
//! only place either of them is written in practice, since every use in glibc is in a wrapper
//! declared `__extern_always_inline`.
//!
//! So the rule here is the pairing rather than either half: variadic, and not emitted. Anywhere
//! else the call is refused, because anywhere else it would reach the IR as a call to a name no
//! object file defines, and a link error naming a builtin is a worse way to learn this than a
//! diagnostic at the line that wrote it.
//!
//! # What this costs
//!
//! The fortification, and nothing else. `bits/stdio2.h` defines `sprintf` as a wrapper that calls
//! `__builtin___sprintf_chk` with the pack on the end, and a program compiled here calls the
//! library's own `sprintf` instead, because the wrapper is an inline definition and this compiler
//! emits none. That is already what happens to every other wrapper in those headers: the fortified
//! `memcpy` in `bits/string_fortified.h` needs no pack, compiles today, and still ends up as a call
//! to `memcpy` rather than to `__memcpy_chk`. So the three headers that use the pack now behave the
//! way the dozen that do not already behaved, which is the point of doing it this way rather than
//! leaving them failing to compile.
//!
//! # Why this is answered after the call is checked
//!
//! The same reason `check/builtin/trap.rs` is. Both rows carry `int(void)`, so a call to one has a
//! prototype, and it is the prototype that reports `__builtin_va_arg_pack(1)` in the ordinary
//! words. The signature is a fiction, since neither of these is a function, and it is a useful one:
//! an `int` is something a variadic call accepts as an argument, which is the one position the pack
//! is ever written in.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};

use crate::check::Checker;
use crate::expr::ExprId;

/// The pack itself, which stands for every anonymous argument.
const PACK: &str = "__builtin_va_arg_pack";

/// How many of them there were.
const LENGTH: &str = "__builtin_va_arg_pack_len";

impl Checker<'_> {
    /// Refuses a call to one of the pair where nothing could forward the arguments.
    ///
    /// Gives back a poisoned node for a call that is refused, and nothing for every other call in
    /// the program, including an accepted one: an accepted call stands as the call the prototype
    /// checked, because the body it is in is dropped whole and the node in it is never read.
    ///
    /// Answers nothing for almost every call, so the test that costs a byte goes first, and the
    /// name decides it rather than the declaration for the reason it does next door: the reserved
    /// prefix is what says the name belongs to the implementation.
    pub(in crate::check) fn argument_pack_builtin(
        &mut self,
        function: Option<Symbol>,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_va_arg_pack") {
            return None;
        }
        if spelled != PACK && spelled != LENGTH {
            return None;
        }
        if self.in_unemitted_variadic_function() {
            return None;
        }
        let spelled = spelled.to_owned();
        self.report(
            Diagnostic::error(
                format!(
                    "'{spelled}' is only accepted in a variadic function nothing is emitted for, \
                     since this compiler does not inline and has nothing to forward"
                ),
                span,
            )
            .with_code("E0694"),
        );
        Some(self.poison(span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_ast::Ast;
    use rucc_base::Interner;
    use rucc_gnu::{Kind, Status};
    use rucc_session::Std;
    use rucc_target::{TargetInfo, Triple};
    use rucc_types::IntKind;

    use super::*;
    use crate::check::Context;
    use crate::check::stmt::Enclosing;

    /// What a checker needs to exist, since it borrows all of it for as long as it lives, plus the
    /// one call every test here checks.
    ///
    /// The call is built before the checker, because the tree is what the checker borrows and a
    /// test that wants a second checker over the same call cannot be adding to the tree by then.
    struct Fixture {
        ast: Ast,
        names: Interner,
        target: TargetInfo,
    }

    impl Fixture {
        fn new() -> Fixture {
            let target =
                TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a triple"));
            Fixture { ast: Ast::new(), names: Interner::new(), target }
        }

        /// A call to the named builtin with no arguments, which is how both of them are written.
        fn call(&mut self, name: &str) -> rucc_ast::ExprId {
            let name = self.names.intern(name);
            let callee = self.ast.expr(rucc_ast::Expr::Name(name), Span::DUMMY);
            let args = self.ast.add_expr_list(&[]);
            self.ast.expr(rucc_ast::Expr::Call { callee, args }, Span::DUMMY)
        }

        fn checker(&self) -> Checker<'_> {
            Checker::new(&self.ast, Context::new(&self.names, &self.target, Std::C23))
        }
    }

    /// Checks the call inside a body of the given shape and gives back what was reported.
    ///
    /// The body is opened by hand rather than by checking a definition, because the pairing these
    /// turn on is the whole of what a definition would be contributing.
    fn reported(f: &Fixture, call: rucc_ast::ExprId, variadic: bool, emitted: bool) -> Vec<String> {
        let mut c = f.checker();
        let ret = c.types.int(IntKind::Int);
        c.open_body(Enclosing { variadic, emitted, ..Enclosing::returning(ret) });
        c.check_expr(call);
        c.errors.diagnostics().iter().map(|d| d.message.clone()).collect()
    }

    /// The pairing is the rule, so all four shapes of a body are worth asking about and so is being
    /// outside one, where there is no argument pack to talk about at all. Both names answer the
    /// same, since neither can be filled in for the same reason.
    #[test]
    fn the_argument_pack_stands_only_where_nothing_is_emitted_for_the_function_around_it() {
        for name in [PACK, LENGTH] {
            let mut f = Fixture::new();
            let call = f.call(name);

            assert!(
                reported(&f, call, true, false).is_empty(),
                "{name} in a variadic body nothing is emitted for"
            );

            for (variadic, emitted) in [(true, true), (false, false), (false, true)] {
                let messages = reported(&f, call, variadic, emitted);
                assert_eq!(
                    messages,
                    vec![format!(
                        "'{name}' is only accepted in a variadic function nothing is emitted for, \
                         since this compiler does not inline and has nothing to forward"
                    )],
                    "{name} in a body with variadic {variadic} and emitted {emitted}"
                );
            }

            let mut c = f.checker();
            c.check_expr(call);
            assert_eq!(c.errors.diagnostics().len(), 1, "{name} outside a function");
        }
    }

    /// Both names have to be rows of the table carrying a signature, because the signature is what
    /// the call is checked against before this is asked about it, and neither may be implemented,
    /// because `__has_builtin` answering yes for one of these would tell a header it may write
    /// something this compiler cannot do anything with.
    #[test]
    fn both_names_are_rows_of_the_table_that_carry_a_signature_and_are_not_done() {
        for name in [PACK, LENGTH] {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Partial, "{name}");
            assert!(!feature.status.is_available(), "{name}");
            assert_eq!(feature.signature, "int(void)", "{name}");
            assert!(feature.library.is_empty(), "{name} is not a call to anything");
        }
    }
}
