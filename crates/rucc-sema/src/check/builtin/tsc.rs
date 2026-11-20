//! `__builtin_ia32_rdtsc` and `__builtin_ia32_rdtscp`, which read the processor's time stamp
//! counter.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! Both are one instruction. `rdtsc` leaves the counter in `edx` and `eax`, the high half in the
//! first, and `rdtscp` does the same after waiting for the instructions before it and also leaves
//! the processor's `TSC_AUX` in `ecx`, which the builtin stores through its pointer argument. gcc
//! writes the instruction where the call was, and so does this: the call becomes
//! [`ExprKind::TimeStamp`], which the lowering turns into the instruction and the arithmetic that
//! puts the two halves together.
//!
//! # Why not a call to a routine that is the instruction
//!
//! That is what these were first, calls to two routines in `librucc_builtins.a`. It worked for a
//! program rucc linked and for nothing else: an object rucc compiled and gcc linked had two names
//! no library defines, and so did an extension rucc built for a server gcc built, which `dlopen`
//! then refused. Postgres 19 reads the counter in `instr_time.h`, which most of the server
//! includes, and tamnd/rucc#2191 is the mixed build that found it. An object that has the
//! instruction in it needs nothing from anybody.
//!
//! # Only on x86-64
//!
//! The instruction is x86's, and a call anywhere else would be to a name nothing defines, which
//! gcc does not accept either. It is refused where it is written instead.

use rucc_ast::UnaryOp;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{TypeId, pointee_as_written};

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The code a call on another target is refused under.
const CODE: &str = "E0727";

/// The two names, which are the whole of what this recognises.
const RDTSC: &str = "__builtin_ia32_rdtsc";
const RDTSCP: &str = "__builtin_ia32_rdtscp";

impl Checker<'_> {
    /// The node a call to one of the two becomes, if the name is one of them.
    ///
    /// Answers nothing for every other call in the program, so the test that costs a byte goes
    /// first. Taken after the call has been checked, for the reason `check/builtin/trap.rs` gives:
    /// the rows carry signatures, so the prototype is what reports a call with the wrong arguments,
    /// and the pointer `rdtscp` is handed has already been converted to `unsigned int *`.
    pub(in crate::check) fn time_stamp_builtin(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        ret: TypeId,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_ia32_rdtsc") {
            return None;
        }
        let stores = match spelled {
            RDTSC => false,
            RDTSCP => true,
            _ => return None,
        };
        let spelled = spelled.to_owned();
        if self.cx.target.tuple.arch().as_str() != "x86_64" {
            let what = format!("`{spelled}` is only available on x86-64");
            let note = "it is one x86 instruction, and there is no function of the name anywhere \
                        for a call on this target to reach";
            self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
            return Some(self.poison(span));
        }
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return Some(self.poison(span));
        }
        let aux = if stores {
            // No pointer at all, which the prototype has already refused.
            let &pointer = args.first()?;
            // The object the pointer points at, as an lvalue, so that the lowering stores into it
            // the way it would store into `*aux` written out.
            let ty = pointee_as_written(&self.types, self.tast[pointer].ty)?;
            let at = self.tast.expr_span(pointer);
            let node = ExprKind::Unary { op: UnaryOp::Deref, operand: pointer };
            Some(self.tast.expr(Expr::new(node, ty, Category::Lvalue), at))
        } else {
            None
        };
        Some(self.tast.expr(Expr::new(ExprKind::TimeStamp { aux }, ret, Category::Rvalue), span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Both names are rows of the table with signatures, since the call is checked against them
    /// before this looks at it, and neither is a call to anything, which is the point of them.
    #[test]
    fn the_names_are_rows_of_the_table_that_carry_a_signature_and_no_library() {
        for (name, signature) in
            [(RDTSC, "unsigned long long(void)"), (RDTSCP, "unsigned long long(unsigned int *)")]
        {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert_eq!(feature.signature, signature, "{name}");
            assert!(feature.library.is_empty(), "{name} is not a call to anything");
        }
    }
}
