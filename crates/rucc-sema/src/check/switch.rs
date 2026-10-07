//! The warnings gcc gives about a whole `switch` once its body has been read, in gcc 13's words,
//! in its order and at its places.
//!
//! What is checked is gcc's `c_do_switch_warnings`, read one rule at a time:
//!
//! * A `switch` without a `default` is said at the `switch`, under `-Wswitch-default`.
//! * One whose controlling expression is a truth value is said at the `switch` under
//!   `-Wswitch-bool`, unless its cases are what a truth table has: a case outside 0 and 1 is
//!   worth saying, and so are both of them with a `default` that can never be taken, and only 0
//!   and 1 without one is not. A truth value is a `_Bool`, or a comparison, `&&`, `||` or `!`
//!   once the left of any comma is put aside, and neither is when the expression starts with a
//!   cast, which is how a program says it meant it.
//! * Of a `switch` on an enumeration, each enumerator no case covers is said at the `switch`, in
//!   the order they were declared. With a `default` that is only worth `-Wswitch-enum`, and
//!   without one it is `-Wswitch`, or `-Wswitch-enum` where only that is on. An enumerator marked
//!   `unused` or `[[maybe_unused]]` is left out, and so is every one but the value of a
//!   controlling expression that folds.
//! * Then each case value that is no enumerator's is said at its `case`, in the order of the
//!   values, under the same pair of options. Each end of a range has to be an enumerator's own,
//!   so `case A ... C` is quiet about its middle and `case 4 ... 5` is said twice. An enumeration
//!   with no enumerators says none of this, nor does one marked `flag_enum`, gcc 15's way of
//!   saying that its values are bits to be combined.
//!
//! gcc names an enumeration with a tag by the typedef the controlling expression was declared
//! with, where there was one, as `'t2' {aka 'enum e2'}`. A typedef name is not kept on the type
//! here, so this compiler says `'enum e2'`.
//!
//! gcc gives an enumerator the enumeration's type in a `switch` on it, so `switch (A)` is a
//! `switch` on the enumeration there. Before C23 this compiler gives an enumerator the `int` it
//! is, and a `switch` on one is not checked. Nor is gcc's exception for the enumerators with
//! reserved names a system header declares, which this checker has no system headers to tell.
//!
//! `flag_enum` marks an enumeration where it is written about the enumeration itself, as
//! `enum __attribute__((flag_enum)) f { ... }` or after the closing brace. gcc also takes it on
//! a declaration or a typedef of an enumeration, as being about a variant of the type that
//! declaration has, and this compiler, which keeps no such variants, takes it there and does
//! nothing with it. On any other type it is ignored with gcc's warning.

use rucc_ast::{self as ast, AttrList, AttrSyntax, Attribute, BinaryOp, UnaryOp};
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{EnumId, TypeId, TypeKind};

use crate::check::Checker;
use crate::expr::{ExprId, ExprKind};
use crate::stmt::Case;

/// The code of the warning about a case or an enumerator, which answers to `-Wswitch`, or to
/// `-Wswitch-enum` where only that is on.
const SWITCH: &str = "E0852";
/// The code of the warning about an enumerator a `switch` with a `default` leaves out.
const SWITCH_ENUM: &str = "E0853";
/// The code of the warning about a `switch` without a `default`.
const SWITCH_DEFAULT: &str = "E0854";
/// The code of the warning about a `switch` on a truth value.
const SWITCH_BOOL: &str = "E0855";
/// The code of the error about `flag_enum` given arguments.
const FLAG_ENUM_ARITY: &str = "E0856";

/// What one `switch` was found to be once its body was read.
pub(in crate::check) struct Switched<'a> {
    /// The type of the controlling expression as it was written, before the promotion.
    pub(in crate::check) written: TypeId,
    /// The controlling expression, promoted, which is what a case value is held in.
    pub(in crate::check) cond: ExprId,
    /// Whether gcc reads the controlling expression as a truth value.
    pub(in crate::check) boolean: bool,
    /// The cases, in the order they were written.
    pub(in crate::check) cases: &'a [Case],
    /// Where each of them was written.
    pub(in crate::check) spans: &'a [Span],
    /// Whether there is a `default`.
    pub(in crate::check) default: bool,
    /// Where the `switch` was written.
    pub(in crate::check) at: Span,
}

