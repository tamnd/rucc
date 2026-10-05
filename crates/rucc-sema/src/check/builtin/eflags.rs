//! `__builtin_ia32_readeflags_u64` and `__builtin_ia32_writeeflags_u64`, and the `_u32` pair i386
//! has, which read and write the processor's flags register.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! There is no instruction that moves the flags to or from a general register, so gcc writes a
//! push and a pop through the stack: `pushfq` and `pop` to read them, `push` and `popfq` to write
//! them. The call becomes [`ExprKind::Eflags`], which the lowering turns into those two
//! instructions where the call was, the same way `check/builtin/tsc.rs` does for the time stamp
//! counter. The x86 selftests read and write the flags this way in helpers.h, to set the trap and
//! the nested task bits around a system call.
//!
//! The 64 bit pair is only on x86-64 and the 32 bit pair only on i386, which is where gcc has them,
//! and a call anywhere else is refused where it is written.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::TypeId;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The code a call on another target is refused under, the one the time stamp counter uses.
const CODE: &str = "E0727";

/// The four names, each with whether it writes and the architecture it is on.
const NAMES: [(&str, bool, &str); 4] = [
    ("__builtin_ia32_readeflags_u64", false, "x86_64"),
    ("__builtin_ia32_writeeflags_u64", true, "x86_64"),
    ("__builtin_ia32_readeflags_u32", false, "i386"),
    ("__builtin_ia32_writeeflags_u32", true, "i386"),
];

impl Checker<'_> {
    /// The node a call to one of the four becomes, if the name is one of them.
    ///
    /// Taken after the call has been checked, for the reason `check/builtin/tsc.rs` gives: the
    /// rows carry signatures, so the prototype has reported a call with the wrong arguments and
    /// converted the value a write is handed to the width of the register.
    pub(in crate::check) fn eflags_builtin(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        ret: TypeId,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_ia32_") {
            return None;
        }
        let &(spelled, writes, arch) = NAMES.iter().find(|(named, ..)| *named == spelled)?;
        if self.cx.target.tuple.arch().as_str() != arch {
            let what = format!("`{spelled}` is only available on {arch}");
            let note = "it is a push and a pop of the flags register in that mode, and there is no \
                        function of the name anywhere for a call on this target to reach";
            self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
            return Some(self.poison(span));
        }
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return Some(self.poison(span));
        }
        let value = if writes { Some(*args.first()?) } else { None };
        Some(self.tast.expr(Expr::new(ExprKind::Eflags { value }, ret, Category::Rvalue), span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The names are rows of the table with signatures, since the call is checked against them
    /// before this looks at it, and none is a call to anything.
    #[test]
    fn the_names_are_rows_of_the_table_that_carry_a_signature_and_no_library() {
        let signatures = [
            "unsigned long long(void)",
            "void(unsigned long long)",
            "unsigned int(void)",
            "void(unsigned int)",
        ];
        for ((name, ..), signature) in NAMES.iter().zip(signatures) {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert_eq!(feature.signature, signature, "{name}");
            assert!(feature.library.is_empty(), "{name} is not a call to anything");
        }
    }
}
