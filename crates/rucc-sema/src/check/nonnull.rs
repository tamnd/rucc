//! `-Wnonnull`: a null pointer handed to a parameter `nonnull` or `nonnull_if_nonzero` says must
//! not be one, said in gcc 15's words at the call.
//!
//! What is checked is gcc's `check_function_nonnull`, read one rule at a time:
//!
//! * A `nonnull` without numbers checks every argument, and then nothing more is checked. Without
//!   one, the arguments some `nonnull` names are checked.
//! * A `nonnull_if_nonzero(p, n)` checks the `p`th argument where the `n`th is an integer constant
//!   other than zero, and `nonnull_if_nonzero(p, n, m)` where the `m`th is one too. A count that
//!   is not a constant is not known to be anything, so nothing is said.
//! * An argument is null where it is a pointer and folds to zero, which `0`, `NULL` and
//!   `(char *)0` do and a pointer object holding null does not. Of a `?:` with a condition that
//!   does not fold, both arms are checked, each on its own.
//!
//! The warning is at the call, since a constant has no place of its own in gcc either, and its
//! note is at the function. A call through a pointer object marked with either attribute is
//! checked too, without the note, since gcc only has a function to point at when there is one.
//! gcc files the warning under `-Wnonnull`, which `-Wall` and `-Wformat` turn on.
//!
//! The attributes are read from the declarations of a name, so a call through any other pointer
//! to the function is not checked, and neither is a call to a library function declared without
//! them, which gcc checks against its own built-in declaration.

use rucc_ast::{AttrArg, AttrList};
use rucc_diag::{Diagnostic, Span};
use rucc_types::{TypeKind, is_integer};

use crate::check::Checker;
use crate::decl::{DeclId, DeclKind};
use crate::eval::truth;
use crate::expr::{ExprId, ExprKind};
use crate::scope::Binding;

/// The code of the warning, which answers to `-Wnonnull`.
const NONNULL: &str = "E0851";

/// What the declarations of one function say about which of its arguments must not be null.
#[derive(Debug, Clone, Default)]
pub(in crate::check) struct Nonnull {
    /// Whether some `nonnull` had no numbers, which asks for every argument.
    every: bool,
    /// The arguments some `nonnull` names, counted from one.
    named: Vec<usize>,
    /// Each `nonnull_if_nonzero` whose numbers passed.
    conditional: Vec<Conditional>,
}

/// One `nonnull_if_nonzero`, with its numbers counted from one.
#[derive(Debug, Clone, Copy)]
pub(in crate::check) struct Conditional {
    /// The pointer argument.
    pub pointer: usize,
    /// The count that has to be nonzero for the pointer to have to be.
    pub count: usize,
    /// The second count of the three-number form.
    pub other: Option<usize>,
}

