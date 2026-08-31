//! Binary floating point, in software, for every format the compiler has to produce.
//!
//! A compiler cannot ask the machine it is running on what a floating constant means. The host
//! may not have the format at all, `long double` is eighty bits on x86-64 and a hundred and
//! twenty eight on AArch64 Linux and sixty four on Apple, and `strtod` is the host's libc
//! rather than the target's semantics. Reproducible output means the same source gives the same
//! bits whoever compiles it, so the conversion is done here, exactly, in integer arithmetic.
//!
//! [`Float`] is a sign, a category, an exponent and a significand of up to a hundred and
//! thirteen bits, which is every format in [`Format`] including the x87 eighty bit one with its
//! stored leading bit. The value of a finite number is `significand * 2^(exponent - precision +
//! 1)`, so the significand is an integer rather than a fraction and the exponent is that of its
//! leading bit.
//!
//! Conversion from text is correctly rounded, round to nearest with ties to even, which is the
//! only rounding mode a translation-time constant uses. The decimal path scales the number by
//! powers of two until it is in `[1, 2)` and then reads the significand off it, using the exact
//! decimal in `decimal.rs` so that no step ever loses a bit. A naive `mantissa * 10^exponent`
//! in `f64` is wrong in the last place for a noticeable fraction of literals, and the last
//! place is exactly what a differential test against another compiler notices. Hexadecimal
//! constants are exact by construction and only have to be rounded once.
//!
//! ```
//! use rucc_base::float::{Float, Format};
//!
//! let (value, status) = Float::parse("0.1", Format::Double).expect("a number");
//! assert_eq!(value.to_bits(), (0.1f64).to_bits() as u128);
//! assert!(status.has(rucc_base::float::Status::INEXACT));
//! ```
//!
//! Arithmetic is not here yet. The constant evaluator needs it and will bring it.

use crate::decimal::{Decimal, Fraction};

/// A binary floating point format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// IEEE binary16, which C spells `_Float16`.
    Half,
    /// The brain float, an IEEE binary32 with the low sixteen bits of its significand cut off,
    /// which C spells `__bf16`. It has the range of a `float` and less than half its precision.
    BFloat16,
    /// IEEE binary32, which C spells `float`.
    Single,
    /// IEEE binary64, which C spells `double`.
    Double,
    /// The x87 eighty bit format, which is `long double` on x86. It is the one format here that
    /// stores the leading significand bit rather than leaving it implied.
    X87Extended,
    /// IEEE binary128, which C spells `_Float128`, and which is `long double` on AArch64 Linux
    /// and on RISC-V.
    Quad,
}

impl Format {
    /// The number of significand bits, counting the leading one whether it is stored or not.
    #[must_use]
    pub const fn precision(self) -> u32 {
        match self {
            Format::Half => 11,
            Format::BFloat16 => 8,
            Format::Single => 24,
            Format::Double => 53,
            Format::X87Extended => 64,
            Format::Quad => 113,
        }
    }

    /// The exponent of the largest finite number, which is also the exponent bias.
    #[must_use]
    pub const fn max_exponent(self) -> i32 {
        match self {
            Format::Half => 15,
            Format::BFloat16 | Format::Single => 127,
            Format::Double => 1023,
            Format::X87Extended | Format::Quad => 16383,
        }
    }

    /// The exponent of the smallest normal number.
    #[must_use]
    pub const fn min_exponent(self) -> i32 {
        1 - self.max_exponent()
    }

    /// The width of the encoding in bits, which for x87 is the eighty bits that matter and not
    /// the ninety six or hundred and twenty eight an ABI pads them out to.
    #[must_use]
    pub const fn width(self) -> u32 {
        match self {
            Format::Half | Format::BFloat16 => 16,
            Format::Single => 32,
            Format::Double => 64,
            Format::X87Extended => 80,
            Format::Quad => 128,
        }
    }

    /// Whether the leading significand bit is stored rather than implied.
    #[must_use]
    pub const fn has_explicit_integer_bit(self) -> bool {
        matches!(self, Format::X87Extended)
    }

