//! Integer constants: the value, and the type the standard's table walk gives it.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.1.
//!
//! This is the second piece of phase 7. A preprocessing number is a loose thing, deliberately
//! looser than a constant, so `1.2.3` and `0x1p+3` are both one pp-token and only here does
//! anyone ask what they mean. What comes back is a value and a type, and both of them are
//! places a compiler quietly goes wrong.
//!
//! The value is accumulated in a `u128` with every step checked, so a constant too large to
//! represent is a diagnostic rather than a number the program did not write. gcc 13.3 does not
//! do that: its accumulator is sixty four bits, and `18446744073709551616` compiles to zero of
//! type `int` after a warning nobody reads. That is not a behaviour worth reproducing, so ours
//! is the only measured difference here that is deliberate: past a hundred and twenty eight
//! bits the constant is refused. clang refuses it too, one bit earlier.
//!
//! The type is the standard's table walk, 6.4.4.1p5: a candidate list chosen by the base and
//! the suffix, walked in order, and the first type that holds the value wins. The list is not
//! the same in every dialect. C89 puts `unsigned long` in the list for a decimal constant with
//! no suffix, which is what makes `18446744073709551615` an `unsigned long` under `-std=c89`
//! and something wider under `-std=c99`, and gcc says so in as many words: "this decimal
//! constant is unsigned only in ISO C90". Both compilers keep `long long` out of the C89 lists
//! and accept it when the suffix asks for it.
//!
//! `__int128` is on the end of every list, which is what gcc does and clang does not.
//! `9223372036854775808` is an `__int128` in gcc 13.3 and an `unsigned long long` in clang,
//! and the difference is visible to a program: negate it and gcc gives a negative number.
//! We follow gcc, because the alternative silently turns a signed constant unsigned.
//!
//! The rest was measured the same way, by writing the constant and asking `_Generic` what it
//! is, on gcc 13.3 on x86-64 Linux and on clang:
//!
//! The suffix letters may be in either case but not both, so `1ll` and `1LL` are constants and
//! `1lL` is not, and the same rule holds for `wb`. The unsigned suffix may come before or after
//! the length suffix. `wb` does not combine with `l` at all.
//!
//! Binary constants are accepted in every dialect by both compilers, as an extension before
//! C23. Digit separators are C23 only in both. `_BitInt` constants are C23 in the standard,
//! clang accepts them in every dialect, and gcc 13.3 has no `_BitInt` at all.
//!
//! A `wb` constant has the narrowest type that holds it, which for a signed one includes the
//! sign bit and is never less than two: `1wb` is `_BitInt(2)`, `42wb` is `_BitInt(7)`, `255uwb`
//! is `unsigned _BitInt(8)` and `0uwb` is `unsigned _BitInt(1)`. Measured against clang, since
//! gcc 13.3 cannot say.

use rucc_session::Std;
use rucc_target::TargetInfo;
use rucc_types::{IntKind, int_width};

/// A converted integer constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntConstant {
    /// The value, which is never negative: a minus sign is an operator and not part of the
    /// constant, which is why `-2147483648` is a `long` on a 32-bit `int` and the reason
    /// `INT_MIN` is spelled the way it is in `limits.h`.
    pub value: u128,
    /// The type the table walk arrived at.
    pub ty: IntConstantType,
    /// What is worth saying about the constant, for the caller that holds the span.
    pub remarks: Remarks,
}

/// The type of an integer constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntConstantType {
    /// One of the integer kinds, chosen by the table walk.
    Standard(IntKind),
    /// A `_BitInt` of exactly the width it takes to hold the value.
    BitInt {
        /// Whether the `u` suffix was there.
        signed: bool,
        /// The width in bits, including the sign bit when there is one.
        width: u32,
    },
}

