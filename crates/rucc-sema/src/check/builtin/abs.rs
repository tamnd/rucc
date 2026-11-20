//! `abs` and its neighbours, which are the magnitude of an integer and not a call.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! These are the first of the family the design calls the library calls with known semantics, and
//! they are the family where the plain name is enough. `abs` is reserved to the implementation by
//! C23 7.1.3, so a program that writes it means the `abs` the library promises, and a compiler
//! that knows what that one does may write the four instructions instead of the call. Every C
//! compiler does, which is why `gcc.c-torture/execute/20021127-1.c` defines `llabs` to abort and
//! expects the call not to reach it.
//!
//! # The unsigned four
//!
//! `__builtin_uabs` and the three beside it answer in the unsigned type of the same width as the
//! argument. That is the one shape of absolute value with no undefined case: the most negative
//! value of a signed type has no positive counterpart in that type and has one in the unsigned
//! type beside it. The draft after C23 adds `uabs`, `ulabs`, `ullabs` and `umaxabs` to
//! `stdlib.h`, so under `-std=c2y` and `-std=gnu2y` the plain names are the library's in the same
//! way `abs` is, and gcc 16 expands them there and calls them in every dialect before it. Before
//! C2y the names are the program's, since nothing reserved them, and a call is a call.
//!
//! What gets built is the same four instructions, because on a two's complement machine the
//! magnitude and the unsigned magnitude are the same bits. The difference is entirely in the type
//! of the answer, which is what decides how a comparison against it is done and how it widens.
//!
//! # The widest two
//!
//! `__builtin_imaxabs` and `__builtin_umaxabs` are the same pair at `intmax_t`, which is not a
//! fixed kind: it is `long` on a target whose `long` is sixty four bits wide and `long long`
//! everywhere else. [`Width::Max`] is that, asked of the target rather than written down, and the
//! signatures in the table say `intmax_t` and `uintmax_t` for the same reason.
//!
//! # Why the declaration is looked at and not only the name
//!
//! The prefixed spellings are decided by the name, the way the rest of `check/builtin.rs` decides
//! them, because the prefix is what says the name belongs to the implementation. The plain ones
//! cannot be: `abs` is only the library's `abs` where nothing else has taken it. So the callee has
//! to be a function with external linkage whose type is exactly the one the library gives that
//! name, which is what a program that means its own thing by the name does not have. A `static
//! long long llabs(long long)` is that program, measured against gcc 16.2.0, which calls it.
//!
//! `-fno-builtin`, `-fno-builtin-<name>` and `-ffreestanding` are the other half of the same
//! question and are answered in [`Context::means_the_library`]. A freestanding program has no C
//! library, so there is no promise about the name for the compiler to rely on.
//!
//! # Why this is the magnitude and not a fold
//!
//! Nothing here needs the argument to be a constant. gcc expands the call inline at `-O0` and so
//! does this, because the point is not that `llabs(-1)` is one, it is that the call does not
//! happen. A program that defines the name and calls it is a program where folding the constant
//! case and calling in the rest would still be wrong.
//!
//! What the magnitude of the most negative value is, is the value itself, here and in gcc. C says
//! the result is undefined when it cannot be represented, and both compilers reach that answer by
//! doing the arithmetic rather than by deciding anything.

use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_session::Std;
use rucc_types::IntKind;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The width one name of the family works at.
///
/// Three of the four are a kind, and the fourth is a question for the target, which is why this is
/// here rather than an [`IntKind`] in the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Width {
    /// `abs` and `uabs`.
    Int,
    /// `labs` and `ulabs`.
    Long,
    /// `llabs` and `ullabs`.
    LongLong,
    /// `imaxabs` and `umaxabs`, which work at `intmax_t`. Not a kind, because that type is `long`
    /// on a target whose `long` is sixty four bits wide and `long long` on one where it is not.
    Max,
}

/// One name of the family, and the type the library gives it.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// The plain name, which every row has.
    name: &'static str,
    /// The first strict dialect whose library defines the plain name. Before it the name is the
    /// program's under `-std=c89` and the rest of the ISO spellings, and only the prefixed one
    /// means this. A GNU dialect takes every plain name as the library's whatever its year, which
    /// is what gcc 16 does: `-std=c89` calls `llabs` and `-std=gnu89` expands it, and `-std=c23`
    /// calls `uabs` and `-std=gnu23` expands it.
    since: Std,
    /// The prefixed spelling, which is a row of `features.toml` and means this whatever the
    /// program has done with the plain name.
    builtin: &'static str,
    /// The width the argument and the answer are both at.
    at: Width,
    /// Whether the answer is the unsigned type of that width rather than the signed one. The
    /// argument is signed either way, since an unsigned value is its own magnitude.
    unsigned: bool,
}