impl Checker<'_> {
    /// Remembers the enumerators marked `unused` or `[[maybe_unused]]`, which a `switch` that
    /// leaves them out is quiet about.
    pub(in crate::check) fn read_unused_enumerator(
        &mut self,
        id: EnumId,
        name: Symbol,
        attrs: AttrList,
    ) {
        let unused = self.ast[attrs].iter().any(|attr| {
            self.gnu_name(attr) == "unused"
                || (attr.syntax == AttrSyntax::Standard
                    && attr.namespace.is_none()
                    && self.text(attr.name) == "maybe_unused")
        });
        if unused {
            self.advice.unused_enumerators.insert((id, name));
        }
    }

    /// Remembers an enumeration marked `flag_enum`, whose values are bits a program may combine,
    /// so that a case value made of them is not one a `switch` on it says is not among them.
    pub(in crate::check) fn read_flag_enum(&mut self, id: EnumId, attrs: AttrList) {
        let ast = self.ast;
        let mut marked = false;
        for &attr in &ast[attrs] {
            if self.is_flag_enum(&attr) {
                marked |= self.flag_enum_arity(attr);
            }
        }
        if marked {
            self.advice.flag_enums.insert(id);
        }
    }

    /// Says what gcc says of `flag_enum` written about a type that is not an enumeration, which
    /// is that it is ignored. On a declaration it is about the type declared.
    pub(in crate::check) fn flag_enum_misplaced(&mut self, lists: &[AttrList], ty: TypeId) {
        if matches!(self.types.kind(self.types.canonical(ty)), TypeKind::Enum(_)) {
            return;
        }
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if self.is_flag_enum(&attr) && self.flag_enum_arity(attr) {
                    let what = "'flag_enum' attribute ignored on non-enum";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
            }
        }
    }

    /// Whether that is `flag_enum`, in any of gcc 15's spellings, `clang::flag_enum` among them.
    fn is_flag_enum(&self, attr: &Attribute) -> bool {
        if attr.namespace.is_some_and(|ns| self.text(ns) == "clang") {
            return self.gnu_name(&Attribute { namespace: None, ..*attr }) == "flag_enum";
        }
        self.gnu_name(attr) == "flag_enum"
    }

    /// Whether `flag_enum` was given no arguments, as it takes, said in gcc's words where not.
    fn flag_enum_arity(&mut self, attr: Attribute) -> bool {
        let count = self.ast[attr.args].len();
        if count == 0 {
            return true;
        }
        let what = "wrong number of arguments specified for 'flag_enum' attribute";
        let refused = Diagnostic::error(what, attr.span).with_code(FLAG_ENUM_ARITY);
        self.report(refused.note(format!("expected 0, found {count}"), attr.span));
        false
    }

    /// Whether gcc reads a controlling expression as a truth value, given as it was written and
    /// as it was checked, before the promotion.
    pub(in crate::check) fn truth_valued(&self, written: ast::ExprId, cond: ExprId) -> bool {
        if self.starts_with_cast(written) {
            return false;
        }
        let mut last = cond;
        while let ExprKind::Comma { rhs, .. } = self.tast[last].kind {
            last = rhs;
        }
        let ty = self.types.canonical(self.tast[cond].ty);
        matches!(self.types.kind(ty), TypeKind::Bool)
            || matches!(
                self.tast[last].kind,
                ExprKind::Binary {
                    op: BinaryOp::Lt
                        | BinaryOp::Gt
                        | BinaryOp::Le
                        | BinaryOp::Ge
                        | BinaryOp::Eq
                        | BinaryOp::Ne
                        | BinaryOp::LogAnd
                        | BinaryOp::LogOr,
                    ..
                } | ExprKind::Unary { op: UnaryOp::Not, .. }
            )
    }

    /// Whether the first thing written in an expression is a parenthesized type name, which is
    /// what gcc looks at: `(int)b < c` starts with a cast as much as `(int)b` does.
    fn starts_with_cast(&self, mut id: ast::ExprId) -> bool {
        loop {
            id = match self.ast[id] {
                ast::Expr::Cast { .. } | ast::Expr::CompoundLiteral { .. } => return true,
                ast::Expr::Index { base, .. } | ast::Expr::Member { base, .. } => base,
                ast::Expr::Call { callee, .. } => callee,
                ast::Expr::Unary { op, operand } if op.is_postfix() => operand,
                ast::Expr::Binary { lhs, .. }
                | ast::Expr::Assign { lhs, .. }
                | ast::Expr::Comma { lhs, .. } => lhs,
                ast::Expr::Cond { cond, .. } => cond,
                _ => return false,
            };
        }
    }

    /// Says what gcc says about a `switch` once its body has been read.
    pub(in crate::check) fn switch_warnings(&mut self, switch: &Switched<'_>) {
        if !switch.default {
            self.report(
                Diagnostic::warning("switch missing default case", switch.at)
                    .with_code(SWITCH_DEFAULT),
            );
        }
        if switch.boolean && self.not_a_truth_table(switch) {
            self.report(
                Diagnostic::warning("switch condition has boolean value", switch.at)
                    .with_code(SWITCH_BOOL),
            );
        }
        let TypeKind::Enum(id) = self.types.kind(self.types.canonical(switch.written)) else {
            return;
        };
        let enumerators = self.types.enum_info(id).enumerators.clone();
        let constant = {
            let mut eval = self.eval();
            eval.integer(switch.cond).ok().filter(|_| !eval.deferred())
        };
        let mut order: Vec<usize> = (0..switch.cases.len()).collect();
        order.sort_by_key(|&index| switch.cases[index].low);
        // Which end of each case some enumerator is, by the case's place in `order`.
        let mut low_seen = vec![false; order.len()];
        let mut high_seen = vec![false; order.len()];
        for enumerator in &enumerators {
            let value = enumerator.value;
            if let Ok(at) = order.binary_search_by_key(&value, |&index| switch.cases[index].low) {
                low_seen[at] = true;
                continue;
            }
            // The case below it might be a range with it inside, and it is only the high end
            // that it can be.
            let below = order.partition_point(|&index| switch.cases[index].low < value);
            if let Some(at) = below.checked_sub(1) {
                let case = &switch.cases[order[at]];
                if case.high > case.low && case.high >= value {
                    high_seen[at] |= case.high == value;
                    continue;
                }
            }
            if self.advice.unused_enumerators.contains(&(id, enumerator.name))
                || constant.is_some_and(|constant| constant != value)
            {
                continue;
            }
            let name = self.text(enumerator.name).to_owned();
            let code = if switch.default { SWITCH_ENUM } else { SWITCH };
            self.report(
                Diagnostic::warning(
                    format!("enumeration value '{name}' not handled in switch"),
                    switch.at,
                )
                .with_code(code),
            );
        }
        // A value made of an enumeration's bits is one of its values when it says it is made of
        // them.
        if enumerators.is_empty() || self.advice.flag_enums.contains(&id) {
            return;
        }
        let named = self.enumerated(id, switch.written);
        for (at, &index) in order.iter().enumerate() {
            let case = &switch.cases[index];
            let mut values = Vec::new();
            if !low_seen[at] {
                values.push(case.low);
            }
            if case.high > case.low && !high_seen[at] {
                values.push(case.high);
            }
            for value in values {
                self.report(
                    Diagnostic::warning(
                        format!("case value '{value}' not in enumerated type{named}"),
                        switch.spans[index],
                    )
                    .with_code(SWITCH),
                );
            }
        }
    }

    /// Whether the cases of a `switch` on a truth value are not just what a truth table has.
    fn not_a_truth_table(&self, switch: &Switched<'_>) -> bool {
        let low = switch.cases.iter().map(|case| case.low).min();
        let high = switch.cases.iter().map(|case| case.high).max();
        high.is_some_and(|high| high > 1)
            || low.is_some_and(|low| low < 0)
            || (switch.default && high == Some(1) && low == Some(0))
    }

    /// The enumeration a case value is not in, as gcc names it after the words: by its tag, by
    /// the first typedef that named it where it has no tag, and not at all where it has neither.
    fn enumerated(&self, id: EnumId, written: TypeId) -> String {
        if self.types.enum_info(id).tag.is_some() {
            return format!(" {}", self.gcc_quoted(written));
        }
        let named =
            self.types.aliases().iter().find(|alias| {
                self.types.kind(self.types.canonical(alias.of)) == TypeKind::Enum(id)
            });
        named.map_or_else(String::new, |alias| format!(" '{}'", self.text(alias.name)))
    }
}