/// Why a preprocessing number is not an integer constant.
///
/// [`IntError::Floating`] is not a diagnostic. It means the spelling belongs to the floating
/// path, and it is an error here so that the caller cannot forget to ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntError {
    /// This is a floating constant. Nothing is wrong with it.
    Floating,
    /// The characters after the digits are not a suffix.
    InvalidSuffix,
    /// An `8` or a `9` in a constant that started with `0`.
    InvalidOctalDigit,
    /// `0x` or `0b` with no digits after it.
    NoDigits,
    /// Larger than any integer type, or than the hundred and twenty eight bits the value is
    /// accumulated in.
    TooLarge,
}

impl IntError {
    /// What to print, in GCC's words where GCC has any.
    ///
    /// The offending character is not in the message, because the caller has the spelling and
    /// the span and can say `invalid suffix "ux" on integer constant` the way GCC does.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            IntError::Floating => "not an integer constant",
            IntError::InvalidSuffix => "invalid suffix on integer constant",
            IntError::InvalidOctalDigit => "invalid digit in octal constant",
            IntError::NoDigits => "no digits in integer constant",
            IntError::TooLarge => "integer constant is too large to be represented in any type",
        }
    }
}

/// What a constant does that the dialect being compiled has an opinion about.
///
/// A bitmask rather than a list, because a constant may earn several and a `Vec` per constant
/// on a file full of them is a cost with nothing to show for it. Every one of these is legal
/// in the dialect this compiler defaults to, so none of them is an error here: the caller
/// decides what `-pedantic` and `-Werror` make of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Remarks(u8);

impl Remarks {
    /// Nothing to say.
    pub const NONE: Remarks = Remarks(0);
    /// A `0b` constant before C23, where it is a GNU extension both compilers accept.
    pub const BINARY: Remarks = Remarks(1);
    /// A digit separator before C23, which neither compiler accepts there.
    pub const SEPARATORS: Remarks = Remarks(2);
    /// A `wb` suffix before C23.
    pub const BIT_INT: Remarks = Remarks(4);
    /// An `ll` suffix under `-std=c89`, where GCC says "use of C99 long long integer constant".
    pub const LONG_LONG: Remarks = Remarks(8);
    /// A decimal constant with no `u` suffix that fits no signed type, so it became an unsigned
    /// one. GCC says "integer constant is so large that it is unsigned", and it is worth saying
    /// because the constant's arithmetic is now unsigned and its negation is not negative.
    pub const UNSIGNED: Remarks = Remarks(16);

    /// Whether every remark in `other` is set here.
    #[inline]
    #[must_use]
    pub const fn has(self, other: Remarks) -> bool {
        self.0 & other.0 == other.0
    }

    /// This set with `other` added.
    #[inline]
    #[must_use]
    pub const fn with(self, other: Remarks) -> Remarks {
        Remarks(self.0 | other.0)
    }

    /// Whether there is nothing to say.
    #[inline]
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// Converts the spelling of a preprocessing number into an integer constant.
///
/// # Errors
///
/// [`IntError`], one case of which is that the spelling is a floating constant rather than a
/// malformed integer one.
pub fn integer(text: &str, std: Std, target: &TargetInfo) -> Result<IntConstant, IntError> {
    let bytes = text.as_bytes();
    let (base, start) = base_of(bytes);
    if floating(bytes, base) {
        return Err(IntError::Floating);
    }
    let mut remarks = Remarks::NONE;
    if base == 2 && std < Std::C23 {
        remarks = remarks.with(Remarks::BINARY);
    }

    let mut value: u128 = 0;
    let mut digits = 0;
    let mut index = start;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\'' {
            // A separator is only a separator between two digits. The scanner keeps one in the
            // number only when an identifier character follows, so a trailing one arrives here
            // as a suffix instead and is refused as one.
            if digits == 0 || index + 1 >= bytes.len() || digit(bytes[index + 1], base).is_none() {
                return Err(IntError::InvalidSuffix);
            }
            if std < Std::C23 {
                remarks = remarks.with(Remarks::SEPARATORS);
            }
            index += 1;
            continue;
        }
        let Some(digit) = digit(byte, base) else {
            break;
        };
        value = value
            .checked_mul(u128::from(base))
            .and_then(|shifted| shifted.checked_add(u128::from(digit)))
            .ok_or(IntError::TooLarge)?;
        digits += 1;
        index += 1;
    }
    if digits == 0 {
        // `0x` with nothing after it, which GCC reports as an invalid suffix because it read
        // the `0` as the constant. The distinction is not worth a worse message than this.
        return Err(IntError::NoDigits);
    }
    if base == 8 && bytes[start..index].iter().any(|&byte| byte == b'8' || byte == b'9') {
        return Err(IntError::InvalidOctalDigit);
    }

