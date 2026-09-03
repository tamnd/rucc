//! The builtins whose answer is a constant, which is what a static initializer needs.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! Most of the library builtins mean the library function of the same name, and a call to one is
//! the whole of what they are. A handful are not like that, because a program writes them where
//! nothing may be called at all: `double n = __builtin_nan("");` at file scope is an initializer
//! for an object with static storage duration, and C 6.7.10p4 says such a thing has to be a
//! constant expression, so a compiler that leaves it as a call refuses a program gcc accepts.
//!
//! So these are answered here, in the front end, before the callee is looked up. Each is a value
//! nothing has to compute: an infinity and a nan are bit patterns, and the length or the order of
//! two string literals is known as soon as the literals are.
//!
//! # When it does not fold
//!
//! Every row wants its arguments written out. `__builtin_nan(p)` for a pointer that came from
//! somewhere else has nothing here to read, and `__builtin_strlen(s)` for a real string is a call
//! to `strlen` and always was. Nothing is reported for either: this hands back nothing, the call
//! is checked and lowered the ordinary way, and the answer is the library function, which is
//! what gcc does with the same program.
//!
//! That is also why the caller has to know which declarations this compiler made for itself. A
//! file with `__builtin_nan(p)` in it leaves a declaration of `__builtin_nan` at file scope, and
//! a `__builtin_nan("1")` later in the same file has to fold anyway.
//!
//! # What is not here
//!
//! The folds that need arithmetic rather than a bit pattern. `__builtin_copysign` and
//! `__builtin_fabs` are the two the corpus wants next and they are in the same family in gcc, but
//! their answer is a value built out of an operand rather than one written down, so they belong
//! with the lowering of the rest of the math builtins.

use rucc_ast as ast;
use rucc_base::Symbol;
use rucc_base::float::Float;
use rucc_diag::Span;
use rucc_lex::Encoding;
use rucc_types::{FloatKind, float_format};

use crate::check::Checker;
use crate::expr::ExprId;
use crate::tast::Const;

/// What one name answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// An infinity, of the type the name says. Takes nothing.
    Infinity(FloatKind),
    /// A nan, quiet or signalling, whose payload is written out as a string.
    Nan(FloatKind, bool),
    /// How long a string literal is, not counting its terminator.
    Length,
    /// How two string literals are ordered.
    Compare,
}

impl Answer {
    /// How many arguments the name takes, which is also how many string literals it wants.
    const fn arity(self) -> usize {
        match self {
            Answer::Infinity(_) => 0,
            Answer::Nan(..) => 1,
            Answer::Length => 1,
            Answer::Compare => 2,
        }
    }
}

/// Every name that is answered here.
///
/// `huge_val` and `inf` are two names for one thing. C89 had `HUGE_VAL` as the value `math.h`
/// returns on overflow, which need not have been an infinity on a machine without one, and gcc
/// gives the two builtins the same answer on every target it has.
const TABLE: &[(&str, Answer)] = &[
    ("__builtin_inf", Answer::Infinity(FloatKind::Double)),
    ("__builtin_inff", Answer::Infinity(FloatKind::Float)),
    ("__builtin_infl", Answer::Infinity(FloatKind::LongDouble)),
    ("__builtin_huge_val", Answer::Infinity(FloatKind::Double)),
    ("__builtin_huge_valf", Answer::Infinity(FloatKind::Float)),
    ("__builtin_huge_vall", Answer::Infinity(FloatKind::LongDouble)),
    ("__builtin_nan", Answer::Nan(FloatKind::Double, true)),
    ("__builtin_nanf", Answer::Nan(FloatKind::Float, true)),
    ("__builtin_nanl", Answer::Nan(FloatKind::LongDouble, true)),
    ("__builtin_nans", Answer::Nan(FloatKind::Double, false)),
    ("__builtin_nansf", Answer::Nan(FloatKind::Float, false)),
    ("__builtin_nansl", Answer::Nan(FloatKind::LongDouble, false)),
    ("__builtin_strlen", Answer::Length),
    ("__builtin_strcmp", Answer::Compare),
];