    /// The width of the exponent field.
    const fn exponent_bits(self) -> u32 {
        self.width() - self.significand_bits() - 1
    }

    /// The width of the stored significand field.
    const fn significand_bits(self) -> u32 {
        if self.has_explicit_integer_bit() { self.precision() } else { self.precision() - 1 }
    }

    /// A decimal exponent above which every number is too large for the format.
    ///
    /// The value is at least `10^(point - 1)`, so a point past this cannot be finite. It is
    /// deliberately loose: it exists to stop the scaling loop from walking a million powers of
    /// ten, not to decide anything.
    const fn max_decimal_exponent(self) -> i32 {
        (self.max_exponent() + 1) * 30103 / 100000 + 2
    }

    /// A decimal exponent below which every number rounds to zero.
    const fn min_decimal_exponent(self) -> i32 {
        (self.min_exponent() - self.precision() as i32) * 30103 / 100000 - 2
    }
}

/// What a conversion had to do to the number to fit it in the format.
///
/// A bitmask, so that one conversion can report several. The names are IEEE 754's exceptions,
/// which is what the diagnostics are ultimately about: GCC warns that a floating constant
/// exceeds the range of its type, or that it was truncated to zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Status(u8);

impl Status {
    /// The value is exactly what was written.
    pub const NONE: Status = Status(0);
    /// The value had to be rounded, so it is not what was written.
    pub const INEXACT: Status = Status(1);
    /// The value is too large for the format and became an infinity.
    pub const OVERFLOW: Status = Status(2);
    /// The value is too small for the format and became a subnormal or a zero.
    pub const UNDERFLOW: Status = Status(4);

    /// Whether every flag in `other` is set here.
    #[inline]
    #[must_use]
    pub const fn has(self, other: Status) -> bool {
        self.0 & other.0 == other.0
    }

    /// This set with `other` added.
    #[inline]
    #[must_use]
    pub const fn with(self, other: Status) -> Status {
        Status(self.0 | other.0)
    }

    /// Whether nothing happened to the number.
    #[inline]
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// Why a spelling is not a number.
///
/// The caller is expected to have checked the shape of the token already, so these are the
/// cases a lexer cannot rule out rather than a full grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// There is no digit anywhere in it.
    NoDigits,
    /// There is an exponent marker with no digits after it.
    NoExponentDigits,
    /// There is a character in it that a number does not have.
    Invalid,
}

/// What kind of number this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Category {
    Zero,
    Finite,
    Infinite,
}

/// A floating point number in a given format.
///
/// A finite value is `significand * 2^(exponent - precision + 1)`. A normal number has its
/// leading significand bit set, a subnormal does not and has the format's minimum exponent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Float {
    format: Format,
    category: Category,
    sign: bool,
    exponent: i32,
    significand: u128,
}

impl Float {
    /// A zero of the given sign.
    #[must_use]
    pub const fn zero(format: Format, sign: bool) -> Float {
        Float { format, category: Category::Zero, sign, exponent: 0, significand: 0 }
    }

    /// An infinity of the given sign.
    #[must_use]
    pub const fn infinity(format: Format, sign: bool) -> Float {
        Float { format, category: Category::Infinite, sign, exponent: 0, significand: 0 }
    }

    /// The format this number is in.
    #[must_use]
    pub const fn format(self) -> Format {
        self.format
    }