    let suffix = suffix_of(&bytes[index..])?;
    if suffix.length == Some(Length::LongLong) && std == Std::C89 {
        remarks = remarks.with(Remarks::LONG_LONG);
    }
    if suffix.length == Some(Length::BitInt) {
        if std < Std::C23 {
            remarks = remarks.with(Remarks::BIT_INT);
        }
        return Ok(IntConstant { value, ty: bit_int(value, suffix.unsigned), remarks });
    }

    let candidates = candidates(base, suffix, std);
    let kind = candidates
        .iter()
        .copied()
        .find(|&kind| fits(value, kind, target))
        .ok_or(IntError::TooLarge)?;
    if base == 10 && !suffix.unsigned && !signed_standard(kind) {
        remarks = remarks.with(Remarks::UNSIGNED);
    }
    Ok(IntConstant { value, ty: IntConstantType::Standard(kind), remarks })
}

/// The base a spelling is written in, and where its digits start.
///
/// A leading `0` means octal only when a digit follows, so `0u` is a decimal zero with a
/// suffix and `08` is an octal constant with a digit that does not exist. That is the split
/// GCC makes, and it is what turns `08` into a message about octal rather than about a suffix.
fn base_of(bytes: &[u8]) -> (u32, usize) {
    match bytes {
        [b'0', b'x' | b'X', ..] => (16, 2),
        [b'0', b'b' | b'B', ..] => (2, 2),
        [b'0', next, ..] if next.is_ascii_digit() => (8, 1),
        _ => (10, 0),
    }
}

/// Whether the spelling is a floating constant rather than an integer one.
///
/// A point anywhere, an `e` exponent in a decimal constant, or a `p` exponent in a hexadecimal
/// one. `1e` and `1e+` are floating constants with no exponent digits, which is a diagnostic
/// the floating path gives, and `1f` is an integer constant with a suffix that does not exist,
/// which is one this path gives. Both compilers split them exactly there.
///
/// A leading zero does not survive an exponent: `08e5` is the floating constant eight hundred
/// thousand and not an octal constant with a digit that does not exist.
fn floating(bytes: &[u8], base: u32) -> bool {
    let exponent = if base == 16 { *b"pP" } else { *b"eE" };
    bytes.iter().any(|&byte| byte == b'.' || exponent.contains(&byte))
}

/// The value of a digit in the given base, and [`None`] when the byte is not one.
///
/// An octal constant reads `8` and `9` as digits, so that a constant holding one ends at the
/// suffix and the error can name the digit rather than complain about the suffix.
fn digit(byte: u8, base: u32) -> Option<u32> {
    char::from(byte).to_digit(if base == 8 { 10 } else { base })
}

/// The length part of a suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Length {
    /// `l` or `L`.
    Long,
    /// `ll` or `LL`.
    LongLong,
    /// `wb` or `WB`.
    BitInt,
}

/// A parsed suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Suffix {
    /// Whether `u` or `U` was there.
    unsigned: bool,
    /// The length part, when there was one.
    length: Option<Length>,
}

