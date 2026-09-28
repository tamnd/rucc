//! A decimal number carried in a [`Float`], and the few exact operations on one.
//!
//! A decimal constant takes the same road through the compiler as a binary one: the lexer makes
//! it, the constant evaluator holds it and the lowering writes its bits into an `fconst`. So it
//! is a [`Float`] too, with the fields meaning the decimal things they are named after. The
//! significand is the coefficient and the exponent is the power of ten, so the value of a finite
//! one is `significand * 10^exponent`, and a zero keeps its exponent, since `0.00dd` and `0dd`
//! are two encodings of zero and a program can print the difference.
//!
//! What is here is what can be done exactly or with one correct rounding without decimal
//! arithmetic: the encoding both ways, the conversions from integers, between the decimal widths
//! and from a binary format, a comparison, and truncation to an integer. Addition and the rest
//! are not, and the constant evaluator does not ask for them, so an expression that needs one is
//! left to the program, which calls libgcc for it at run time the way gcc's output does.

use std::cmp::Ordering;

use crate::decimal::Decimal;
use crate::dfp::{self, Decoded, Width};
use crate::float::{Category, Float, Format, Status};

impl Float {
    /// The number in the decimal encoding its format names.
    pub(super) fn decimal_bits(self, width: Width) -> u128 {
        let sign = if self.sign { dfp::sign_bit(width) } else { 0 };
        match self.category {
            Category::Zero | Category::Finite => {
                dfp::encode(self.sign, self.significand, self.exponent, width)
            }
            Category::Infinite => dfp::infinity(self.sign, width),
            // A signalling nan is the quiet one with the bit below the combination field set,
            // and the exponent is where [`Float::decimal_from_bits`] left which it was.
            Category::Nan => {
                let signaling = if self.exponent != 0 { 1u128 << (width.bits() - 7) } else { 0 };
                dfp::nan(width) | signaling | sign
            }
        }
    }

    /// The number the bits of a decimal encoding hold.
    pub(super) fn decimal_from_bits(format: Format, width: Width, bits: u128) -> Float {
        match dfp::decode(bits, width) {
            Decoded::Finite { sign, coefficient, exponent } => Float {
                format,
                category: if coefficient == 0 { Category::Zero } else { Category::Finite },
                sign,
                exponent,
                significand: coefficient,
            },
            Decoded::Infinite { sign } => {
                Float { format, category: Category::Infinite, sign, exponent: 0, significand: 0 }
            }
            Decoded::Nan { sign, signaling } => Float {
                format,
                category: Category::Nan,
                sign,
                exponent: i32::from(signaling),
                significand: 0,
            },
        }
    }

    /// The decimal number nearest `text`, which is the whole of what [`dfp::parse`] does.
    fn decimal_parsed(text: &str, format: Format, width: Width) -> (Float, Status) {
        match dfp::parse(text, width) {
            Ok((bits, status)) => (Float::decimal_from_bits(format, width, bits), status),
            // Every caller here builds the text itself, so it is always a number.
            Err(_) => (Float::nan(format), Status::INVALID),
        }
    }

    /// The text of a finite decimal number, which [`dfp::parse`] reads back to the same encoding.
    fn decimal_text(self) -> String {
        let sign = if self.sign { "-" } else { "" };
        format!("{sign}{}e{}", self.significand, self.exponent)
    }

    /// An integer in a decimal format, rounded to nearest with ties to even when it has more
    /// digits than the format.
    pub(super) fn decimal_from_integer(
        negative: bool,
        magnitude: u128,
        format: Format,
        width: Width,
    ) -> (Float, Status) {
        let sign = if negative { "-" } else { "" };
        Float::decimal_parsed(&format!("{sign}{magnitude}"), format, width)
    }

    /// This number in another format where one of the two is decimal.
    ///
    /// Between two decimal widths the coefficient and the exponent go across and are rounded if
    /// the narrower one has too few digits. From binary to decimal the binary number's exact
    /// decimal expansion goes across with its trailing zeros taken off, which is the exponent
    /// gcc gives the same fold, and from decimal to binary the decimal number is read the way a
    /// binary constant is, correctly rounded.
    pub(super) fn decimal_to_format(self, format: Format) -> (Float, Status) {
        match self.category {
            Category::Nan => {
                let nan = Float::nan(format);
                return (Float { sign: self.sign, ..nan }, Status::NONE);
            }
            Category::Infinite => return (Float::infinity(format, self.sign), Status::NONE),
            Category::Zero | Category::Finite => {}
        }
        let text = if self.format.decimal().is_some() {
            if self.is_zero() && format.decimal().is_none() {
                return (Float::zero(format, self.sign), Status::NONE);
            }
            self.decimal_text()
        } else if self.is_zero() {
            let sign = if self.sign { "-" } else { "" };
            format!("{sign}0")
        } else {
            self.binary_text()
        };
        match format.decimal() {
            Some(width) => Float::decimal_parsed(&text, format, width),
            None => Float::parse(&text, format).unwrap_or((Float::nan(format), Status::INVALID)),
        }
    }