impl Checker<'_> {
    /// Keeps what a declaration's `nonnull` attributes say, along with the `nonnull_if_nonzero`
    /// ones whose numbers passed, for its calls to be checked against.
    ///
    /// A number is read where it is a literal or an enumerator, which is every way the headers
    /// write one, and nothing is said about the others here.
    pub(in crate::check) fn read_nonnull(
        &mut self,
        decl: DeclId,
        lists: &[AttrList],
        conditional: Vec<Conditional>,
    ) {
        let ast = self.ast;
        for &list in lists {
            for attr in &ast[list] {
                if self.gnu_name(attr) != "nonnull" {
                    continue;
                }
                let args = &ast[attr.args];
                if args.is_empty() {
                    self.advice.nonnull.entry(decl).or_default().every = true;
                    continue;
                }
                for &arg in args {
                    let number = match arg {
                        AttrArg::Ident(name) => match self.scopes.lookup(name) {
                            Some(Binding::Enumerator { value, .. }) => usize::try_from(value).ok(),
                            _ => None,
                        },
                        AttrArg::Expr(_) => self.attribute_number(arg),
                    };
                    if let Some(number) = number {
                        self.advice.nonnull.entry(decl).or_default().named.push(number);
                    }
                }
            }
        }
        if !conditional.is_empty() {
            self.advice.nonnull.entry(decl).or_default().conditional.extend(conditional);
        }
    }

    /// Says which arguments of a call are null where the function's attributes say they must not
    /// be, once they are converted to the parameters' types.
    pub(in crate::check) fn heed_nonnull(&mut self, callee: ExprId, args: &[ExprId], span: Span) {
        let Some(decl) = self.called_decl(callee) else { return };
        let Some(nonnull) = self.advice.nonnull.get(&decl).cloned() else { return };
        let mut said = Vec::new();
        for (index, &arg) in args.iter().enumerate() {
            if nonnull.every || nonnull.named.contains(&(index + 1)) {
                for _ in 0..self.null_arms(arg) {
                    said.push((index + 1, None));
                }
            }
        }
        if !nonnull.every {
            for &Conditional { pointer, count, other } in &nonnull.conditional {
                let in_range = |n: usize| (1..=args.len()).contains(&n);
                if !in_range(pointer) || !in_range(count) || !other.is_none_or(in_range) {
                    continue;
                }
                let other = other.unwrap_or(count);
                if !self.nonzero(args[count - 1]) || !self.nonzero(args[other - 1]) {
                    continue;
                }
                for _ in 0..self.null_arms(args[pointer - 1]) {
                    said.push((pointer, Some((count, other))));
                }
            }
        }
        for (number, condition) in said {
            self.say_nonnull(decl, number, condition, span);
        }
    }

    /// The warning about one argument, with the note at the function when it is one.
    fn say_nonnull(
        &mut self,
        decl: DeclId,
        number: usize,
        condition: Option<(usize, usize)>,
        span: Span,
    ) {
        let what = match condition {
            None => format!("argument {number} null where non-null expected"),
            Some((count, other)) if count == other => format!(
                "argument {number} null where non-null expected because argument {count} is \
                 nonzero"
            ),
            Some((count, other)) => format!(
                "argument {number} null where non-null expected because arguments {count} and \
                 {other} are nonzero"
            ),
        };
        let mut warning = Diagnostic::warning(what, span).with_code(NONNULL);
        if self.tast[decl].kind == DeclKind::Function
            && let Some(name) = self.tast[decl].name
        {
            let attribute = if condition.is_some() { "nonnull_if_nonzero" } else { "nonnull" };
            let note =
                format!("in a call to function '{}' declared '{attribute}'", self.text(name));
            warning = warning.note(note, self.tast.decl_span(decl));
        }
        self.report(warning);
    }

    /// How many null pointers an argument may be: one where it is one, none where it is not, and
    /// for a `?:` whose condition does not fold, those of both arms.
    fn null_arms(&mut self, arg: ExprId) -> usize {
        match self.tast[arg].kind {
            ExprKind::Cond { cond, then, otherwise } => {
                let folded = self.eval().constant(cond).ok().and_then(truth);
                match folded {
                    Some(true) => self.null_arms(then),
                    Some(false) => self.null_arms(otherwise),
                    None => self.null_arms(then) + self.null_arms(otherwise),
                }
            }
            ExprKind::Cast(operand) | ExprKind::Convert { operand, .. }
                if self.is_pointer_typed(operand) =>
            {
                self.null_arms(operand)
            }
            _ => {
                usize::from(self.is_pointer_typed(arg) && self.conv().is_null_pointer_constant(arg))
            }
        }
    }

    /// Whether an argument is an integer constant other than zero.
    fn nonzero(&self, arg: ExprId) -> bool {
        let ty = self.types.canonical(self.tast[arg].ty);
        if !is_integer(&self.types, ty) {
            return false;
        }
        let mut eval = self.eval();
        matches!(eval.integer(arg), Ok(value) if value != 0) && !eval.deferred()
    }

    /// Whether an expression has a pointer type.
    fn is_pointer_typed(&self, expr: ExprId) -> bool {
        let ty = self.types.canonical(self.tast[expr].ty);
        matches!(self.types.kind(ty), TypeKind::Pointer(_))
    }
}