/// Reads the suffix, which may hold each part once and in either order.
fn suffix_of(mut rest: &[u8]) -> Result<Suffix, IntError> {
    let mut suffix = Suffix { unsigned: false, length: None };
    while let Some(&byte) = rest.first() {
        let taken = match byte {
            b'u' | b'U' if !suffix.unsigned => {
                suffix.unsigned = true;
                1
            }
            // The two letters have to agree about case, so `1ll` and `1LL` are constants and
            // `1lL` is not. Both compilers refuse the mixed spelling in every dialect.
            b'l' | b'L' if suffix.length.is_none() => {
                if rest.get(1) == Some(&byte) {
                    suffix.length = Some(Length::LongLong);
                    2
                } else {
                    suffix.length = Some(Length::Long);
                    1
                }
            }
            b'w' | b'W' if suffix.length.is_none() => {
                let second = if byte == b'w' { b'b' } else { b'B' };
                if rest.get(1) != Some(&second) {
                    return Err(IntError::InvalidSuffix);
                }
                suffix.length = Some(Length::BitInt);
                2
            }
            _ => return Err(IntError::InvalidSuffix),
        };
        rest = &rest[taken..];
    }
    Ok(suffix)
}

/// The type of a `wb` constant, which is the narrowest one that holds the value.
///
/// The sign bit counts, so a signed one is never narrower than two bits: `1wb` is
/// `_BitInt(2)`. An unsigned zero is `unsigned _BitInt(1)`, because a width of zero is not a
/// type. Measured against clang.
fn bit_int(value: u128, unsigned: bool) -> IntConstantType {
    let used = 128 - value.leading_zeros();
    let width = if unsigned { used.max(1) } else { used + 1 };
    IntConstantType::BitInt { signed: !unsigned, width: width.max(if unsigned { 1 } else { 2 }) }
}

/// Whether `kind` is one of the standard signed types, which is what decides the remark about
/// a decimal constant having gone unsigned.
fn signed_standard(kind: IntKind) -> bool {
    matches!(kind, IntKind::Int | IntKind::Long | IntKind::LongLong)
}

/// Whether the value fits in `kind` on this target.
fn fits(value: u128, kind: IntKind, target: &TargetInfo) -> bool {
    let width = int_width(kind, target);
    // Signedness here never depends on what plain `char` is, because no candidate list holds a
    // character type.
    let bits = if kind.is_signed(false) { width - 1 } else { width };
    // `unsigned __int128` holds every value the accumulator can, and shifting a `u128` by all
    // of its bits is not a shift, so the widest type is answered without one.
    bits >= 128 || value >> bits == 0
}

/// The candidate list for a base and a suffix, in the order the standard walks it.
///
/// `__int128` and `unsigned __int128` are on the end of every list, which is what gcc does:
/// `9223372036854775808` is an `__int128` there and an `unsigned long long` in clang. Both
/// compilers put `long long` out of reach in C89 unless the suffix asks for it, and C89 is
/// also the dialect that offers `unsigned long` for a decimal constant with no suffix at all.
fn candidates(base: u32, suffix: Suffix, std: Std) -> &'static [IntKind] {
    use IntKind::{Int, Int128, Long, LongLong, UInt, UInt128, ULong, ULongLong};

    let decimal = base == 10;
    let c89 = std == Std::C89;
    match (suffix.unsigned, suffix.length) {
        (false, None) if decimal && c89 => &[Int, Long, ULong, Int128, UInt128],
        (false, None) if decimal => &[Int, Long, LongLong, Int128],
        (false, None) if c89 => &[Int, UInt, Long, ULong, Int128, UInt128],
        (false, None) => &[Int, UInt, Long, ULong, LongLong, ULongLong, Int128, UInt128],

        (true, None) if c89 => &[UInt, ULong, UInt128],
        (true, None) => &[UInt, ULong, ULongLong, UInt128],

        (false, Some(Length::Long)) if decimal && c89 => &[Long, ULong, Int128, UInt128],
        (false, Some(Length::Long)) if decimal => &[Long, LongLong, Int128],
        (false, Some(Length::Long)) if c89 => &[Long, ULong, Int128, UInt128],
        (false, Some(Length::Long)) => &[Long, ULong, LongLong, ULongLong, Int128, UInt128],

        (true, Some(Length::Long)) if c89 => &[ULong, UInt128],
        (true, Some(Length::Long)) => &[ULong, ULongLong, UInt128],

        (false, Some(Length::LongLong)) if decimal => &[LongLong, Int128],
        (false, Some(Length::LongLong)) => &[LongLong, ULongLong, Int128, UInt128],
        (true, Some(Length::LongLong)) => &[ULongLong, UInt128],

        // A `wb` constant never reaches here: its type comes from the value alone.
        (_, Some(Length::BitInt)) => &[],
    }
}

