//! Numeric constants: the value, and the type the standard's table walk gives it.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.1.
//!
//! This is the second piece of phase 7. A preprocessing number is a loose thing, deliberately
//! looser than a constant, so `1.2.3` and `0x1p+3` are both one pp-token and only here does
//! anyone ask what they mean. What comes back is a value and a type, and both of them are
//! places a compiler quietly goes wrong.
//!
//! There are two entry points, [`integer`] and [`floating`], and either of them hands the
//! spelling to the other rather than reporting an error when it turns out to belong there. The
//! split is not where a reader expects it: `1e` is a floating constant with no exponent digits
//! and `1f` is an integer constant with a suffix that does not exist, and both compilers agree
//! on that, because the exponent marker is part of a preprocessing number and the suffix letter
//! is not part of anything.
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
//!
//! # Floating constants
//!
//! A floating constant has none of the table walk about it: the suffix names the type outright,
//! and with no suffix it is a `double`. What it has instead is a list of suffixes that is much
//! longer than the standard's three, and a conversion that has to be exactly right.
//!
//! The conversion is [`rucc_base::float`], correctly rounded and done in software, so that the
//! bits do not depend on the machine the compiler runs on. The type decides the format and the
//! target decides what some of the types are: `long double` is the x87 eighty bit format on
//! x86-64 Linux and true quad precision on AArch64 Linux, and both of them say 128 bits wide,
//! which is why [`TargetInfo::long_double_format`] exists.
//!
//! The suffixes were measured on gcc 13.3 on x86-64 Linux, with `_Generic` for the type and by
//! printing the bytes for the format. `f` is `float` and `l` is `long double`, and then the
//! extensions: `q` is `__float128`, `w` is the x87 `__float80`, `d` is a `double` written the
//! long way, `f16` `f32` `f64` `f128` are the `_FloatN` types and `f32x` `f64x` the `_FloatNx`
//! ones, and `i` or `j` in either position makes the constant imaginary. `_Float32x` turns out
//! to be plain `double` and `_Float64x` the x87 format, which is not what the names suggest:
//! `0.1f32x` is `0x3fb999999999999a` and `0.1f64x` is `0x3ffbcccccccccccccccd`.
//!
//! The case rules are their own small grammar. The `f` of a `_FloatN` suffix may be either case
//! and the trailing `x` may not, so `F64x` is a constant and `f64X` is not. A decimal float
//! suffix is two letters that have to agree, so `df` and `DF` are constants and `Df` is not.
//! Every one of these is accepted in every dialect, C89 included, and every one of them is
//! worth a remark when the dialect did not ask for it.
//!
//! Decimal floating constants are refused rather than converted, because there is no decimal
//! float value anywhere in this compiler to put one in. That is a gap and the error says so.

use rucc_base::float::{Float, Format, ParseError, Status};
use rucc_session::Std;
use rucc_target::{Arch, TargetInfo};
use rucc_types::{IntKind, int_width};

use crate::remarks::Remarks;

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
        /// Whether the type is signed, which it is when the `u` suffix was not there.
        signed: bool,
        /// The width in bits, including the sign bit when there is one.
        width: u32,
    },
}

impl IntConstantType {
    /// A suffix that names this type, for a printer putting the constant back.
    ///
    /// The value decides the rest. Nothing spells `__int128`, because no suffix does and none is
    /// needed: a constant only gets that type by being too large for every other one, so writing
    /// the value back gets the same answer out of the table walk.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            IntConstantType::Standard(kind) => match kind {
                IntKind::UInt | IntKind::UInt128 => "u",
                IntKind::Long => "l",
                IntKind::ULong => "ul",
                IntKind::LongLong => "ll",
                IntKind::ULongLong => "ull",
                _ => "",
            },
            IntConstantType::BitInt { signed: true, .. } => "wb",
            IntConstantType::BitInt { signed: false, .. } => "uwb",
        }
    }
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

/// A converted floating constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatConstant {
    /// The value, correctly rounded into the format of its type.
    pub value: Float,
    /// The type the suffix named, which is `double` when there was no suffix.
    pub ty: FloatConstantType,
    /// Whether an `i` or a `j` made this the imaginary part of a complex constant. The value is
    /// the real number that was written either way, so the caller builds the complex one.
    pub imaginary: bool,
    /// What is worth saying about the constant, for the caller that holds the span.
    pub remarks: Remarks,
}

