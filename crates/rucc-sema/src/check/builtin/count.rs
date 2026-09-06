//! The bit counting builtins: `clz`, `ctz`, `popcount`, `parity` and `ffs`, each in three widths.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5, and tamnd/rucc#310.
//!
//! Five questions about which bits of a value are set. A program writes one to walk a bitmap, to
//! find the size of a number in bits, to round up to a power of two, or to pick the next free slot
//! out of a word. The kernel's `find_next_bit` is built on them, ffmpeg counts leading zeroes in its
//! bitstream reader, and SQLite uses one to size a page. Fifteen rows of `features.toml` and one
//! node, because the five differ only in the question.
//!
//! # Why the name decides this
//!
//! Every spelling here carries the `__builtin_` prefix, and the prefix is what says the name belongs
//! to the implementation. `ffs` on its own is a POSIX function a program may define, and this leaves
//! that name alone: a call to plain `ffs` is a call, the way it is today. Only the prefixed names
//! become this node, so nothing a program can write is taken away from it.
//!
//! # Why the answer is taken after the call is checked
//!
//! Same reason `check/builtin/bswap.rs` gives. Each row has a signature, so the ordinary call
//! checking reports the argument count and converts the argument to the type of the parameter. That
//! conversion is the whole reason there are three widths of each name rather than one: what
//! `__builtin_clzll` counts is the leading zeroes of a `unsigned long long`, and what
//! `__builtin_clz` counts is the leading zeroes of the same value narrowed to `unsigned int`, which
//! is a different number.
//!
//! # The width of the operand and the width of the answer
//!
//! They are not the same, which is the one place this differs from the byte swaps. The operand keeps
//! the width its declaration gave it, because that width is the question. The answer is an `int` at
//! every width, because a count of bits in a `unsigned long long` still fits in one comfortably.
//! Carrying both is what lets the walk to the IR count at the operand's width and then narrow, which
//! is what the machine does too.
//!
//! # Zero
//!
//! `__builtin_clz(0)` and `__builtin_ctz(0)` are undefined, which is gcc's rule and is written down
//! here because it is easy to assume it is an accident of some machine and it is not: `bsr` and
//! `bsf` leave the destination unchanged for a zero input rather than writing an answer into it, so
//! a rule for either claims nothing about zero. The other three are defined everywhere, and `ffs(0)`
//! is zero rather than undefined, which is the one exception in the family and is the reason `ffs`
//! costs a comparison that `ctz` does not.

use rucc_base::Symbol;
use rucc_diag::Span;

use crate::check::Checker;
use crate::expr::{BitCount, Category, Expr, ExprId, ExprKind};

/// The five questions and the stem each is spelled with.
///
/// The width suffix is not here because it is not part of the question: `__builtin_clz`,
/// `__builtin_clzl` and `__builtin_clzll` all ask for leading zeroes and differ only in the type the
/// signature converts the argument to. The test at the bottom of this file is what keeps this list
/// and the fifteen rows of the table from drifting apart.
const FAMILY: &[(&str, BitCount)] = &[
    ("clz", BitCount::Leading),
    ("ctz", BitCount::Trailing),
    ("popcount", BitCount::Ones),
    ("parity", BitCount::Parity),
    ("ffs", BitCount::FirstSet),
];

/// The three width suffixes, in the order a longest match has to try them.
///
/// `ll` before `l` before nothing, because `__builtin_clzll` ends in `l` as well and stripping one
/// character off it would leave `__builtin_clzl`, which is a different row with a different type.
const WIDTHS: &[&str] = &["ll", "l", ""];

/// Which question a name asks, if it asks one.
fn question(spelled: &str) -> Option<BitCount> {
    let stem = spelled.strip_prefix("__builtin_")?;
    for &width in WIDTHS {
        let Some(head) = stem.strip_suffix(width) else { continue };
        if let Some(&(_, count)) = FAMILY.iter().find(|&&(name, _)| name == head) {
            return Some(count);
        }
    }
    None
}

impl Checker<'_> {
    /// The count a call to one of the bit counting builtins is, if the call is one.
    ///
    /// Answers nothing for every other call in the program, so the test that costs a byte goes
    /// first.
    pub(in crate::check) fn count_builtin_value(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_") {
            return None;
        }
        let count = question(spelled)?;
        // Nothing to count, which is the call written with no arguments. The count has been
        // reported by this point, so this leaves the ordinary call node alone and the program is
        // refused for the reason it was already going to be refused for.
        let &operand = args.first()?;
        if self.is_poisoned(operand) {
            return Some(self.poison(span));
        }
        // An `int` however wide the operand is, because a count of bits is a small number.
        let ty = self.int();
        Some(
            self.tast
                .expr(Expr::new(ExprKind::BitCount { operand, count }, ty, Category::Rvalue), span),
        )
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every one of the fifteen names is a row of the table carrying a signature, because the
    /// signature is what the call is checked against before this replaces it. A row without one
    /// would never be declared and the call would be to an undeclared name.
    #[test]
    fn all_fifteen_names_are_rows_of_the_table_that_carry_a_signature() {
        for &(stem, want) in FAMILY {
            for &width in WIDTHS {
                let name = format!("__builtin_{stem}{width}");
                let Some(feature) = rucc_gnu::lookup(Kind::Builtin, &name) else {
                    panic!("{name} is answered here and is not in features.toml");
                };
                assert_eq!(feature.status, Status::Implemented, "{name}");
                assert!(!feature.signature.is_empty(), "{name} is checked against its prototype");
                assert!(feature.library.is_empty(), "{name} is not a call to anything");
                assert_eq!(question(&name), Some(want), "{name}");
            }
        }
    }

    /// The answer is an `int` at every width and the operand is not, which is the thing about this
    /// family that is easy to get wrong. A signature that answered in the operand's type would make
    /// `__builtin_popcountll(x) - 1` a different number when the count is zero.
    #[test]
    fn every_signature_answers_in_int_and_asks_about_the_width_its_name_says() {
        for &(stem, _) in FAMILY {
            // `ffs` is the one whose operand is signed, because that is what the C library's `ffs`
            // takes and the builtin is the same function.
            let of = if stem == "ffs" { "" } else { "unsigned " };
            for (width, spelled) in [("", "int"), ("l", "long"), ("ll", "long long")] {
                let name = format!("__builtin_{stem}{width}");
                let feature = rucc_gnu::lookup(Kind::Builtin, &name).expect("a row");
                assert_eq!(feature.signature, format!("int({of}{spelled})"), "{name}");
            }
        }
    }

    /// The width suffix is stripped longest first, so the `ll` names are not read as the `l` ones
    /// with a stray character. Getting this backwards would count at the wrong width and would do
    /// it silently, since both spellings are real names.
    #[test]
    fn a_name_ending_in_two_ells_is_not_the_one_ending_in_one() {
        assert_eq!(question("__builtin_clzll"), Some(BitCount::Leading));
        assert_eq!(question("__builtin_clzl"), Some(BitCount::Leading));
        assert_eq!(question("__builtin_clz"), Some(BitCount::Leading));
    }

    /// Nothing outside the family is answered here, and in particular the plain names are left to
    /// the program. `ffs` is POSIX rather than ISO C and a program may define its own.
    #[test]
    fn a_name_without_the_prefix_or_outside_the_family_asks_nothing() {
        for name in ["ffs", "clz", "popcount", "__builtin_clzlll", "__builtin_cl", "__builtin_"] {
            assert_eq!(question(name), None, "{name}");
        }
    }
}
