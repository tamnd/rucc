//! The math library functions the compiler can answer when it is handed constants.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! `ceil`, `floor`, `trunc` and `round` take a number to an integer, and `fmax` and `fmin` pick
//! one of two. All six are functions of `math.h`, all six are called when the program hands them
//! something it worked out at run time, and all six have an answer at translation time when it
//! hands them a constant instead. The answer is what this file is: the call is built the ordinary
//! way and then replaced, and a call whose arguments are not constants is left exactly as it was.
//!
//! # Why the call stays a call
//!
//! This is the opposite of what `check/builtin/sign.rs` next door does, and the difference is
//! worth writing down because both are math library functions. `fabs` is the sign bit and nothing
//! else, so gcc emits no call for one on any target and leaving the call behind would mean a
//! program that wrote `__builtin_fabs` and never asked for `-lm` no longer links. These six are
//! not like that. gcc emits `jmp ceil` for `__builtin_ceil` on x86-64 at the default
//! architecture, measured on gcc 16.2.0, and reaches the `roundsd` instruction only under
//! `-msse4.1`, which is not what a program gets unless it asks. So a program that writes one of
//! these and links without `-lm` fails under gcc too, and matching that is the whole job.
//!
//! # Which ones fold and which do not
//!
//! `rint` and `nearbyint` are in this family everywhere except here. What they answer depends on
//! the rounding mode the program is running under, which a compiler folding a constant does not
//! know, so gcc refuses `static double x = __builtin_rint(2.5);` as not a constant expression and
//! keeps the call. They are rows of `features.toml` carrying the library function to call and they
//! are not rows of the table below.
//!
//! `fmax` and `fmin` of a nan do not fold either, for the same reason turned around: the nan rule
//! is the library's, 7.12.12.2 saying the answer is the other operand, and gcc refuses that one as
//! not a constant as well. [`Float::larger`] knows the rule and this asks it only where gcc does.
//!
//! # Both spellings
//!
//! A program that includes `math.h` and calls `ceil` reaches the plain name, because glibc's copy
//! of the header does not spell the prefixed one anywhere. gcc folds the plain name too, and
//! `-fno-builtin` and `-fno-builtin-ceil` turn that off and leave the prefixed spelling alone,
//! which is what [`Context::means_the_library`] answers and what was measured against gcc 16.2.0
//! one flag at a time.
//!
//! [`Context::means_the_library`]: crate::check::Context::means_the_library

use rucc_base::Symbol;
use rucc_base::float::{Float, Integral};
use rucc_diag::Span;
use rucc_types::FloatKind;

use crate::check::Checker;
use crate::expr::ExprId;
use crate::tast::Const;

/// What one name does with the numbers it is handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// The integer the number is taken to, in the direction the name says.
    To(Integral),
    /// The larger of the two, which is `fmax`.
    Larger,
    /// The smaller of the two, which is `fmin`.
    Smaller,
}

impl Op {
    /// How many arguments the name takes.
    const fn arity(self) -> usize {
        match self {
            Op::To(_) => 1,
            Op::Larger | Op::Smaller => 2,
        }
    }

    /// The answer, or nothing where gcc will not answer either.
    fn answer(self, args: &[Float]) -> Option<Float> {
        let first = *args.first()?;
        match self {
            Op::To(toward) => Some(first.to_integral(toward)),
            Op::Larger | Op::Smaller => {
                let second = *args.get(1)?;
                if first.is_nan() || second.is_nan() {
                    return None;
                }
                Some(match self {
                    Op::Larger => first.larger(second),
                    _ => first.smaller(second),
                })
            }
        }
    }
}

/// One name of the family, in both its spellings.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// The plain name, which is the one `math.h` declares and the one programs write.
    name: &'static str,
    /// The prefixed spelling, which is a row of `features.toml` carrying the type checked against
    /// and the library function the call is made to.
    builtin: &'static str,
    /// What the name does.
    op: Op,
    /// The type of the arguments and of the answer, which the spelling decides.
    at: FloatKind,
}

/// Every name that is answered here.
///
/// Each of the six is three rows, one per width, because gcc gives each spelling its own type
/// rather than making the family type generic. The test at the bottom of this file is what keeps
/// these and `features.toml` from drifting apart.
const FAMILY: &[Row] = &[
    Row { name: "ceil", builtin: "__builtin_ceil", op: UP, at: FloatKind::Double },
    Row { name: "ceilf", builtin: "__builtin_ceilf", op: UP, at: FloatKind::Float },
    Row { name: "ceill", builtin: "__builtin_ceill", op: UP, at: FloatKind::LongDouble },
    Row { name: "floor", builtin: "__builtin_floor", op: DOWN, at: FloatKind::Double },
    Row { name: "floorf", builtin: "__builtin_floorf", op: DOWN, at: FloatKind::Float },
    Row { name: "floorl", builtin: "__builtin_floorl", op: DOWN, at: FloatKind::LongDouble },
    Row { name: "trunc", builtin: "__builtin_trunc", op: ZERO, at: FloatKind::Double },
    Row { name: "truncf", builtin: "__builtin_truncf", op: ZERO, at: FloatKind::Float },
    Row { name: "truncl", builtin: "__builtin_truncl", op: ZERO, at: FloatKind::LongDouble },
    Row { name: "round", builtin: "__builtin_round", op: NEAREST, at: FloatKind::Double },
    Row { name: "roundf", builtin: "__builtin_roundf", op: NEAREST, at: FloatKind::Float },
    Row { name: "roundl", builtin: "__builtin_roundl", op: NEAREST, at: FloatKind::LongDouble },
    Row { name: "fmax", builtin: "__builtin_fmax", op: Op::Larger, at: FloatKind::Double },
    Row { name: "fmaxf", builtin: "__builtin_fmaxf", op: Op::Larger, at: FloatKind::Float },
    Row { name: "fmaxl", builtin: "__builtin_fmaxl", op: Op::Larger, at: FloatKind::LongDouble },
    Row { name: "fmin", builtin: "__builtin_fmin", op: Op::Smaller, at: FloatKind::Double },
    Row { name: "fminf", builtin: "__builtin_fminf", op: Op::Smaller, at: FloatKind::Float },
    Row { name: "fminl", builtin: "__builtin_fminl", op: Op::Smaller, at: FloatKind::LongDouble },
];