/// The type of a floating constant.
///
/// This is not [`rucc_types::FloatKind`], which has the three types C has. The suffixes reach
/// further than that, and a constant knows exactly which type it was written as long before
/// anything has to decide what that type is on this target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatConstantType {
    /// `f` or `F`.
    Float,
    /// No suffix, or the `d` and `D` that GCC also accepts for it.
    Double,
    /// `l` or `L`.
    LongDouble,
    /// `f16` or `F16`, which is `_Float16`.
    Float16,
    /// `f32` or `F32`, which is `_Float32` and is the same format as `float`.
    Float32,
    /// `f64` or `F64`, which is `_Float64` and is the same format as `double`.
    Float64,
    /// `f128` or `F128`, and `q` or `Q`, which is `_Float128` and `__float128`. GCC keeps the
    /// two spellings as distinct types and they are the same format, which is all this says.
    Float128,
    /// `f32x` or `F32x`, which is `_Float32x`. The name suggests something wider than
    /// `_Float32` and on every target here it is exactly `double`.
    Float32x,
    /// `f64x` or `F64x`, which is `_Float64x`: the widest format the target has beyond
    /// `_Float64`, so the x87 one on x86 and quad precision elsewhere.
    Float64x,
    /// `w` or `W`, which is GCC's `__float80`. It is the x87 format whatever `long double` is,
    /// which is the reason it is not the same thing as [`FloatConstantType::LongDouble`], and
    /// GCC has it on x86 only.
    Float80,
}

impl FloatConstantType {
    /// The format a constant of this type is converted in.
    ///
    /// The two that depend on the target are the two that have to: `long double` is the x87
    /// format on x86-64 Linux and quad precision on AArch64 Linux, and `_Float64x` is whatever
    /// the target has above `_Float64`, which is the same split.
    #[must_use]
    pub fn format(self, target: &TargetInfo) -> Format {
        match self {
            FloatConstantType::Float | FloatConstantType::Float32 => Format::Single,
            FloatConstantType::Double
            | FloatConstantType::Float64
            | FloatConstantType::Float32x => Format::Double,
            FloatConstantType::LongDouble => target.long_double_format,
            FloatConstantType::Float16 => Format::Half,
            FloatConstantType::Float128 => Format::Quad,
            FloatConstantType::Float64x if target.triple.arch == Arch::X86_64 => {
                Format::X87Extended
            }
            FloatConstantType::Float64x => Format::Quad,
            FloatConstantType::Float80 => Format::X87Extended,
        }
    }

    /// The C spelling of the type, which a diagnostic naming it has to print.
    ///
    /// GCC says "floating constant exceeds range of 'double'" and puts the type in the message,
    /// so the type has to be able to say what it is called.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            FloatConstantType::Float => "float",
            FloatConstantType::Double => "double",
            FloatConstantType::LongDouble => "long double",
            FloatConstantType::Float16 => "_Float16",
            FloatConstantType::Float32 => "_Float32",
            FloatConstantType::Float64 => "_Float64",
            FloatConstantType::Float128 => "_Float128",
            FloatConstantType::Float32x => "_Float32x",
            FloatConstantType::Float64x => "_Float64x",
            FloatConstantType::Float80 => "__float80",
        }
    }

    /// The suffix that names this type, for a printer putting the constant back.
    ///
    /// One spelling per type rather than the one that was written, so `0.1q` and `0.1f128` both
    /// come back as `f128`. They are the same type, and the printer's job is to mean the same
    /// thing rather than to look the same.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            FloatConstantType::Float => "f",
            FloatConstantType::Double => "",
            FloatConstantType::LongDouble => "l",
            FloatConstantType::Float16 => "f16",
            FloatConstantType::Float32 => "f32",
            FloatConstantType::Float64 => "f64",
            FloatConstantType::Float128 => "f128",
            FloatConstantType::Float32x => "f32x",
            FloatConstantType::Float64x => "f64x",
            FloatConstantType::Float80 => "w",
        }
    }
}