    /// Whether the number is negative, which a zero can be.
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.sign
    }

    /// Whether the number is a zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        matches!(self.category, Category::Zero)
    }

    /// Whether the number is an infinity.
    #[must_use]
    pub const fn is_infinite(self) -> bool {
        matches!(self.category, Category::Infinite)
    }

    /// Whether the number is finite, which a zero is.
    #[must_use]
    pub const fn is_finite(self) -> bool {
        !self.is_infinite()
    }

    /// Converts a decimal or hexadecimal spelling into the nearest number in `format`, rounding
    /// to nearest with ties to even.
    ///
    /// The spelling is the number alone: no suffix, because the suffix is what chose the
    /// format, and no infinity or nan, because C has no spelling for those. A sign is accepted
    /// even though a C constant never has one, since the value the constant evaluator folds
    /// does. C23 digit separators are stripped here.
    ///
    /// # Errors
    ///
    /// [`ParseError`], for a spelling that is not a number at all.
    pub fn parse(text: &str, format: Format) -> Result<(Float, Status), ParseError> {
        let bytes = text.as_bytes();
        let (sign, rest) = match bytes.first() {
            Some(b'-') => (true, &bytes[1..]),
            Some(b'+') => (false, &bytes[1..]),
            _ => (false, bytes),
        };
        if rest.len() > 1 && rest[0] == b'0' && rest[1] | 32 == b'x' {
            hexadecimal(&rest[2..], sign, format)
        } else {
            decimal(rest, sign, format)
        }
    }

    /// The bits of the encoding, in the low [`Format::width`] bits.
    ///
    /// The x87 format keeps its leading significand bit, so its eightieth bit is the sign and
    /// its sixty fourth is the one every other format leaves implied.
    #[must_use]
    pub fn to_bits(self) -> u128 {
        let format = self.format;
        let significand_mask = (1u128 << format.significand_bits()) - 1;
        let (exponent_field, significand_field) = match self.category {
            Category::Zero => (0, 0),
            Category::Infinite => (
                (1u128 << format.exponent_bits()) - 1,
                if format.has_explicit_integer_bit() {
                    1u128 << (format.precision() - 1)
                } else {
                    0
                },
            ),
            Category::Finite => {
                let subnormal = self.significand >> (format.precision() - 1) == 0;
                let field =
                    if subnormal { 0 } else { (self.exponent + format.max_exponent()) as u128 };
                (field, self.significand & significand_mask)
            }
        };
        let sign = u128::from(self.sign) << (format.width() - 1);
        sign | (exponent_field << format.significand_bits()) | significand_field
    }

    /// Reads a number back out of its encoding, which is what makes [`Float::to_bits`] testable
    /// and what a constant folded in the IR is stored as.
    ///
    /// A signalling or quiet nan comes back as an infinity, because nothing here makes one and
    /// nothing here has anywhere to put the payload yet.
    #[must_use]
    pub fn from_bits(format: Format, bits: u128) -> Float {
        let significand_bits = format.significand_bits();
        let sign = (bits >> (format.width() - 1)) & 1 == 1;
        let exponent_field =
            ((bits >> significand_bits) & ((1u128 << format.exponent_bits()) - 1)) as i32;
        let stored = bits & ((1u128 << significand_bits) - 1);
        if exponent_field == (1 << format.exponent_bits()) - 1 {
            return Float::infinity(format, sign);
        }
        let implicit = if format.has_explicit_integer_bit() || exponent_field == 0 {
            0
        } else {
            1u128 << (format.precision() - 1)
        };
        let significand = stored | implicit;
        if significand == 0 {
            return Float::zero(format, sign);
        }
        let exponent = if exponent_field == 0 {
            format.min_exponent()
        } else {
            exponent_field - format.max_exponent()
        };
        Float { format, category: Category::Finite, sign, exponent, significand }
    }
}

/// Converts a decimal spelling.
fn decimal(bytes: &[u8], sign: bool, format: Format) -> Result<(Float, Status), ParseError> {
    let mut digits = Vec::new();
    let mut integer_digits = 0i32;
    let mut seen_point = false;
    let mut seen_digit = false;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            byte @ b'0'..=b'9' => {
                digits.push(byte - b'0');
                if !seen_point {
                    integer_digits += 1;
                }
                seen_digit = true;
            }
            b'\'' => {}
            b'.' if !seen_point => seen_point = true,
            b'e' | b'E' => break,
            _ => return Err(ParseError::Invalid),
        }
        index += 1;
    }
    if !seen_digit {
        return Err(ParseError::NoDigits);
    }
    let mut point = integer_digits;
    if index < bytes.len() {
        point = point.saturating_add(exponent_of(&bytes[index + 1..])?);
    }
    Ok(convert(Decimal::new(digits, point), sign, format))
}

