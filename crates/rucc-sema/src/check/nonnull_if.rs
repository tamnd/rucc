//! `__attribute__((nonnull_if_nonzero(p, n)))` and `nonnull_if_nonzero(p, n, m)`, gcc 15's
//! `nonnull` with a condition: the `p`th argument may be a null pointer where the `n`th is zero,
//! or where either of the `n`th and the `m`th is, and must not be one otherwise. glibc 2.42 writes
//! it on `memcpy` and the rest of `<string.h>`, where a null pointer with a length of zero is a
//! call C2y makes valid.
//!
//! gcc reads it for its `-Wnonnull` warning about a call and for what its optimizers may assume
//! about the pointer. This compiler takes `nonnull` without assuming anything from it, which is
//! always safe, and takes this the same way. The warning is in `check/nonnull.rs`, and what is
//! here is gcc's checking of the numbers, in its words: each one names a parameter the way
//! `nonnull`'s do, through gcc's `positional_argument`, and the first has to be a pointer and the
//! others integers. An attribute whose numbers pass is handed back for the warning to read, and
//! one gcc says something about is dropped, as gcc drops it.

use rucc_ast::AttrList;
use rucc_diag::Diagnostic;
use rucc_types::TypeId;

use crate::check::Checker;
use crate::check::nonnull::Conditional;
use crate::check::string_arg::{Lead, Position, Wanted};

/// The attribute, as gcc spells it.
const NAME: &str = "nonnull_if_nonzero";

impl Checker<'_> {
    /// What gcc says about every `nonnull_if_nonzero` in the lists of one declaration of that
    /// type.
    ///
    /// A function and a pointer to one are what it is for, since gcc applies an attribute that
    /// wants a function type to what the pointer points at. On any other type gcc warns that it
    /// only applies to function types. Every argument is checked whatever was said about the one
    /// before it, which is what gcc does, and the attributes that pass are handed back, for a
    /// function to have its calls checked against.
    pub(in crate::check) fn nonnull_if_nonzero(
        &mut self,
        lists: &[AttrList],
        ty: TypeId,
    ) -> Vec<Conditional> {
        let ast = self.ast;
        let mut taken = Vec::new();
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if self.gnu_name(&attr) != NAME {
                    continue;
                }
                let args = &ast[attr.args];
                if !(2..=3).contains(&args.len()) {
                    let what =
                        format!("wrong number of arguments specified for '{NAME}' attribute");
                    let refused = Diagnostic::error(what, attr.span).with_code("E0846");
                    let note = format!("expected between 2 and 3, found {}", args.len());
                    self.report(refused.note(note, attr.span));
                    continue;
                }
                let Some(function) = self.function_type(ty) else {
                    let what = format!("'{NAME}' attribute only applies to function types");
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    continue;
                };
                let mut numbers = Vec::new();
                for (index, &arg) in args.iter().enumerate() {
                    let lead = Lead { name: NAME, argno: Some(index + 1) };
                    let wanted = if index == 0 { Wanted::Pointer } else { Wanted::Integer };
                    if let Position::Number(value, spelled) = self.position(attr, arg, lead) {
                        if self.names_a(attr, function, value, &spelled, lead, wanted) {
                            if let Ok(value) = usize::try_from(value) {
                                numbers.push(value);
                            }
                        }
                    }
                }
                if numbers.len() == args.len() {
                    if let [pointer, count, rest @ ..] = numbers.as_slice() {
                        let other = rest.first().copied();
                        taken.push(Conditional { pointer: *pointer, count: *count, other });
                    }
                }
            }
        }
        taken
    }
}