    /// The exact decimal expansion of a finite binary number, as text.
    fn binary_text(self) -> String {
        let (significand, exponent) = self.parts();
        let digits: Vec<u8> = significand.to_string().bytes().map(|b| b - b'0').collect();
        let point = digits.len() as i32;
        let mut exact = Decimal::new(digits, point);
        exact.shift(exponent);
        let sign = if self.sign { "-" } else { "" };
        format!("{sign}{}", exact.spelled())
    }

    /// The order of two decimal numbers of one format, which is [`None`] when either is a nan.
    pub(super) fn decimal_compare(self, other: Float) -> Option<Ordering> {
        if self.is_nan() || other.is_nan() {
            return None;
        }
        if self.is_zero() && other.is_zero() {
            return Some(Ordering::Equal);
        }
        if self.sign != other.sign {
            return Some(if self.sign { Ordering::Less } else { Ordering::Greater });
        }
        let magnitudes = match (self.category, other.category) {
            (Category::Infinite, Category::Infinite) => Ordering::Equal,
            (Category::Infinite, _) => Ordering::Greater,
            (_, Category::Infinite) => Ordering::Less,
            (Category::Zero, _) => Ordering::Less,
            (_, Category::Zero) => Ordering::Greater,
            _ => decimal_magnitudes(self, other),
        };
        Some(if self.sign { magnitudes.reverse() } else { magnitudes })
    }

    /// A decimal number truncated toward zero into an integer, with the same answers for a
    /// value that does not fit as [`Float::to_integer`] gives for a binary one.
    pub(super) fn decimal_integer(self, limit: u128) -> (i128, Status) {
        match self.category {
            Category::Nan => (0, Status::INVALID),
            Category::Infinite => (self.signed_value(limit), Status::INVALID),
            Category::Zero => (0, Status::NONE),
            Category::Finite => {
                let (magnitude, inexact) = if self.exponent >= 0 {
                    let scaled = u32::try_from(self.exponent)
                        .ok()
                        .and_then(|power| 10u128.checked_pow(power))
                        .and_then(|scale| self.significand.checked_mul(scale));
                    match scaled {
                        Some(magnitude) => (magnitude, false),
                        None => return (self.signed_value(limit), Status::INVALID),
                    }
                } else {
                    let power = u32::try_from(-i64::from(self.exponent)).unwrap_or(u32::MAX);
                    match 10u128.checked_pow(power) {
                        Some(scale) => (self.significand / scale, self.significand % scale != 0),
                        None => (0, true),
                    }
                };
                if magnitude > limit {
                    return (self.signed_value(limit), Status::INVALID);
                }
                let status = if inexact { Status::INEXACT } else { Status::NONE };
                (self.signed_value(magnitude), status)
            }
        }
    }

    /// The spelling a diagnostic or a dump gives a decimal number.
    pub(super) fn decimal_spelling(self) -> String {
        let sign = if self.sign { "-" } else { "" };
        match self.category {
            Category::Nan => format!("{sign}nan"),
            Category::Infinite => format!("{sign}inf"),
            Category::Zero | Category::Finite => self.decimal_text(),
        }
    }

    /// Whether a finite decimal number is normal, which is having its leading digit at or above
    /// the smallest exponent a normal number of the format has.
    pub(super) fn decimal_is_normal(self, width: Width) -> bool {
        let Category::Finite = self.category else { return false };
        let adjusted = self.exponent + digits(self.significand) as i32 - 1;
        adjusted >= width.min_exponent() + width.digits() as i32 - 1
    }
}

/// How many decimal digits a coefficient has, which is one for zero.
fn digits(mut value: u128) -> u32 {
    let mut count = 1;
    while value >= 10 {
        value /= 10;
        count += 1;
    }
    count
}

/// The order of the magnitudes of two finite nonzero decimal numbers.
///
/// The adjusted exponent, the power of ten of the leading digit, decides unless the two agree,
/// and when they do the coefficients have the same number of digits once the shorter one is
/// scaled up, which is at most thirty four digits and fits.
fn decimal_magnitudes(left: Float, right: Float) -> Ordering {
    let left_digits = digits(left.significand) as i32;
    let right_digits = digits(right.significand) as i32;
    let left_adjusted = left.exponent + left_digits;
    let right_adjusted = right.exponent + right_digits;
    if left_adjusted != right_adjusted {
        return left_adjusted.cmp(&right_adjusted);
    }
    let (mut a, mut b) = (left.significand, right.significand);
    match left_digits.cmp(&right_digits) {
        Ordering::Less => a *= 10u128.pow((right_digits - left_digits) as u32),
        Ordering::Greater => b *= 10u128.pow((left_digits - right_digits) as u32),
        Ordering::Equal => {}
    }
    a.cmp(&b)
}