/// Converts a hexadecimal spelling, which is exact until the one rounding at the end.
fn hexadecimal(bytes: &[u8], sign: bool, format: Format) -> Result<(Float, Status), ParseError> {
    let mut significand: u128 = 0;
    let mut exponent = 0i32;
    let mut sticky = false;
    let mut seen_point = false;
    let mut seen_digit = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            b'\'' => {
                index += 1;
                continue;
            }
            b'.' if !seen_point => {
                seen_point = true;
                index += 1;
                continue;
            }
            b'p' | b'P' => break,
            _ => return Err(ParseError::Invalid),
        };
        seen_digit = true;
        if significand.leading_zeros() >= 4 {
            significand = (significand << 4) | u128::from(digit);
            if seen_point {
                exponent -= 4;
            }
        } else {
            // Past a hundred and twenty eight bits the digits cannot change the value, only
            // whether it is exactly halfway, which is what the sticky bit is for.
            sticky |= digit != 0;
            if !seen_point {
                exponent += 4;
            }
        }
        index += 1;
    }
    if !seen_digit {
        return Err(ParseError::NoDigits);
    }
    if index < bytes.len() {
        exponent = exponent.saturating_add(exponent_of(&bytes[index + 1..])?);
    }
    Ok(round(significand, exponent, sticky, sign, format))
}