/// Why a preprocessing number is not a floating constant.
///
/// [`FloatError::Integer`] is not a diagnostic, in the same way [`IntError::Floating`] is not:
/// it means the spelling belongs to the other path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatError {
    /// This is an integer constant. Nothing is wrong with it.
    Integer,
    /// The characters after the number are not a suffix.
    InvalidSuffix,
    /// A hexadecimal floating constant with no `p` exponent. The exponent is required there and
    /// not optional as it is in a decimal one, because `f` is a hexadecimal digit and there
    /// would be no way to tell a suffix from the number.
    MissingExponent,
    /// An `e` or a `p` with no digits after it.
    NoExponentDigits,
    /// A constant with a point and no digits at all.
    NoDigits,
    /// More than one point, which is a preprocessing number and not a constant.
    TooManyPoints,
    /// A `df`, `dd` or `dl` suffix. The constant is well formed and this compiler has nowhere
    /// to put a decimal floating value yet.
    DecimalFloat,
    /// A suffix naming a type this target does not have, which is `w` anywhere but x86 and
    /// `f128x` everywhere.
    UnsupportedType,
}

impl FloatError {
    /// What to print, in GCC's words where GCC has any.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            FloatError::Integer => "not a floating constant",
            FloatError::InvalidSuffix => "invalid suffix on floating constant",
            FloatError::MissingExponent => "hexadecimal floating constants require an exponent",
            FloatError::NoExponentDigits => "exponent has no digits",
            FloatError::NoDigits => "no digits in floating constant",
            FloatError::TooManyPoints => "too many decimal points in number",
            FloatError::DecimalFloat => "decimal floating constants are not supported yet",
            FloatError::UnsupportedType => {
                "the type of this floating constant is not supported on this target"
            }
        }
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
    if is_floating(bytes, base) {
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
fn is_floating(bytes: &[u8], base: u32) -> bool {
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

/// Converts the spelling of a preprocessing number into a floating constant.
///
/// # Errors
///
/// [`FloatError`], one case of which is that the spelling is an integer constant rather than a
/// malformed floating one.
pub fn floating(text: &str, std: Std, target: &TargetInfo) -> Result<FloatConstant, FloatError> {
    let bytes = text.as_bytes();
    let (base, _) = base_of(bytes);
    if !is_floating(bytes, base) {
        return Err(FloatError::Integer);
    }
    // A leading zero means nothing to a floating constant, so there are two bases here and not
    // four: `08e5` is eight hundred thousand rather than an octal constant with a bad digit.
    let hex = base == 16;
    let base = if hex { 16 } else { 10 };
    let mut remarks = Remarks::NONE;
    if hex && std < Std::C99 {
        remarks = remarks.with(Remarks::HEX_FLOAT);
    }

    let mut index = if hex { 2 } else { 0 };
    let mut digits = 0;
    let mut point = false;
    let mut separators = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\'' {
            if digits == 0 || !next_is_digit(bytes, index, base) {
                return Err(FloatError::InvalidSuffix);
            }
            separators = true;
        } else if byte == b'.' {
            if point {
                return Err(FloatError::TooManyPoints);
            }
            point = true;
        } else if digit(byte, base).is_some() {
            digits += 1;
        } else {
            break;
        }
        index += 1;
    }
    if digits == 0 {
        return Err(FloatError::NoDigits);
    }

    let marker = if hex { *b"pP" } else { *b"eE" };
    if index < bytes.len() && marker.contains(&bytes[index]) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let mut exponent_digits = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte == b'\'' {
                if exponent_digits == 0 || !next_is_digit(bytes, index, 10) {
                    return Err(FloatError::InvalidSuffix);
                }
                separators = true;
            } else if byte.is_ascii_digit() {
                exponent_digits += 1;
            } else {
                break;
            }
            index += 1;
        }
        if exponent_digits == 0 {
            return Err(FloatError::NoExponentDigits);
        }
    } else if hex {
        // The exponent is not optional in a hexadecimal constant, because `f` is a digit there
        // and `0x1.8f` would otherwise be a number and a suffix at the same time.
        return Err(FloatError::MissingExponent);
    }
    if separators && std < Std::C23 {
        remarks = remarks.with(Remarks::SEPARATORS);
    }

    let suffix = float_suffix(&bytes[index..], target)?;
    remarks = remarks.with(suffix.remarks);
    let (value, status) =
        Float::parse(&text[..index], suffix.ty.format(target)).map_err(|error| match error {
            // The scan above has already ruled all three of these out, and mapping them is
            // still better than an unwrap that a later change could reach.
            ParseError::NoDigits => FloatError::NoDigits,
            ParseError::NoExponentDigits => FloatError::NoExponentDigits,
            ParseError::Invalid => FloatError::InvalidSuffix,
        })?;
    if status.has(Status::OVERFLOW) {
        remarks = remarks.with(Remarks::OUT_OF_RANGE);
    }
    // Underflow on its own is a subnormal, which is a number the program can use. Losing the
    // value entirely is the part worth a word.
    if status.has(Status::UNDERFLOW) && value.is_zero() {
        remarks = remarks.with(Remarks::TRUNCATED);
    }
    Ok(FloatConstant { value, ty: suffix.ty, imaginary: suffix.imaginary, remarks })
}

