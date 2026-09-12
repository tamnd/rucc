//! `conj`, `creal` and `cimag`, which are the halves of a complex value and not a call.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! These three are the whole of what `complex.h` promises about a value that is already in front
//! of the compiler. `creal(z)` is the real half of the object, `cimag(z)` is the other one, and
//! `conj(z)` is the pair with the sign of the second flipped. None of them rounds, raises anything
//! or has a case it cannot answer, so there is nothing for a library to do that the translation
//! cannot do itself, and gcc emits no call for any of them on any target.
//!
//! The language already has two of the three. `__real__` and `__imag__` are what `creal` and
//! `cimag` mean, and `~z` on a complex operand is the conjugate, so what this file does is put the
//! names on the operators rather than describe anything new. That is also why the answers fold: a
//! static initializer written `double x = creal(1.5 + 2.5i);` reaches the same folding `__real__`
//! already had.
//!
//! # Both spellings
//!
//! The prefixed names are what gcc declares and they are not what programs write. `complex.h`
//! declares `creal` and glibc's copy of it does not spell the prefixed one anywhere, so a program
//! that includes the header and calls the function reaches the plain name every time. So the plain
//! names are here too, on the same rows, and which of the two was written decides nothing.
//!
//! The plain name is only the library's where nothing else has taken it, which is why the callee's
//! declaration is looked at and not only the name. `check/builtin/abs.rs` explains that check at
//! length and this family asks for it in the same words. `-fno-builtin`, `-fno-builtin-creal` and
//! `-ffreestanding` are the other half of the question and are answered in
//! [`Context::means_the_library`], and they leave the prefixed spelling alone, because writing the
//! prefix is the program saying which function it means.
//!
//! # Why the call cannot be left behind
//!
//! All three are in the math library rather than the C one. A program that wrote only the prefixed
//! spelling never had a reason to ask for `-lm`, and one that included `complex.h` and wrote the
//! plain name under gcc never had one either, since gcc expands both. A call left behind is
//! therefore a program that compiles and does not link, which is the failure
//! `check/builtin/sign.rs` describes for `fabs` and is the same failure here.
//!
//! # What is not here
//!
//! The rest of `complex.h`. `cabs`, `carg`, `cexp` and the others are genuinely the library's
//! work, they have rounding and error rules this compiler has nothing to say about, and a call to
//! one is the right answer.
//!
//! What `_Complex long double` does below the front end, which is issue 326. The rows for the
//! `long double` width are here and are checked here, and a program that reaches one still stops
//! where every other use of that type stops.
//!
//! [`Context::means_the_library`]: crate::check::Context::means_the_library

use rucc_ast::UnaryOp;
use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_types::{FloatKind, TypeId};

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// One name of the family, and what it does.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// The plain name, which is the one `complex.h` declares and the one a program usually writes.
    name: &'static str,
    /// The prefixed spelling, which is a row of `features.toml` carrying the type the call is
    /// checked against and means this whatever the program has done with the plain name.
    builtin: &'static str,
    /// The operator the name is another way of writing. `~` on a complex operand is the
    /// conjugate, which is why the complement is the one that stands for `conj`.
    op: UnaryOp,
    /// The floating type the halves are in. Unlike the classification family none of these is
    /// type generic: gcc gives `__builtin_creal` a `_Complex double` parameter and a `double`
    /// result, and the spellings ending in a width their own.
    at: FloatKind,
}