#[cfg(test)]
mod tests {
    use rucc_target::Triple;

    use super::*;

    fn linux() -> TargetInfo {
        TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a known triple"))
    }

    /// The value and the type of a constant in the default dialect.
    fn c23(text: &str) -> Result<IntConstant, IntError> {
        integer(text, Std::C23, &linux())
    }

    /// The type of a constant in the given dialect, on x86-64 Linux.
    fn kind(text: &str, std: Std) -> IntKind {
        match integer(text, std, &linux()).expect("a valid constant").ty {
            IntConstantType::Standard(kind) => kind,
            IntConstantType::BitInt { .. } => panic!("{text} is a _BitInt constant"),
        }
    }

    #[test]
    fn a_constant_in_each_base_has_the_value_it_says() {
        assert_eq!(c23("0").expect("zero").value, 0);
        assert_eq!(c23("42").expect("decimal").value, 42);
        assert_eq!(c23("0777").expect("octal").value, 0o777);
        assert_eq!(c23("0xdeadBEEF").expect("hex").value, 0xdead_beef);
        assert_eq!(c23("0b1010").expect("binary").value, 0b1010);
        assert_eq!(c23("0X10").expect("upper case prefix").value, 16);
        // A leading zero with nothing after it is a decimal zero rather than an octal one with
        // no digits, which is the split that lets `0u` through and stops `08`.
        assert_eq!(c23("0u").expect("zero with a suffix").value, 0);
    }

    #[test]
    fn digit_separators_are_stripped_and_reported_before_c23() {
        let value = c23("1'000'000").expect("a C23 constant");
        assert_eq!(value.value, 1_000_000);
        assert!(value.remarks.is_none());
        assert_eq!(c23("0x1'0").expect("hex with a separator").value, 16);

        let older = integer("1'000", Std::C17, &linux()).expect("still converted");
        assert!(older.remarks.has(Remarks::SEPARATORS));
        assert_eq!(older.value, 1000);
    }

    #[test]
    fn the_type_of_a_decimal_constant_walks_the_signed_types_only() {
        // Measured with `_Generic` on gcc 13.3, x86-64 Linux.
        assert_eq!(kind("2147483647", Std::C23), IntKind::Int);
        assert_eq!(kind("2147483648", Std::C23), IntKind::Long);
        assert_eq!(kind("4294967295", Std::C23), IntKind::Long);
        assert_eq!(kind("9223372036854775807", Std::C23), IntKind::Long);
        // Past `long long` gcc reaches for `__int128` rather than for an unsigned type, and
        // says so: the constant is so large that it is unsigned.
        assert_eq!(kind("9223372036854775808", Std::C23), IntKind::Int128);
        assert_eq!(kind("18446744073709551615", Std::C23), IntKind::Int128);
        let large = c23("18446744073709551615").expect("fits __int128");
        assert!(large.remarks.has(Remarks::UNSIGNED));
    }