impl Checker<'_> {
    /// Answers a call to one of these, if the name is one and the arguments are written out.
    pub(super) fn constant_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        let spelled = self.text(name);
        let &(_, answer) = TABLE.iter().find(|(row, _)| *row == spelled)?;
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        if written.len() != answer.arity() {
            // A call with the wrong number of arguments is a mistake, and the ordinary path is
            // where it is reported, against the prototype the table gives the name.
            return None;
        }
        match answer {
            Answer::Infinity(kind) => {
                let format = float_format(kind, self.cx.target);
                Some(self.folded_float(kind, Float::infinity(format, false), span))
            }
            Answer::Nan(kind, quiet) => {
                let payload = payload(&self.literal(written[0])?)?;
                let format = float_format(kind, self.cx.target);
                Some(self.folded_float(kind, Float::nan_with(format, false, quiet, payload), span))
            }
            Answer::Length => {
                let bytes = self.literal(written[0])?;
                let length = length(&bytes);
                let ty = self.size_type();
                Some(self.constant(Const::Int(i128::from(length)), ty, span))
            }
            Answer::Compare => {
                let left = self.literal(written[0])?;
                let right = self.literal(written[1])?;
                let order = compare(&left, &right);
                let ty = self.int();
                Some(self.constant(Const::Int(i128::from(order)), ty, span))
            }
        }
    }

    /// One floating constant, of the type the name says.
    fn folded_float(&mut self, kind: FloatKind, value: Float, span: Span) -> ExprId {
        let ty = self.types.float(kind);
        self.constant(Const::Float(value), ty, span)
    }

    /// The bytes of an argument that is written as a narrow string literal, terminator included.
    ///
    /// Nothing else counts. A wide literal is the wrong type for every name in the table, and an
    /// expression that merely folds to the address of a literal is not what gcc asks for either:
    /// `__builtin_strlen(p)` for a `const char *p = "abc";` is a call there and is a call here.
    fn literal(&self, arg: ast::ExprId) -> Option<Vec<u8>> {
        let ast::Expr::Str(id) = self.ast[arg] else { return None };
        let literal = &self.ast[id];
        matches!(literal.encoding, Encoding::Plain | Encoding::Utf8)
            .then(|| literal.bytes(self.cx.target))
    }
}

/// How long a string is, which is where its first zero byte is.
///
/// A literal may have one written inside it, and `__builtin_strlen("a\0b")` is one for the same
/// reason `strlen` of it would be.
fn length(bytes: &[u8]) -> u64 {
    bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len()) as u64
}

/// How two strings are ordered, in the way `strcmp` orders them.
///
/// The comparison is on `unsigned char`, C 7.24.4p1, whatever the sign of a plain `char` is on
/// the target. That is what makes `strcmp("X", "X\376")` negative: the second string has a two
/// hundred and fifty four where the first has its terminator.
///
/// The answer is minus one, zero or one. C promises only the sign, and those are the three values
/// gcc folds to.
fn compare(left: &[u8], right: &[u8]) -> i32 {
    for (left, right) in left.iter().zip(right) {
        if left != right {
            return if left < right { -1 } else { 1 };
        }
        // Both are the terminator, so neither string goes any further whatever is written after
        // it in the literal.
        if *left == 0 {
            return 0;
        }
    }
    0
}