/// Every name in the family.
///
/// The prefixed spellings are rows of `features.toml` carrying the type this checks against, and
/// the test at the bottom of this file is what keeps the two from drifting apart.
const FAMILY: &[Row] = &[
    Row { name: "abs", since: Std::C89, builtin: "__builtin_abs", at: Width::Int, unsigned: false },
    Row {
        name: "labs",
        since: Std::C89,
        builtin: "__builtin_labs",
        at: Width::Long,
        unsigned: false,
    },
    Row {
        name: "llabs",
        since: Std::C99,
        builtin: "__builtin_llabs",
        at: Width::LongLong,
        unsigned: false,
    },
    Row {
        name: "imaxabs",
        since: Std::C99,
        builtin: "__builtin_imaxabs",
        at: Width::Max,
        unsigned: false,
    },
    Row {
        name: "uabs",
        since: Std::C2y,
        builtin: "__builtin_uabs",
        at: Width::Int,
        unsigned: true,
    },
    Row {
        name: "ulabs",
        since: Std::C2y,
        builtin: "__builtin_ulabs",
        at: Width::Long,
        unsigned: true,
    },
    Row {
        name: "ullabs",
        since: Std::C2y,
        builtin: "__builtin_ullabs",
        at: Width::LongLong,
        unsigned: true,
    },
    Row {
        name: "umaxabs",
        since: Std::C2y,
        builtin: "__builtin_umaxabs",
        at: Width::Max,
        unsigned: true,
    },
];

impl Checker<'_> {
    /// The magnitude a call to one of the absolute value functions is, if the call is one.
    ///
    /// Answers nothing for every other call in the program, which is nearly every call, so the
    /// tests that cost a byte go first.
    ///
    /// Taken after the call has been checked rather than before it, for the reason
    /// `check/builtin/expect.rs` gives at more length: the prototype is what reports the argument
    /// count, converts the argument to the parameter type and refuses a structure handed to it,
    /// and all of that would have to be written again here to gain nothing.
    pub(in crate::check) fn abs_builtin_value(
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
            .find(|row| {
                row.builtin == spelled
                    || (row.name == spelled && (self.cx.gnu || self.cx.std >= row.since))
            })
            .filter(|_| self.cx.means_the_library(spelled))?;
        let takes = self.types.int(self.kind(row.at, false));
        let answers = self.types.int(self.kind(row.at, row.unsigned));
        if !self.callee_is_the_library_one(callee, answers, &[takes]) {
            return None;
        }
        let &operand = args.first()?;
        if self.is_poisoned(operand) {
            return Some(self.poison(span));
        }
        Some(self.tast.expr(Expr::new(ExprKind::Abs { operand }, answers, Category::Rvalue), span))
    }

    /// The integer kind one row works at, signed or unsigned.
    fn kind(&self, at: Width, unsigned: bool) -> IntKind {
        match (at, unsigned) {
            (Width::Int, false) => IntKind::Int,
            (Width::Int, true) => IntKind::UInt,
            (Width::Long, false) => IntKind::Long,
            (Width::Long, true) => IntKind::ULong,
            (Width::LongLong, false) => IntKind::LongLong,
            (Width::LongLong, true) => IntKind::ULongLong,
            (Width::Max, unsigned) => self.widest_integer(!unsigned),
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every prefixed spelling here has to be a row of the table carrying the type this checks a
    /// declaration against, or the two lists say different things about what `llabs` takes.
    #[test]
    fn every_prefixed_spelling_is_a_row_of_the_table_with_the_type_this_expects() {
        for row in FAMILY {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, row.builtin) else {
                panic!("{} is answered here and is not in features.toml", row.builtin);
            };
            assert_eq!(feature.status, Status::Implemented, "{}", row.builtin);
            let takes = match row.at {
                Width::Int => "int",
                Width::Long => "long",
                Width::LongLong => "long long",
                Width::Max => "intmax_t",
            };
            let answers = match (row.at, row.unsigned) {
                (_, false) => takes.to_owned(),
                (Width::Max, true) => "uintmax_t".to_owned(),
                (_, true) => format!("unsigned {takes}"),
            };
            assert_eq!(feature.signature, format!("{answers}({takes})"), "{}", row.builtin);
        }
    }

    /// The prefixed spelling of each is the plain one with the prefix on it, where there is a
    /// plain one, which is what makes the two spellings one row rather than two.
    #[test]
    fn the_prefixed_spelling_is_the_plain_name_with_the_prefix_on_it() {
        for row in FAMILY {
            assert_eq!(row.builtin.strip_prefix("__builtin_"), Some(row.name));
        }
    }

    /// The unsigned four are the four only the draft after C23 declares, so a strict dialect before
    /// it leaves the plain names to the program. Getting this backwards would take `uabs` away from
    /// a C23 program that defines it.
    #[test]
    fn the_unsigned_four_are_the_library_from_c2y_on() {
        for row in FAMILY {
            assert_eq!(row.since == Std::C2y, row.unsigned, "{}", row.builtin);
            let feature = rucc_gnu::lookup(Kind::Builtin, row.builtin).expect("a row");
            assert_eq!(feature.library.is_empty(), row.unsigned, "{}", row.builtin);
        }
    }
}