/// Every name in the family.
///
/// The prefixed spellings are rows of `features.toml` carrying the types this checks a
/// declaration against, and the test at the bottom of this file is what keeps the two from
/// drifting apart.
const FAMILY: &[Row] = &[
    Row { name: "conjf", builtin: "__builtin_conjf", op: UnaryOp::BitNot, at: FloatKind::Float },
    Row { name: "conj", builtin: "__builtin_conj", op: UnaryOp::BitNot, at: FloatKind::Double },
    Row {
        name: "conjl",
        builtin: "__builtin_conjl",
        op: UnaryOp::BitNot,
        at: FloatKind::LongDouble,
    },
    Row { name: "crealf", builtin: "__builtin_crealf", op: UnaryOp::Real, at: FloatKind::Float },
    Row { name: "creal", builtin: "__builtin_creal", op: UnaryOp::Real, at: FloatKind::Double },
    Row {
        name: "creall",
        builtin: "__builtin_creall",
        op: UnaryOp::Real,
        at: FloatKind::LongDouble,
    },
    Row { name: "cimagf", builtin: "__builtin_cimagf", op: UnaryOp::Imag, at: FloatKind::Float },
    Row { name: "cimag", builtin: "__builtin_cimag", op: UnaryOp::Imag, at: FloatKind::Double },
    Row {
        name: "cimagl",
        builtin: "__builtin_cimagl",
        op: UnaryOp::Imag,
        at: FloatKind::LongDouble,
    },
];

impl Checker<'_> {
    /// The half a call to one of the complex functions is, if the call is one.
    ///
    /// Answers nothing for every other call in the program, which is nearly every call, so the
    /// tests that cost a byte go first.
    ///
    /// Taken after the call has been checked rather than before it, for the reason
    /// `check/builtin/expect.rs` gives at more length: the prototype is what reports the argument
    /// count, converts the argument to the parameter type and refuses something that will not
    /// convert, and all of that would have to be written again here to gain nothing. The prefixed
    /// spellings have a prototype for the same reason, which is why they carry a signature in
    /// `features.toml` where the sign family next door carries none.
    pub(in crate::check) fn complex_builtin_value(
        &mut self,
        callee: ExprId,
        function: Option<Symbol>,
        args: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        let row = *FAMILY
            .iter()
            .find(|row| row.name == spelled || row.builtin == spelled)
            .filter(|_| self.cx.means_the_library(spelled))?;
        let whole = self.types.complex_float(row.at);
        let ty = self.result_type(row);
        if !self.callee_is_the_library_one(callee, ty, &[whole]) {
            return None;
        }
        let &operand = args.first()?;
        if self.is_poisoned(operand) {
            return Some(self.poison(span));
        }
        Some(
            self.tast.expr(
                Expr::new(ExprKind::Unary { op: row.op, operand }, ty, Category::Rvalue),
                span,
            ),
        )
    }

    /// What one of these answers with, which is the whole value for the conjugate and one half
    /// for the other two.
    fn result_type(&mut self, row: Row) -> TypeId {
        match row.op {
            UnaryOp::BitNot => self.types.complex_float(row.at),
            _ => self.types.float(row.at),
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every prefixed spelling here has to be a row of the table carrying the type this checks a
    /// declaration against, or the two lists say different things about what `creal` takes.
    #[test]
    fn every_prefixed_spelling_of_the_complex_family_is_a_row_of_the_table_with_its_type() {
        for row in FAMILY {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, row.builtin) else {
                panic!("{} is answered here and is not in features.toml", row.builtin);
            };
            assert_eq!(feature.status, Status::Implemented, "{}", row.builtin);
            let width = match row.at {
                FloatKind::Float => "float",
                FloatKind::Double => "double",
                FloatKind::LongDouble => "long double",
                other => panic!("{other:?} is not one of the three widths this family has"),
            };
            let result = match row.op {
                UnaryOp::BitNot => format!("_Complex {width}"),
                _ => width.to_string(),
            };
            assert_eq!(feature.signature, format!("{result}(_Complex {width})"), "{}", row.builtin);
        }
    }

    /// The names are what the lookup searches, so a name written twice is a second row nothing
    /// can ever reach.
    #[test]
    fn no_name_of_the_complex_family_is_in_the_table_twice() {
        let mut names: Vec<&str> = FAMILY.iter().flat_map(|row| [row.name, row.builtin]).collect();
        names.sort_unstable();
        let all = names.len();
        names.dedup();
        assert_eq!(names.len(), all, "a name is in the table twice");
    }
}