/// Reads the digits of an exponent, which may be signed.
fn exponent_of(bytes: &[u8]) -> Result<i32, ParseError> {
    let (negative, digits) = match bytes.first() {
        Some(b'-') => (true, &bytes[1..]),
        Some(b'+') => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    if digits.is_empty() {
        return Err(ParseError::NoExponentDigits);
    }
    let mut value = 0i32;
    for &byte in digits {
        if byte == b'\'' {
            continue;
        }
        if !byte.is_ascii_digit() {
            return Err(ParseError::Invalid);
        }
        // An exponent far past the format's range is the same as one at the edge of it, so it
        // saturates rather than overflowing.
        value = value.saturating_mul(10).saturating_add(i32::from(byte - b'0'));
    }
    Ok(if negative { -value } else { value })
}

/// Scales an exact decimal down to the format's significand and rounds it.
fn convert(mut value: Decimal, sign: bool, format: Format) -> (Float, Status) {
    if value.is_zero() {
        return (Float::zero(format, sign), Status::NONE);
    }
    if value.point() > format.max_decimal_exponent() {
        return (Float::infinity(format, sign), Status::OVERFLOW.with(Status::INEXACT));
    }
    if value.point() < format.min_decimal_exponent() {
        return (Float::zero(format, sign), Status::UNDERFLOW.with(Status::INEXACT));
    }

    // Scale until the value is in `[1, 2)`, counting the powers of two taken out of it. Each
    // step is an underestimate of the distance left, so no step overshoots and the loop always
    // moves, which is what stops it oscillating.
    let mut exponent = 0i32;
    loop {
        let point = value.point();
        if point > 1 || (point == 1 && value.first_digit() >= 2) {
            let step = binary_digits(point - 1).clamp(1, 60);
            value.shift(-step);
            exponent += step;
        } else if point < 1 {
            let step = (1 + binary_digits(-point)).clamp(1, 60);
            value.shift(step);
            exponent -= step;
        } else {
            break;
        }
    }

    // The significand is the value scaled by this many powers of two, clamped so that a number
    // below the smallest normal loses precision instead of exponent.
    let precision = format.precision() as i32;
    let scale = (exponent - precision + 1).max(format.min_exponent() - precision + 1);
    value.shift(exponent - scale);
    let (integer, fraction) = value.round_to_u128();
    let rounded = match fraction {
        Fraction::Zero | Fraction::BelowHalf => integer,
        Fraction::Half => integer + (integer & 1),
        Fraction::AboveHalf => integer + 1,
    };
    finish(rounded, scale, fraction != Fraction::Zero, sign, format)
}

/// Roughly how many binary digits a decimal one of this many digits has, never overestimating.
const fn binary_digits(decimal: i32) -> i32 {
    decimal * 33219 / 10000
}

/// Rounds `significand * 2^exponent` into the format, with `sticky` saying that something
/// nonzero was already dropped below it.
fn round(
    significand: u128,
    exponent: i32,
    sticky: bool,
    sign: bool,
    format: Format,
) -> (Float, Status) {
    if significand == 0 {
        return (Float::zero(format, sign), Status::NONE);
    }
    let precision = format.precision() as i32;
    let leading = (128 - significand.leading_zeros()) as i32;
    let scale = (exponent + leading - precision).max(format.min_exponent() - precision + 1);
    let mut sticky = sticky;
    let (integer, half) = if scale <= exponent {
        (significand << (exponent - scale), false)
    } else {
        let drop = (scale - exponent) as u32;
        if drop >= 128 {
            sticky = true;
            (0, false)
        } else {
            let half = (significand >> (drop - 1)) & 1 == 1;
            sticky |= drop > 1 && significand & ((1u128 << (drop - 1)) - 1) != 0;
            (significand >> drop, half)
        }
    };
    let rounded = if half && (sticky || integer & 1 == 1) { integer + 1 } else { integer };
    finish(rounded, scale, half || sticky, sign, format)
}

/// Turns a rounded significand and the power of two it is scaled by into a number, handling the
/// carry out of the significand and the two ends of the format's range.
fn finish(
    significand: u128,
    scale: i32,
    inexact: bool,
    sign: bool,
    format: Format,
) -> (Float, Status) {
    let precision = format.precision();
    let mut significand = significand;
    let mut scale = scale;
    if significand >> precision != 0 {
        // Rounding up carried out of the top bit, which only ever gives a power of two.
        significand >>= 1;
        scale += 1;
    }
    let mut status = if inexact { Status::INEXACT } else { Status::NONE };
    if significand == 0 {
        return (Float::zero(format, sign), status.with(Status::UNDERFLOW));
    }
    let exponent = scale + precision as i32 - 1;
    if exponent > format.max_exponent() {
        return (
            Float::infinity(format, sign),
            status.with(Status::OVERFLOW).with(Status::INEXACT),
        );
    }
    let normal = significand >> (precision - 1) != 0;
    if !normal && inexact {
        status = status.with(Status::UNDERFLOW);
    }
    let exponent = if normal { exponent } else { format.min_exponent() };
    (Float { format, category: Category::Finite, sign, exponent, significand }, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bits a `double` conversion gives, next to what Rust's own parser gives.
    fn double(text: &str) -> u128 {
        Float::parse(text, Format::Double).expect("a number").0.to_bits()
    }

    /// The bits a `float` conversion gives.
    fn single(text: &str) -> u128 {
        Float::parse(text, Format::Single).expect("a number").0.to_bits()
    }

    #[test]
    fn the_ordinary_numbers_land_where_the_host_would_put_them() {
        for text in ["0", "1", "2", "0.5", "1.5", "3.14159", "2.718281828459045", "100", "1e10"] {
            let host = text.parse::<f64>().expect("a number Rust reads too");
            assert_eq!(double(text), u128::from(host.to_bits()), "{text}");
        }
    }

    #[test]
    fn a_number_that_needs_the_last_bit_rounded_gets_it_right() {
        // Every one of these is a literal a naive `mantissa * 10^exponent` gets wrong, and the
        // last is the longest one a `double` conversion has to read to round correctly.
        let hard = [
            "0.1",
            "0.3",
            "2.2250738585072011e-308",
            "2.2250738585072014e-308",
            "1.7976931348623157e308",
            "4.9406564584124654e-324",
            "5e-324",
            "8.98846567431158e307",
            "9007199254740993",
            "123456789012345678901234567890",
            "1.000000000000000000000000000000000000000000000000000000000000000001",
            "7.8459735791271921e65",
            "3.518437208883201171875e13",
            "0.500000000000000166533453693773481063544750213623046875",
        ];
        for text in hard {
            let host = text.parse::<f64>().expect("a number Rust reads too");
            assert_eq!(double(text), u128::from(host.to_bits()), "{text}");
        }
    }

    #[test]
    fn the_number_that_takes_seven_hundred_and_sixty_seven_digits() {
        // The exact decimal of a `double` halfway case. A conversion that truncates its input
        // rounds this one the wrong way, which is the bug this buffer size exists to avoid.
        let text = concat!(
            "2.47032822920623272088284396434110686182529901307162382",
            "35378852574870103599108683372845652890455735483022221802",
            "58573249056416711547735232764105795166208503595426876755",
            "62317084535693494535245273750735013572761315046354601316",
            "12127849863326369238975694273040488011871029093711789936",
            "42245692702737764465109076580131048946378905599180391359",
            "70011386455512221706120629864144453927884519445934871524",
            "63344875888932891414823975864211858166195965106373837732",
            "34435703331457550505022232309998195892058070506176382679",
            "16323484472119097902806154870514036458498974142754747141",
            "39683784321102080606305920253373777969877864922227306716",
            "01324339457879181214233820577228206278891620001855078759",
            "16278352090142077553206262229158550205643778244387017277",
            "94459649305087139089301871550805125768938177360937844105",
            "63661045147381814281647890691181239104545396303476425117",
            "7562185422741845851144691421326303120484712594187004993e-324"
        );
        let host = text.parse::<f64>().expect("a number Rust reads too");
        assert_eq!(double(text), u128::from(host.to_bits()));
    }

    #[test]
    fn a_sweep_of_random_numbers_agrees_with_rust_in_every_bit() {
        // A conversion that is wrong in the last place is wrong on a small fraction of inputs,
        // so this is a sweep rather than a handful. The generator is a fixed sequence, so a
        // failure names the same number on every machine.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..4000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let digits = state >> 11;
            let exponent = (state % 600) as i32 - 300;
            let text = format!("{digits}e{exponent}");
            let host = text.parse::<f64>().expect("a number Rust reads too");
            assert_eq!(double(&text), u128::from(host.to_bits()), "{text}");
            let host = text.parse::<f32>().expect("a number Rust reads too");
            assert_eq!(single(&text), u128::from(host.to_bits()), "{text} as a float");
        }
    }

    #[test]
    fn the_ends_of_the_range_are_an_infinity_and_a_zero() {
        let (value, status) = Float::parse("1e400", Format::Double).expect("a number");
        assert!(value.is_infinite() && status.has(Status::OVERFLOW));
        let (value, status) = Float::parse("1e-400", Format::Double).expect("a number");
        assert!(value.is_zero() && status.has(Status::UNDERFLOW) && status.has(Status::INEXACT));
        // The largest `double` is finite and the next number up is not.
        let (value, status) = Float::parse("1.7976931348623157e308", Format::Double).expect("one");
        assert!(value.is_finite() && !status.has(Status::OVERFLOW));
        let (value, _) = Float::parse("1.8e308", Format::Double).expect("a number");
        assert!(value.is_infinite());
        // Half the smallest subnormal rounds to zero, and just over half rounds up to it.
        assert_eq!(double("2.4e-324"), u128::from((0f64).to_bits()));
        assert_eq!(double("2.5e-324"), 1);
    }

    #[test]
    fn a_number_that_is_exactly_what_was_written_says_so() {
        assert!(Float::parse("1", Format::Double).expect("a number").1.is_none());
        assert!(Float::parse("0.5", Format::Double).expect("a number").1.is_none());
        assert!(Float::parse("0.1", Format::Double).expect("a number").1.has(Status::INEXACT));
        // A number small enough to lose bits is inexact and underflowed, both.
        let (_, status) = Float::parse("1e-320", Format::Double).expect("a number");
        assert!(status.has(Status::INEXACT) && status.has(Status::UNDERFLOW));
    }

    #[test]
    fn a_hexadecimal_constant_is_exact_and_needs_no_scaling() {
        assert_eq!(double("0x1p0"), u128::from((1f64).to_bits()));
        assert_eq!(double("0x1.8p1"), u128::from((3f64).to_bits()));
        assert_eq!(double("0x1p-1074"), 1);
        assert_eq!(double("0xa.bp-4"), u128::from((0.66796875f64).to_bits()));
        assert_eq!(double("0X1.FFFFFFFFFFFFFP+1023"), u128::from(f64::MAX.to_bits()));
        assert!(Float::parse("0x1p0", Format::Double).expect("a number").1.is_none());
        // Seventeen hexadecimal digits is more than a `double` has, so this one rounds.
        let (_, status) = Float::parse("0x1.00000000000008p0", Format::Double).expect("a number");
        assert!(status.has(Status::INEXACT));
        assert_eq!(double("0x1.00000000000008p0"), u128::from((1f64).to_bits()));
        assert_eq!(double("0x1.00000000000018p0"), u128::from((1f64).to_bits() + 2));
    }

    #[test]
    fn digit_separators_are_not_part_of_the_number() {
        assert_eq!(double("1'000.000'1"), double("1000.0001"));
        assert_eq!(double("0x1'0p0"), double("16.0"));
        assert_eq!(double("1e1'0"), double("1e10"));
    }

    #[test]
    fn a_spelling_that_is_not_a_number_says_which_way_it_is_wrong() {
        assert_eq!(Float::parse("", Format::Double), Err(ParseError::NoDigits));
        assert_eq!(Float::parse(".", Format::Double), Err(ParseError::NoDigits));
        assert_eq!(Float::parse("1e", Format::Double), Err(ParseError::NoExponentDigits));
        assert_eq!(Float::parse("1e+", Format::Double), Err(ParseError::NoExponentDigits));
        assert_eq!(Float::parse("0x1p", Format::Double), Err(ParseError::NoExponentDigits));
        assert_eq!(Float::parse("0xp1", Format::Double), Err(ParseError::NoDigits));
        assert_eq!(Float::parse("1x0", Format::Double), Err(ParseError::Invalid));
    }

    #[test]
    fn a_sign_is_accepted_although_a_c_constant_never_has_one() {
        let (value, _) = Float::parse("-1.5", Format::Double).expect("a number");
        assert!(value.is_negative());
        assert_eq!(value.to_bits(), u128::from((-1.5f64).to_bits()));
        let (value, _) = Float::parse("-0.0", Format::Double).expect("a number");
        assert!(value.is_zero() && value.is_negative());
        assert_eq!(value.to_bits(), u128::from((-0.0f64).to_bits()));
    }

    #[test]
    fn every_format_says_how_wide_its_fields_are() {
        for format in [
            Format::Half,
            Format::BFloat16,
            Format::Single,
            Format::Double,
            Format::X87Extended,
            Format::Quad,
        ] {
            assert_eq!(
                format.exponent_bits() + format.significand_bits() + 1,
                format.width(),
                "{format:?}"
            );
            assert_eq!(format.min_exponent(), 1 - format.max_exponent());
        }
        assert_eq!(Format::Half.exponent_bits(), 5);
        assert_eq!(Format::BFloat16.exponent_bits(), 8);
        assert_eq!(Format::Single.exponent_bits(), 8);
        assert_eq!(Format::Double.exponent_bits(), 11);
        assert_eq!(Format::X87Extended.exponent_bits(), 15);
        assert_eq!(Format::Quad.exponent_bits(), 15);
    }

    #[test]
    fn a_number_survives_a_trip_through_its_encoding() {
        for format in [
            Format::Half,
            Format::BFloat16,
            Format::Single,
            Format::Double,
            Format::X87Extended,
            Format::Quad,
        ] {
            for text in ["0", "-0", "1", "-1.5", "3.14159", "1e-5", "65504", "0x1p-20"] {
                let (value, _) = Float::parse(text, format).expect("a number");
                let bits = value.to_bits();
                assert_eq!(Float::from_bits(format, bits).to_bits(), bits, "{text} in {format:?}");
            }
            assert_eq!(
                Float::from_bits(format, Float::infinity(format, false).to_bits()).to_bits(),
                Float::infinity(format, false).to_bits()
            );
        }
    }

    #[test]
    fn the_narrow_formats_round_where_they_are_supposed_to() {
        // `_Float16` has eleven bits, so its largest finite number is 65504 and the next power
        // of two is an infinity. `__bf16` has eight, so it loses a `float`'s low bits and keeps
        // its range, which is the whole point of the format.
        let (value, status) = Float::parse("65504", Format::Half).expect("a number");
        assert!(value.is_finite() && status.is_none());
        assert_eq!(value.to_bits(), 0x7bff);
        let (value, _) = Float::parse("65536", Format::Half).expect("a number");
        assert!(value.is_infinite());
        assert_eq!(Float::parse("1", Format::Half).expect("one").0.to_bits(), 0x3c00);
        assert_eq!(Float::parse("1", Format::BFloat16).expect("one").0.to_bits(), 0x3f80);
        assert_eq!(Float::parse("1e30", Format::BFloat16).expect("big").0.to_bits(), 0x714a);
        // The smallest `_Float16` subnormal, and half of it.
        assert_eq!(Float::parse("0x1p-24", Format::Half).expect("tiny").0.to_bits(), 1);
        assert!(Float::parse("0x1p-26", Format::Half).expect("tinier").0.is_zero());
    }

    #[test]
    fn the_x87_format_stores_the_bit_the_others_leave_implied() {
        // 1.0 is 0x3fff8000000000000000: the exponent field, then a significand whose top bit
        // is stored rather than implied. Every other format here would have zeros there.
        let one = Float::parse("1", Format::X87Extended).expect("one").0;
        assert_eq!(one.to_bits(), 0x3fff_8000_0000_0000_0000);
        assert_eq!(
            Float::parse("2", Format::X87Extended).expect("two").0.to_bits(),
            0x4000_8000_0000_0000_0000
        );
        // Sixty four bits of precision, so this is exact where a `double` would round it.
        let (value, status) = Float::parse("9007199254740993", Format::X87Extended).expect("one");
        assert!(status.is_none());
        assert_eq!(value.to_bits(), 0x4034_8000_0000_0000_0400);
        // Measured, by compiling the constant with gcc 13.3 on x86-64 and reading the ten
        // bytes back out of the program rather than trusting a table.
        assert_eq!(
            Float::parse("0.1", Format::X87Extended).expect("a tenth").0.to_bits(),
            0x3ffb_cccc_cccc_cccc_cccd
        );
        // A subnormal four thousand powers of ten down, which is three of the smallest number
        // the format has. gcc puts the same three there.
        assert_eq!(Float::parse("1e-4950", Format::X87Extended).expect("tiny").0.to_bits(), 3);
    }

    #[test]
    fn the_quad_format_has_a_hundred_and_thirteen_bits_of_it() {
        assert_eq!(
            Float::parse("1", Format::Quad).expect("one").0.to_bits(),
            0x3fff_0000_0000_0000_0000_0000_0000_0000
        );
        // 0.1 in binary128, which is the same digits a `double` gets and then sixty more bits.
        assert_eq!(
            Float::parse("0.1", Format::Quad).expect("a tenth").0.to_bits(),
            0x3ffb_9999_9999_9999_9999_9999_9999_999a
        );
        // Also measured against gcc, through `__float128`.
        assert_eq!(
            Float::parse("3.14159", Format::Quad).expect("pi, roughly").0.to_bits(),
            0x4000_921f_9f01_b866_e43a_a79b_badc_0981
        );
        let (value, status) = Float::parse("1e5000", Format::Quad).expect("a number");
        assert!(value.is_infinite() && status.has(Status::OVERFLOW));
        let (value, _) = Float::parse("1e-5000", Format::Quad).expect("a number");
        assert!(value.is_zero());
    }
}
