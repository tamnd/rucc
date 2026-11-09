//! The builtins libgcc's DWARF unwinder is written with.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! `unwind-dw2.c` throws an exception without any assembly of its own. It asks the compiler for
//! six things, and these are the six:
//!
//! - `__builtin_unwind_init()` makes the function save every register a call preserves, so that
//!   the unwind table of the function says where each of them is in its frame.
//! - `__builtin_dwarf_cfa()` is the canonical frame address of the function, which is where the
//!   unwind table counts those places from.
//! - `__builtin_eh_return(offset, handler)` puts every saved register back from the frame, moves
//!   the stack pointer `offset` bytes above the canonical frame address and goes to `handler`.
//! - `__builtin_dwarf_sp_column()` is the DWARF column of the stack pointer, as an `unsigned int`.
//! - `__builtin_eh_return_data_regno(n)` is the DWARF column of the register the handler is handed
//!   its `n`th value in, or `-1` past the last of them.
//! - `__builtin_init_dwarf_reg_size_table(table)` writes how many bytes the unwinder keeps for each
//!   column into the bytes at `table`.
//!
//! The walk starts in a function that writes the first and the second: `uw_init_context` reads
//! the registers back out of that frame through its own unwind table, the personality routine
//! writes the handler's values over them through `_Unwind_SetGR`, and the third leaves with them.
//! The last three are facts about the target, and `rucc_target::unwinder` has the numbers gcc
//! answers with on each machine. The first three are nodes the back end writes.
//!
//! # Why these are answered after the call is checked
//!
//! The same reason `check/builtin/thread.rs` gives: each row in the table carries a signature, and
//! the prototype is what reports a call with the wrong arguments and converts the right ones.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::TypeId;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::tast::Const;

/// The six names.
const NAMES: [&str; 6] = [
    "__builtin_unwind_init",
    "__builtin_dwarf_cfa",
    "__builtin_eh_return",
    "__builtin_dwarf_sp_column",
    "__builtin_eh_return_data_regno",
    "__builtin_init_dwarf_reg_size_table",
];

/// The code for asking for one of them on a target whose unwinder is not described.
const CODE: &str = "E0871";

/// The code for a register number that is not a constant.
const CONSTANT: &str = "E0872";

impl Checker<'_> {
    /// The node a call to one of the six becomes, if the name is one of them.
    pub(in crate::check) fn unwind_builtin(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        ret: TypeId,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_") {
            return None;
        }
        let &spelled = NAMES.iter().find(|&&named| named == spelled)?;
        let arch = self.cx.target.tuple.arch();
        let Some(unwinder) = rucc_target::unwinder(arch) else {
            let what = format!("`{spelled}` is not available on {}", arch.as_str());
            let note = "it answers questions about the DWARF unwinder of the target, and this one \
                        is not described: x86-64, i686, AArch64 and RISC-V 64 are";
            self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
            return Some(self.poison(span));
        };
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return Some(self.poison(span));
        }
        let kind = match spelled {
            "__builtin_unwind_init" => ExprKind::UnwindInit,
            "__builtin_dwarf_cfa" => ExprKind::DwarfCfa,
            "__builtin_eh_return" => {
                let (&offset, &handler) = (args.first()?, args.get(1)?);
                ExprKind::EhReturn { offset, handler }
            }
            "__builtin_init_dwarf_reg_size_table" => {
                ExprKind::RegSizeTable { table: *args.first()? }
            }
            "__builtin_dwarf_sp_column" => {
                let column = i128::from(unwinder.sp_column);
                return Some(self.constant(Const::Int(column), ret, span));
            }
            _ => {
                let &arg = args.first()?;
                let Ok(number) = self.eval().integer(arg) else {
                    let at = self.tast.expr_span(arg);
                    let what = format!("the argument of `{spelled}` is a constant");
                    let note = "the answer is a column of the unwind table, which is a number the \
                                compiler writes down rather than one the program works out";
                    self.report(Diagnostic::error(what, at).with_code(CONSTANT).note(note, at));
                    return Some(self.poison(span));
                };
                let column = i128::from(unwinder.data_regno(number));
                return Some(self.constant(Const::Int(column), ret, span));
            }
        };
        Some(self.tast.expr(Expr::new(kind, ret, Category::Rvalue), span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every name has to be an implemented row of the table, with the signature gcc gives it, for
    /// the reasons given in `thread.rs`.
    #[test]
    fn the_names_are_rows_of_the_table_that_carry_gccs_signatures() {
        let signatures = [
            "void(void)",
            "void *(void)",
            "void(ptrdiff_t, void *)",
            "unsigned int(void)",
            "int(int)",
            "void(void *)",
        ];
        for (name, signature) in NAMES.into_iter().zip(signatures) {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert_eq!(feature.signature, signature, "{name}");
            assert!(feature.library.is_empty(), "{name} is not a call to anything");
        }
    }
}