/// Whether the byte after `index` is a digit in `base`, which is what makes a separator one.
fn next_is_digit(bytes: &[u8], index: usize, base: u32) -> bool {
    bytes.get(index + 1).is_some_and(|&next| digit(next, base).is_some())
}

/// A parsed floating suffix.
struct FloatSuffix {
    /// The type it named, which is `double` when it named none.
    ty: FloatConstantType,
    /// Whether it held an `i` or a `j`.
    imaginary: bool,
    /// What the suffix alone is worth saying about.
    remarks: Remarks,
}

/// Reads the suffix, which may name a type once and mark the constant imaginary once, in either
/// order.
///
/// Everything past `f` and `l` is an extension, and the extensions are where the case rules stop
/// being uniform: the `f` of `_FloatN` may be either case and the `x` of `_FloatNx` may not, and
/// the two letters of a decimal suffix have to agree. All of it measured on gcc 13.3.
fn float_suffix(mut rest: &[u8], target: &TargetInfo) -> Result<FloatSuffix, FloatError> {
    let mut ty = None;
    let mut imaginary = false;
    let mut remarks = Remarks::NONE;
    while let Some(&byte) = rest.first() {
        let taken = match byte {
            b'i' | b'j' | b'I' | b'J' if !imaginary => {
                imaginary = true;
                remarks = remarks.with(Remarks::IMAGINARY);
                1
            }
            // One type per constant, so `1.0fl` is not a constant and neither is `1.0ff`.
            _ if ty.is_some() => return Err(FloatError::InvalidSuffix),
            b'f' | b'F' => {
                let (named, taken, extra) = float_n(rest)?;
                ty = Some(named);
                remarks = remarks.with(extra);
                taken
            }
            b'l' | b'L' => {
                ty = Some(FloatConstantType::LongDouble);
                1
            }
            b'q' | b'Q' => {
                ty = Some(FloatConstantType::Float128);
                remarks = remarks.with(Remarks::EXTENDED_SUFFIX);
                1
            }
            b'w' | b'W' => {
                // `__float80` is the x87 format, which only x86 has.
                if target.triple.arch != Arch::X86_64 {
                    return Err(FloatError::UnsupportedType);
                }
                ty = Some(FloatConstantType::Float80);
                remarks = remarks.with(Remarks::EXTENDED_SUFFIX);
                1
            }
            b'd' | b'D' => {
                let second = rest.get(1).copied();
                let decimal = if byte == b'd' {
                    matches!(second, Some(b'f' | b'd' | b'l'))
                } else {
                    matches!(second, Some(b'F' | b'D' | b'L'))
                };
                if decimal {
                    return Err(FloatError::DecimalFloat);
                }
                ty = Some(FloatConstantType::Double);
                remarks = remarks.with(Remarks::DOUBLE_SUFFIX);
                1
            }
            _ => return Err(FloatError::InvalidSuffix),
        };
        rest = &rest[taken..];
    }
    Ok(FloatSuffix { ty: ty.unwrap_or(FloatConstantType::Double), imaginary, remarks })
}