/// The payload a nan is written with, or nothing when the string is not one.
///
/// The string is read the way `strtol` reads one with a base of zero, which is what gcc does with
/// it: leading space, then an optional sign, then a base taken from the prefix, and every
/// character after that has to be a digit of that base or the whole thing is not a payload. An
/// empty string is a payload of zero, which is the plain quiet nan and is what `math.h` writes
/// `NAN` as.
///
/// A value too large for the format is cut down to size rather than refused, which is
/// [`Float::nan_with`]'s doing and is gcc's answer as well.
fn payload(bytes: &[u8]) -> Option<u128> {
    // The string as the library function would see it, which stops at the first zero however
    // many bytes the literal has after it.
    let text = std::str::from_utf8(&bytes[..length(bytes) as usize]).ok()?;
    let text = text.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    // The sign is read and thrown away. A payload is a bit pattern and has no sign, and gcc
    // answers `__builtin_nan("-1")` and `__builtin_nan("1")` with the same number.
    let text = text.strip_prefix(['+', '-']).unwrap_or(text);
    if text.is_empty() {
        return Some(0);
    }
    let (radix, digits) = match text.as_bytes() {
        [b'0', b'x' | b'X', rest @ ..] => (16, rest),
        // A lone zero is a zero in any base, which is what makes this reachable with nothing
        // left to read.
        [b'0', rest @ ..] => (8, rest),
        rest => (10, rest),
    };
    if digits.is_empty() {
        return Some(0);
    }
    let mut value: u128 = 0;
    for &byte in digits {
        let digit = char::from(byte).to_digit(radix)?;
        value = value.wrapping_mul(u128::from(radix)).wrapping_add(u128::from(digit));
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every name here has to be a row of the roster, or it is a builtin `__has_builtin` has
    /// never heard of and a header that asks takes its fallback path for no reason.
    #[test]
    fn every_name_answered_here_is_a_row_of_the_table_and_says_it_is_done() {
        for (name, _) in TABLE {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            // These have a type, unlike the families that take whatever they are handed, because
            // the call that does not fold is a call and has to be checked against something.
            assert!(!feature.signature.is_empty(), "{name} is called when it does not fold");
        }
    }

    /// The names are what the lookup searches, so a name written twice is a second row nothing
    /// can ever reach.
    #[test]
    fn no_name_is_in_the_table_twice() {
        let mut names: Vec<&str> = TABLE.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        let all = names.len();
        names.dedup();
        assert_eq!(names.len(), all, "a name is in the table twice");
    }

    /// A string as the table's own code sees one, with the terminator the object would have.
    fn text(spelled: &str) -> Vec<u8> {
        let mut bytes = spelled.as_bytes().to_vec();
        bytes.push(0);
        bytes
    }

    /// Each of these is what gcc 16 answers for the same string.
    #[test]
    fn a_payload_is_read_the_way_strtol_reads_a_number() {
        for (spelled, value) in [
            ("", 0),
            ("0", 0),
            ("1", 1),
            ("0x1", 1),
            ("0X1", 1),
            ("010", 8),
            ("0xff", 255),
            ("8", 8),
            ("-1", 1),
            ("+2", 2),
            (" 1", 1),
            ("\t 0x10", 16),
            // A prefix with nothing after it, which gcc reads as the zero it starts with.
            ("0x", 0),
        ] {
            assert_eq!(payload(&text(spelled)), Some(value), "for {spelled:?}");
        }
        // Every one of these is a string gcc leaves as a call rather than answering, which is
        // the same set: a payload has to be the whole of what is written.
        for spelled in ["abc", "1x", "1 ", "08", "0x1g", "0x1p3", "--1"] {
            assert_eq!(payload(&text(spelled)), None, "for {spelled:?}");
        }
        // A literal with a zero written inside it stops there, the way the library would.
        assert_eq!(payload(b"1\x002\0"), Some(1));
    }

    #[test]
    fn a_length_stops_at_the_first_zero_and_an_order_is_over_unsigned_bytes() {
        assert_eq!(length(&text("")), 0);
        assert_eq!(length(&text("abc")), 3);
        assert_eq!(length(b"a\0bc\0"), 1);
        assert_eq!(compare(&text("X"), &text("X")), 0);
        assert_eq!(compare(&text("a"), &text("b")), -1);
        assert_eq!(compare(&text("b"), &text("a")), 1);
        assert_eq!(compare(&text("a"), &text("ab")), -1);
        // The byte is two hundred and fifty four and not minus two, which is the whole of what
        // `execute/921007-1.c` in the torture suite is about.
        assert_eq!(compare(&text("X"), b"X\xfe\0"), -1);
        // Nothing past the terminator is part of either string.
        assert_eq!(compare(b"a\0x\0", b"a\0y\0"), 0);
    }
}