    #[test]
    fn a_constant_in_another_base_may_be_unsigned_without_saying_so() {
        // This is the split that surprises people: `4294967295` is a `long` and `0xffffffff`
        // is an `unsigned int`, because only the decimal list is signed types alone.
        assert_eq!(kind("0xffffffff", Std::C23), IntKind::UInt);
        assert_eq!(kind("0x7fffffff", Std::C23), IntKind::Int);
        assert_eq!(kind("0x80000000", Std::C23), IntKind::UInt);
        assert_eq!(kind("0x100000000", Std::C23), IntKind::Long);
        assert_eq!(kind("0xffffffffffffffff", Std::C23), IntKind::ULong);
        assert_eq!(kind("0777", Std::C23), IntKind::Int);
        assert_eq!(kind("0b1010", Std::C23), IntKind::Int);
        // And no remark, because nothing about it is surprising enough to say.
        assert!(c23("0xffffffff").expect("a constant").remarks.is_none());
    }

    #[test]
    fn c89_has_unsigned_long_in_the_decimal_list_and_no_long_long_in_any() {
        // gcc under `-std=c89 -pedantic`: "this decimal constant is unsigned only in ISO C90",
        // and eight bytes rather than sixteen.
        assert_eq!(kind("18446744073709551615", Std::C89), IntKind::ULong);
        assert_eq!(kind("18446744073709551615", Std::C99), IntKind::Int128);
        let old = integer("18446744073709551615", Std::C89, &linux()).expect("a C89 constant");
        assert!(old.remarks.has(Remarks::UNSIGNED));
        // The suffix still reaches `long long`, with the remark gcc prints for it.
        let long_long = integer("1ll", Std::C89, &linux()).expect("an extension");
        assert!(long_long.remarks.has(Remarks::LONG_LONG));
        assert_eq!(kind("1ll", Std::C89), IntKind::LongLong);
        assert!(integer("1ll", Std::C99, &linux()).expect("standard").remarks.is_none());
    }

    #[test]
    fn a_suffix_narrows_the_list_it_does_not_pick_the_type() {
        assert_eq!(kind("1u", Std::C23), IntKind::UInt);
        assert_eq!(kind("1l", Std::C23), IntKind::Long);
        assert_eq!(kind("1ul", Std::C23), IntKind::ULong);
        assert_eq!(kind("1ll", Std::C23), IntKind::LongLong);
        assert_eq!(kind("1llu", Std::C23), IntKind::ULongLong);
        // The suffix is a floor rather than an answer: `4294967296u` is an `unsigned long`
        // because `unsigned int` cannot hold it.
        assert_eq!(kind("4294967296u", Std::C23), IntKind::ULong);
        assert_eq!(kind("0xffffffffu", Std::C23), IntKind::UInt);
    }

    #[test]
    fn the_letters_of_a_suffix_may_be_in_either_case_but_not_both() {
        for text in ["1u", "1U", "1l", "1L", "1ll", "1LL", "1ul", "1lu", "1uL", "1LLU", "1llu"] {
            assert!(c23(text).is_ok(), "{text} is a constant in both compilers");
        }
        for text in ["1lL", "1Ll", "1uu", "1lul", "1z", "1uz", "1f", "1x", "1_000"] {
            assert_eq!(c23(text), Err(IntError::InvalidSuffix), "{text} is not");
        }
    }

    #[test]
    fn a_bit_int_constant_has_the_narrowest_type_that_holds_it() {
        // Measured against clang, which is the only one of the two that has the type.
        let cases = [
            ("0wb", true, 2),
            ("1wb", true, 2),
            ("3wb", true, 3),
            ("42wb", true, 7),
            ("255wb", true, 9),
            ("0uwb", false, 1),
            ("1uwb", false, 1),
            ("255uwb", false, 8),
            ("256uwb", false, 9),
            ("0xffffffffffffffffuwb", false, 64),
        ];
        for (text, signed, width) in cases {
            let constant = c23(text).expect("a _BitInt constant");
            assert_eq!(
                constant.ty,
                IntConstantType::BitInt { signed, width },
                "{text} is the wrong width"
            );
        }
        // Either order, either case, and never with a length suffix.
        for text in ["1uwb", "1wbu", "1UWB", "1WBu", "1uWB"] {
            assert!(c23(text).is_ok(), "{text} is a constant in clang");
        }
        for text in ["1wB", "1Wb", "1lwb", "1wbl", "1wbwb"] {
            assert_eq!(c23(text), Err(IntError::InvalidSuffix), "{text} is not");
        }
        // Before C23 it is still converted, and still worth a word.
        let older = integer("1wb", Std::C17, &linux()).expect("clang accepts it everywhere");
        assert!(older.remarks.has(Remarks::BIT_INT));
    }