/// Reads a suffix that starts with `f`, which is `float` on its own and one of the `_FloatN` or
/// `_FloatNx` types when digits follow.
///
/// Returns the type, how many bytes it took and what is worth saying about it.
fn float_n(rest: &[u8]) -> Result<(FloatConstantType, usize, Remarks), FloatError> {
    let mut end = 1;
    while rest.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end == 1 {
        return Ok((FloatConstantType::Float, 1, Remarks::NONE));
    }
    // The `x` is lower case in every spelling gcc accepts, so `F64x` is a constant and `f64X`
    // is not, however odd that looks next to the `F` being free.
    let extended = rest.get(end) == Some(&b'x');
    let ty = match (&rest[1..end], extended) {
        (b"16", false) => FloatConstantType::Float16,
        (b"32", false) => FloatConstantType::Float32,
        (b"64", false) => FloatConstantType::Float64,
        (b"128", false) => FloatConstantType::Float128,
        (b"32", true) => FloatConstantType::Float32x,
        (b"64", true) => FloatConstantType::Float64x,
        // gcc knows the name `_Float128x` and has the type on no target here, and says so
        // rather than calling the suffix invalid. `_Float16x` is not a type at all.
        (b"128", true) => return Err(FloatError::UnsupportedType),
        _ => return Err(FloatError::InvalidSuffix),
    };
    Ok((ty, end + usize::from(extended), Remarks::EXTENDED_SUFFIX))
}

#[cfg(test)]
mod tests {
    use rucc_target::Triple;

    use super::*;

    fn linux() -> TargetInfo {
        TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a known triple"))
    }

