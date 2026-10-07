//! `__attribute__((hardbool))`, gcc 14's hardened boolean types.
//!
//! The attribute makes an integer type into a `bool` that is stored as two numbers of the
//! program's choosing, `false_value` and `true_value`, zero and its complement where it chooses
//! nothing. gcc builds the type as an enumeration over the integer type, and so does this, which is
//! what gives it the integer's size, alignment and place in a call. Its values decay to `bool`
//! wherever an lvalue would be read, a conversion into it is a conversion to `bool` and then to the
//! representation of that, and reading anything but one of the two representations traps: that
//! last part is the hardening, and the conversions and the trap are `Conv::value`,
//! `Conv::to_type` and the lowering's.
//!
//! Here is gcc's checking of the attribute, in its words, and the type it makes.

use rucc_ast::{AttrArg, AttrList, Attribute};
use rucc_diag::{Diagnostic, Span};
use rucc_types::{Hardbool, IntegerInfo, TypeId, TypeKind, integer_info};

use crate::check::Checker;

/// How the type gcc gives a bit-field narrower than its hardened type is spelled, which is an
/// enumeration with no name: the narrower integer under it has none.
const NARROWER: &str = "enum <anonymous>";

impl Checker<'_> {
    /// The type a `hardbool` in an attribute list makes of the type it was written on, and the
    /// type as written where there is no such attribute in it.
    ///
    /// Each one is applied to what the one before it made, which is the order gcc applies them
    /// in, so a second one is written on a type that is no longer an integer and is refused.
    pub(in crate::check) fn hardened(&mut self, ty: TypeId, attrs: AttrList) -> TypeId {
        let written = self.ast[attrs].to_vec();
        let mut ty = ty;
        for attr in written {
            if self.gnu_name(&attr) != "hardbool" {
                continue;
            }
            if let Some(made) = self.harden(ty, attr) {
                ty = made;
            }
        }
        ty
    }

    /// One `hardbool` applied to the type it was written on, and [`None`] where it was refused.
    ///
    /// The type has to be one of the standard integer types, which is gcc's `INTEGER_TYPE`: a
    /// `bool`, an enumeration and a `_BitInt` are each a kind of their own there and refused. Its
    /// qualifiers and its `_Atomic` are kept and go back on the type it makes.
    fn harden(&mut self, ty: TypeId, attr: Attribute) -> Option<TypeId> {
        let args = self.ast[attr.args].to_vec();
        if args.len() > 2 {
            let what = "wrong number of arguments specified for 'hardbool' attribute";
            let note = format!("expected between 0 and 2, found {}", args.len());
            let refused = Diagnostic::error(what, attr.span).with_code("E0845");
            self.report(refused.note(note, attr.span));
            return None;
        }
        let canonical = self.types.canonical(ty);
        let quals = self.types.quals(canonical);
        let (atomic, base) = match self.types.kind(canonical) {
            TypeKind::Atomic(inner) => (true, inner),
            _ => (false, ty),
        };
        let base = self.types.unqualified(base);
        let shape = match self.types.kind(self.types.canonical(base)) {
            TypeKind::Int(_) => integer_info(&self.types, base, self.cx.target),
            _ => None,
        };
        let Some(shape) = shape else {
            let what = "'hardbool' attribute only supported on integral types";
            self.report(Diagnostic::error(what, attr.span).with_code("E0845"));
            return None;
        };

        let mut written = [None, None];
        for (slot, &arg) in written.iter_mut().zip(&args) {
            *slot = Some(self.hardbool_argument(arg, attr.span)?);
        }
        let to = self.spell(base);
        let false_value = match written[0] {
            Some(arg) => self.represented(arg, shape, &to, attr.span),
            None => 0,
        };
        let true_value = match written[1] {
            Some(arg) => self.represented(arg, shape, &to, attr.span),
            None => shape.wrap(!false_value),
        };
        if false_value == true_value {
            let what = format!(
                "'hardbool' attribute requires different values for 'false' and 'true' for type \
                 '{to}'"
            );
            self.report(Diagnostic::error(what, attr.span).with_code("E0845"));
            return None;
        }
        let canonical_base = self.types.canonical(base);
        let made = self
            .types
            .hardbool(canonical_base, Hardbool { false_value, true_value, written });
        let made = if atomic { self.types.atomic(made) } else { made };
        Some(self.types.qualified(made, quals))
    }

    /// One argument of a `hardbool`, as the number it is and the type it was written in.
    ///
    /// gcc takes an integer constant expression or an enumerator, and stops with an internal
    /// error on anything else, so the refusal of anything else is in words of this compiler's.
    fn hardbool_argument(&mut self, arg: AttrArg, span: Span) -> Option<(i128, TypeId)> {
        let what = "'hardbool' attribute argument is not an integer constant";
        match arg {
            AttrArg::Expr(expr) => {
                let value = self.expr(expr);
                match self.eval_integer(value) {
                    Ok(number) => Some((number, self.tast[value].ty)),
                    Err(failed) => {
                        if !failed.poisoned {
                            let at = self.tast.expr_span(failed.at);
                            self.report(Diagnostic::error(what, at).with_code("E0845"));
                        }
                        None
                    }
                }
            }
            AttrArg::Ident(name) => match self.enumerator(name) {
                Some(number) => Some((number, self.int())),
                None => {
                    self.report(Diagnostic::error(what, span).with_code("E0845"));
                    None
                }
            },
        }
    }

    /// An argument as a value of the type, warned about in gcc's words where the type cannot
    /// hold it, which is only ever a signed one: a conversion to an unsigned type wraps and is
    /// silent, in gcc as in C.
    fn represented(&mut self, arg: (i128, TypeId), shape: IntegerInfo, to: &str, at: Span) -> i128 {
        let (number, ty) = arg;
        let value = shape.wrap(number);
        if shape.signed && value != number {
            let from = self.spell(ty);
            let what = format!(
                "overflows in conversion from '{from}' to '{to}' changes value from '{number}' to \
                 '{value}'"
            );
            self.report(Diagnostic::warning(what, at).with_code("E0703"));
        }
        value
    }

    /// What gcc says about a bit-field of a hardened type narrower than the type.
    ///
    /// gcc gives the member a narrower integer type and applies the attribute to that again with
    /// the arguments as they were written, so a representation that does not fit in the width is
    /// cut down to it, with the warning a signed one gets, and two that come out the same are
    /// refused: a field that holds the same bits for both has no way left to say which it is.
    /// What the two are cut down to is the lowering's to work out again from the width, since
    /// the member keeps the type it was declared with.
    pub(in crate::check) fn hardbool_bit_field(&mut self, ty: TypeId, width: u32, span: Span) {
        let Some(hardbool) = self.types.hardbool_of(ty) else { return };
        let Some(shape) = integer_info(&self.types, ty, self.cx.target) else { return };
        if width >= shape.width {
            return;
        }
        let narrower = IntegerInfo::new(shape.signed, width);
        let false_value = match hardbool.written[0] {
            Some(arg) => self.represented(arg, narrower, NARROWER, span),
            None => 0,
        };
        let true_value = match hardbool.written[1] {
            Some(arg) => self.represented(arg, narrower, NARROWER, span),
            None => narrower.wrap(!false_value),
        };
        if false_value == true_value {
            let what = format!(
                "'hardbool' attribute requires different values for 'false' and 'true' for type \
                 '{NARROWER}'"
            );
            self.report(Diagnostic::error(what, span).with_code("E0845"));
        }
    }
}