    #[test]
    fn a_binary_constant_is_an_extension_before_c23() {
        assert!(c23("0b1").expect("standard in C23").remarks.is_none());
        let older = integer("0b1", Std::C17, &linux()).expect("both compilers accept it");
        assert!(older.remarks.has(Remarks::BINARY));
    }

    #[test]
    fn an_octal_constant_names_the_digit_that_is_not_one() {
        assert_eq!(c23("08"), Err(IntError::InvalidOctalDigit));
        assert_eq!(c23("0778"), Err(IntError::InvalidOctalDigit));
        assert_eq!(c23("09"), Err(IntError::InvalidOctalDigit));
        // A `9` elsewhere is fine, and the message is only for constants that began with `0`.
        assert_eq!(c23("9").expect("decimal").value, 9);
    }

    #[test]
    fn a_prefix_with_no_digits_after_it_is_not_a_constant() {
        assert_eq!(c23("0x"), Err(IntError::NoDigits));
        assert_eq!(c23("0b"), Err(IntError::NoDigits));
    }

    #[test]
    fn a_constant_larger_than_any_type_is_refused_rather_than_wrapped() {
        // gcc accumulates in sixty four bits and silently gives this the value zero and the
        // type `int` after a warning. That is the one measured behaviour here we refuse to
        // reproduce, and clang refuses it too.
        assert_eq!(c23("340282366920938463463374607431768211456"), Err(IntError::TooLarge));
        assert_eq!(c23("0x100000000000000000000000000000000"), Err(IntError::TooLarge));
        // 2^127 fits in the accumulator and in no signed type, and the decimal list has no
        // unsigned one to fall back to.
        assert_eq!(c23("170141183460469231731687303715884105728"), Err(IntError::TooLarge));
        // The same value written in hex reaches `unsigned __int128`, because that list has it.
        assert_eq!(kind("0x80000000000000000000000000000000", Std::C23), IntKind::UInt128);
        assert_eq!(kind("0xffffffffffffffffffffffffffffffff", Std::C23), IntKind::UInt128);
    }

    #[test]
    fn a_floating_constant_is_handed_back_rather_than_refused() {
        for text in ["1.0", ".5", "1.", "1e5", "1E-5", "1e", "0x1p3", "0x1.8p+1", "1.5e3"] {
            assert_eq!(c23(text), Err(IntError::Floating), "{text} belongs to the other path");
        }
        // A leading zero does not make this an octal constant with a digit that does not
        // exist. gcc compiles it, as eight hundred thousand.
        assert_eq!(c23("08e5"), Err(IntError::Floating));
        // A hexadecimal `e` is a digit, not an exponent, and `1f` is an integer with a suffix
        // that does not exist rather than a float. Both compilers split them there.
        assert_eq!(c23("0xe5").expect("hex digits").value, 0xe5);
        assert_eq!(c23("1f"), Err(IntError::InvalidSuffix));
    }

    #[test]
    fn the_type_comes_from_the_target_and_not_from_the_host() {
        // `4294967295` is a `long` where `long` is sixty four bits and a `long long` where it
        // is thirty two. A compiler that asked its own platform gets one of these wrong.
        let windows =
            TargetInfo::new("x86_64-pc-windows-msvc".parse::<Triple>().expect("a known triple"));
        let on_windows = integer("4294967295", Std::C23, &windows).expect("a constant");
        assert_eq!(on_windows.ty, IntConstantType::Standard(IntKind::LongLong));
        assert_eq!(kind("4294967295", Std::C23), IntKind::Long);
    }
}