    fn aarch64() -> TargetInfo {
        TargetInfo::new("aarch64-unknown-linux-gnu".parse::<Triple>().expect("a known triple"))
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

    /// A floating constant in the default dialect, on x86-64 Linux.
    fn float(text: &str) -> Result<FloatConstant, FloatError> {
        floating(text, Std::C23, &linux())
    }

    /// The bits of a constant's value, which is the form every measured row here was taken in.
    fn bits(text: &str) -> u128 {
        float(text).expect("a valid constant").value.to_bits()
    }

    #[test]
    fn a_constant_with_no_suffix_is_a_double() {
        let constant = float("1.5").expect("a constant");
        assert_eq!(constant.ty, FloatConstantType::Double);
        assert!(!constant.imaginary);
        assert!(constant.remarks.is_none());
        assert_eq!(constant.value.to_bits(), 0x3ff8_0000_0000_0000);
        assert_eq!(bits("0.1"), 0x3fb9_9999_9999_999a);
        assert_eq!(bits(".5"), 0x3fe0_0000_0000_0000);
        assert_eq!(bits("1."), 0x3ff0_0000_0000_0000);
        assert_eq!(bits("1e5"), 0x40f8_6a00_0000_0000);
        assert_eq!(bits("0x1p3"), 0x4020_0000_0000_0000);
        // A leading zero is not an octal prefix once there is an exponent, so this is eight
        // hundred thousand and gcc compiles it as one.
        assert_eq!(bits("08e5"), 0x4128_6a00_0000_0000);
    }

    #[test]
    fn the_suffix_names_the_type_rather_than_narrowing_a_list() {
        // Measured with `_Generic` on gcc 13.3, x86-64 Linux.
        let cases = [
            ("1.0", FloatConstantType::Double),
            ("1.0f", FloatConstantType::Float),
            ("1.0F", FloatConstantType::Float),
            ("1.0l", FloatConstantType::LongDouble),
            ("1.0L", FloatConstantType::LongDouble),
            ("1.0d", FloatConstantType::Double),
            ("1.0q", FloatConstantType::Float128),
            ("1.0w", FloatConstantType::Float80),
            ("1.0f16", FloatConstantType::Float16),
            ("1.0F16", FloatConstantType::Float16),
            ("1.0f32", FloatConstantType::Float32),
            ("1.0f64", FloatConstantType::Float64),
            ("1.0f128", FloatConstantType::Float128),
            ("1.0f32x", FloatConstantType::Float32x),
            ("1.0F64x", FloatConstantType::Float64x),
        ];
        for (text, ty) in cases {
            assert_eq!(float(text).expect("a constant").ty, ty, "{text} has the wrong type");
        }
    }

    #[test]
    fn each_type_is_converted_in_the_format_the_target_has_for_it() {
        // Every row measured by printing the bytes of the constant on gcc 13.3, x86-64 Linux.
        // The two that surprise are `_Float32x`, which is plain `double`, and `_Float64x`,
        // which is the x87 format and so the same bits as `long double` and `__float80`.
        assert_eq!(bits("0.1f"), 0x3dcc_cccd);
        assert_eq!(bits("0.1f16"), 0x2e66);
        assert_eq!(bits("0.1f32x"), 0x3fb9_9999_9999_999a);
        assert_eq!(bits("0.1f64x"), 0x3ffb_cccc_cccc_cccc_cccd);
        assert_eq!(bits("0.1w"), 0x3ffb_cccc_cccc_cccc_cccd);
        assert_eq!(bits("0.1l"), 0x3ffb_cccc_cccc_cccc_cccd);
        assert_eq!(bits("0.1q"), 0x3ffb_9999_9999_9999_9999_9999_9999_999a);
        assert_eq!(bits("0.1f128"), 0x3ffb_9999_9999_9999_9999_9999_9999_999a);
        assert_eq!(bits("1.0l"), 0x3fff_8000_0000_0000_0000);
    }

    #[test]
    fn the_format_comes_from_the_target_and_not_from_the_host() {
        // `long double` is 128 bits wide on both of these and it is not the same type on both,
        // which is the whole reason the target carries a format and not only a width.
        let arm = floating("1.0l", Std::C23, &aarch64()).expect("a constant");
        assert_eq!(arm.value.to_bits(), 0x3fff_0000_0000_0000_0000_0000_0000_0000);
        assert_eq!(bits("1.0l"), 0x3fff_8000_0000_0000_0000);
        // And `_Float64x` follows it, being whatever the target has above `_Float64`.
        let arm_wide = floating("0.1f64x", Std::C23, &aarch64()).expect("a constant");
        assert_eq!(arm_wide.value.to_bits(), 0x3ffb_9999_9999_9999_9999_9999_9999_999a);
        // Where `long double` is `double` the constant is a `double` too.
        let windows =
            TargetInfo::new("x86_64-pc-windows-msvc".parse::<Triple>().expect("a known triple"));
        let on_windows = floating("1.0l", Std::C23, &windows).expect("a constant");
        assert_eq!(on_windows.value.to_bits(), 0x3ff0_0000_0000_0000);
    }

    #[test]
    fn the_case_rules_of_a_floating_suffix_are_not_uniform() {
        // The `f` of a `_FloatN` suffix is free and the `x` of a `_FloatNx` one is not, and the
        // two letters of a decimal suffix have to agree. Measured on gcc 13.3, which accepts
        // every one of these in every dialect.
        for text in ["1.0f", "1.0F", "1.0L", "1.0Q", "1.0W", "1.0F32", "1.0f64x", "1.0F64x"] {
            assert!(float(text).is_ok(), "{text} is a constant in gcc");
        }
        for text in ["1.0F32X", "1.0f32X", "1.0f16x", "1.0ff", "1.0fl", "1.0lf", "1.0fF", "1.0LL"] {
            assert_eq!(float(text), Err(FloatError::InvalidSuffix), "{text} is not");
        }
    }

    #[test]
    fn an_imaginary_suffix_may_sit_on_either_side_of_the_type() {
        for text in ["1.0i", "1.0j", "1.0I", "1.0J", "1.0if", "1.0fi", "1.0Li", "1.0iL", "1.0f16i"]
        {
            let constant = float(text).expect("a constant in gcc");
            assert!(constant.imaginary, "{text} is imaginary");
            assert!(constant.remarks.has(Remarks::IMAGINARY));
        }
        assert_eq!(float("1.0ii"), Err(FloatError::InvalidSuffix));
        assert_eq!(float("1.0ij"), Err(FloatError::InvalidSuffix));
        assert!(!float("1.0f").expect("a constant").imaginary);
    }

    #[test]
    fn a_decimal_floating_constant_is_recognised_and_refused() {
        // The constant is well formed and there is nowhere in this compiler to put its value.
        for text in ["1.0df", "1.0dd", "1.0dl", "1.0DF", "1.0DD", "1.0DL"] {
            assert_eq!(float(text), Err(FloatError::DecimalFloat), "{text} is a decimal float");
        }
        // The letters have to agree about case, so these are not decimal floats and not
        // constants either.
        for text in ["1.0Df", "1.0dF", "1.0dD", "1.0Dl"] {
            assert_eq!(float(text), Err(FloatError::InvalidSuffix), "{text} is neither");
        }
        // A `d` on its own is a `double` written the long way, which gcc allows everywhere.
        let long_way = float("1.0d").expect("a GCC extension");
        assert_eq!(long_way.ty, FloatConstantType::Double);
        assert!(long_way.remarks.has(Remarks::DOUBLE_SUFFIX));
    }

    #[test]
    fn a_type_the_target_does_not_have_is_refused_by_name() {
        // gcc says "'_Float128x' is not supported on this target" rather than calling the
        // suffix invalid, and it is supported on no target here.
        assert_eq!(float("1.0f128x"), Err(FloatError::UnsupportedType));
        // `__float80` is the x87 format, which only x86 has.
        assert_eq!(floating("1.0w", Std::C23, &aarch64()), Err(FloatError::UnsupportedType));
        assert!(float("1.0w").is_ok());
    }

    #[test]
    fn a_hexadecimal_constant_needs_an_exponent_and_a_decimal_one_does_not() {
        // `f` is a hexadecimal digit, so without the exponent there is no telling the number
        // from the suffix. Both compilers require it.
        assert_eq!(float("0x1.8"), Err(FloatError::MissingExponent));
        assert_eq!(bits("0x1.8p0"), 0x3ff8_0000_0000_0000);
        assert_eq!(bits("0x.8p1"), 0x3ff0_0000_0000_0000);
        assert_eq!(bits("1.5"), 0x3ff8_0000_0000_0000);
        for text in ["1.0e", "1e+", "1e-", "0x1p", "0x1p+"] {
            assert_eq!(float(text), Err(FloatError::NoExponentDigits), "{text} has no exponent");
        }
        assert_eq!(float("1.2.3"), Err(FloatError::TooManyPoints));
    }

    #[test]
    fn an_integer_constant_is_handed_back_rather_than_refused() {
        for text in ["1", "0", "0x10", "1u", "0777", "1wb", "0b1", "0xe5", "1f"] {
            assert_eq!(float(text), Err(FloatError::Integer), "{text} belongs to the other path");
        }
    }

    #[test]
    fn a_value_past_the_range_of_its_type_is_still_a_constant() {
        let large = float("1e400").expect("a constant gcc compiles");
        assert!(large.value.is_infinite());
        assert!(large.remarks.has(Remarks::OUT_OF_RANGE));
        let small = float("1e-400").expect("a constant gcc compiles");
        assert!(small.value.is_zero());
        assert!(small.remarks.has(Remarks::TRUNCATED));
        // The same two in the format the suffix asked for rather than in `double`.
        assert!(float("1e39f").expect("a constant").remarks.has(Remarks::OUT_OF_RANGE));
        assert!(float("1e-46f").expect("a constant").remarks.has(Remarks::TRUNCATED));
        assert!(float("1e-4951l").expect("a constant").remarks.has(Remarks::TRUNCATED));
        // A subnormal is a number the program can use, and gcc says nothing about it.
        let subnormal = float("1e-320").expect("a constant");
        assert!(!subnormal.value.is_zero());
        assert!(subnormal.remarks.is_none());
    }

    #[test]
    fn the_dialect_decides_what_a_constant_is_worth_saying_about() {
        // "use of C99 hexadecimal floating constant", which gcc says under C89 and not after.
        let old = floating("0x1p3", Std::C89, &linux()).expect("gcc compiles it anyway");
        assert!(old.remarks.has(Remarks::HEX_FLOAT));
        assert!(floating("0x1p3", Std::C99, &linux()).expect("standard").remarks.is_none());
        // Separators are C23 in both compilers, in the number and in the exponent.
        assert_eq!(bits("1'0.5"), 0x4025_0000_0000_0000);
        assert_eq!(bits("1.0e1'0"), 0x4202_a05f_2000_0000);
        assert!(float("0x1'0p0").expect("a C23 constant").remarks.is_none());
        let older = floating("1'0.5", Std::C17, &linux()).expect("still converted");
        assert!(older.remarks.has(Remarks::SEPARATORS));
        // And every extension suffix is accepted in every dialect, with a word about it.
        for text in ["1.0q", "1.0w", "1.0f16", "1.0f32x"] {
            let constant = floating(text, Std::C89, &linux()).expect("gcc accepts it in C89");
            assert!(constant.remarks.has(Remarks::EXTENDED_SUFFIX), "{text} is not standard");
        }
        assert!(float("1.0f").expect("a constant").remarks.is_none());
    }
}