/// The four directions, named short enough that a row fits on a line.
const UP: Op = Op::To(Integral::Upward);
const DOWN: Op = Op::To(Integral::Downward);
const ZERO: Op = Op::To(Integral::TowardZero);
const NEAREST: Op = Op::To(Integral::NearestTiesAway);

impl Checker<'_> {
    /// The value a call to one of these is, if the call is one and the arguments are constants.
    ///
    /// Answers nothing for every other call in the program, which is nearly every call, so the
    /// tests that cost a byte go first. Answering nothing leaves the call, which is the right
    /// answer for a call to `ceil` of something nobody knows yet.
    ///
    /// Taken after the call has been checked rather than before it, which is what lets the
    /// prototype report the argument count and convert each argument to the type the name says.
    /// `__builtin_ceil(1)` is the call that needs it: the argument folds to an integer constant
    /// and the fold below wants the `double` the conversion made of it.
    pub(in crate::check) fn math_library_value(
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
        let ty = self.types.float(row.at);
        let wanted = row.op.arity();
        if !self.callee_is_the_library_one(callee, ty, &vec![ty; wanted]) {
            return None;
        }
        // A call the prototype already refused is one with the wrong number of arguments or one
        // with a mistake inside an argument, and neither is improved by folding what is left.
        if args.len() != wanted || args.iter().any(|&arg| self.is_poisoned(arg)) {
            return None;
        }
        let (values, found) = self.constants(args);
        let values = values?;
        let answer = row.op.answer(&values)?;
        // Absorbed only now, because a warning about an argument belongs to whoever reports the
        // expression it is in. If this folds, the arguments are gone and this is the only report
        // there will be; if it does not, the call stands and the next thing to fold it says so.
        self.absorb(found);
        Some(self.constant(Const::Float(answer), ty, span))
    }

    /// Every argument as a floating constant, or nothing when one of them is not one, along with
    /// whatever the folding found on the way.
    fn constants(&self, args: &[ExprId]) -> (Option<Vec<Float>>, Vec<rucc_diag::Diagnostic>) {
        let mut eval = self.eval();
        let values = args
            .iter()
            .map(|&arg| match eval.constant(arg) {
                Ok(Const::Float(value)) => Some(value),
                _ => None,
            })
            .collect();
        (values, eval.finish())
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every prefixed spelling has to be a row of `features.toml` that carries the type this
    /// checks a declaration against and the library function the call is made to. A row without
    /// the library function is a call to a name no object file defines.
    #[test]
    fn every_prefixed_spelling_is_a_row_of_the_table_that_is_called_under_its_own_name() {
        for row in FAMILY {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, row.builtin) else {
                panic!("{} is answered here and is not in features.toml", row.builtin);
            };
            assert_eq!(feature.status, Status::Implemented, "{}", row.builtin);
            assert_eq!(feature.library, row.name, "{}", row.builtin);
            let written = match row.at {
                FloatKind::Float => "float(float)",
                FloatKind::Double => "double(double)",
                FloatKind::LongDouble => "long double(long double)",
                other => panic!("{other:?} is not one of the three widths this family has"),
            };
            let signature = match row.op.arity() {
                1 => written.to_owned(),
                _ => pair(written),
            };
            assert_eq!(feature.signature, signature, "{}", row.builtin);
        }
    }

    /// The two the rounding mode decides are in `features.toml` as calls and are deliberately not
    /// answered here, so this is what notices if one is ever added to the table above.
    #[test]
    fn the_two_that_read_the_rounding_mode_are_called_and_not_folded() {
        for name in ["rint", "nearbyint"] {
            for suffix in ["", "f", "l"] {
                let spelled = format!("__builtin_{name}{suffix}");
                let feature = rucc_gnu::lookup(Kind::Builtin, &spelled).expect("in the table");
                assert_eq!(feature.status, Status::Implemented, "{spelled}");
                assert_eq!(feature.library, format!("{name}{suffix}"), "{spelled}");
                assert!(!FAMILY.iter().any(|row| row.builtin == spelled), "{spelled} folds");
            }
        }
    }

    /// The names are what the lookup searches, so a name written twice is a second row nothing
    /// can ever reach, and a prefixed spelling that is not the plain name with the prefix on it
    /// is a row that answers for one function under the type of another.
    #[test]
    fn no_name_is_in_the_table_twice_and_the_two_spellings_are_one_name() {
        let mut names: Vec<&str> = FAMILY.iter().map(|row| row.name).collect();
        names.sort_unstable();
        let all = names.len();
        names.dedup();
        assert_eq!(names.len(), all, "a name is in the table twice");
        for row in FAMILY {
            assert_eq!(row.builtin.strip_prefix("__builtin_"), Some(row.name));
        }
    }

    /// A signature of two arguments of the one type, written the way `features.toml` writes it.
    fn pair(single: &str) -> String {
        let (ret, param) = single.split_once('(').expect("a signature");
        let param = param.trim_end_matches(')');
        format!("{ret}({param}, {param})")
    }
}
