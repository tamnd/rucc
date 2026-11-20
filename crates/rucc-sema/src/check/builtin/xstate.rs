//! `__builtin_ia32_xsave` and the rest of its family, which save the processor's extended state
//! to memory or restore it from there.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! Each is one instruction that takes the area through its pointer and the components to save or
//! restore as a mask in `edx` and `eax`, the high half in the first. gcc writes the instruction
//! where the call was, and so does this: the call becomes [`ExprKind::XState`], which the lowering
//! turns into the instruction with the mask split across the two registers. The mm selftests
//! save and restore the protection keys register this way in protection_keys.c.
//!
//! The `64` forms are the ones with REX.W, which only x86-64 has, and the others are on i386 as
//! well. A call anywhere else is refused where it is written, as the time stamp counter is.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::TypeId;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind, XStateOp};

/// The code a call on another target is refused under, the one the time stamp counter uses.
const CODE: &str = "E0727";

/// The names, each with the instruction it is and whether it is only on x86-64.
const NAMES: [(&str, XStateOp, bool); 12] = [
    ("__builtin_ia32_xsave", XStateOp::Xsave, false),
    ("__builtin_ia32_xsave64", XStateOp::Xsave64, true),
    ("__builtin_ia32_xrstor", XStateOp::Xrstor, false),
    ("__builtin_ia32_xrstor64", XStateOp::Xrstor64, true),
    ("__builtin_ia32_xsaveopt", XStateOp::Xsaveopt, false),
    ("__builtin_ia32_xsaveopt64", XStateOp::Xsaveopt64, true),
    ("__builtin_ia32_xsavec", XStateOp::Xsavec, false),
    ("__builtin_ia32_xsavec64", XStateOp::Xsavec64, true),
    ("__builtin_ia32_xsaves", XStateOp::Xsaves, false),
    ("__builtin_ia32_xsaves64", XStateOp::Xsaves64, true),
    ("__builtin_ia32_xrstors", XStateOp::Xrstors, false),
    ("__builtin_ia32_xrstors64", XStateOp::Xrstors64, true),
];

impl Checker<'_> {
    /// The node a call to one of the family becomes, if the name is one of them.
    ///
    /// Taken after the call has been checked, for the reason `check/builtin/tsc.rs` gives: the
    /// rows carry signatures, so the prototype has reported a call with the wrong arguments and
    /// converted the mask to `long long`.
    pub(in crate::check) fn xstate_builtin(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        ret: TypeId,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_ia32_x") {
            return None;
        }
        let &(spelled, op, wide) = NAMES.iter().find(|(named, ..)| *named == spelled)?;
        let arch = self.cx.target.tuple.arch().as_str();
        let here = if wide { arch == "x86_64" } else { matches!(arch, "x86_64" | "i686") };
        if !here {
            let on = if wide { "x86_64" } else { "x86" };
            let what = format!("`{spelled}` is only available on {on}");
            let note = "it is one instruction of that machine, and there is no function of the \
                        name anywhere for a call on this target to reach";
            self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
            return Some(self.poison(span));
        }
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return Some(self.poison(span));
        }
        let (&address, &mask) = (args.first()?, args.get(1)?);
        let kind = ExprKind::XState { op, address, mask };
        Some(self.tast.expr(Expr::new(kind, ret, Category::Rvalue), span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The names are rows of the table with the one signature, since the call is checked
    /// against it before this looks at it, and none is a call to anything.
    #[test]
    fn the_names_are_rows_of_the_table_that_carry_a_signature_and_no_library() {
        for (name, ..) in NAMES {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert_eq!(feature.signature, "void(void *, long long)", "{name}");
            assert!(feature.library.is_empty(), "{name} is not a call to anything");
        }
    }
}
