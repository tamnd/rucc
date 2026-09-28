//! Decimal floating point, as IEEE 754 lays it out in the binary integer decimal encoding.
//!
//! `_Decimal32`, `_Decimal64` and `_Decimal128` are C23 Annex H, and gcc implements them on every
//! x86-64 and AArch64 target by storing the coefficient as a binary integer, which is the BID
//! encoding, and calling the `__bid_*` routines in libgcc for the arithmetic. A constant is the
//! only place the compiler itself has to produce one, so this module is the conversion from the
//! text of a constant to the bits of the encoding, and back again for anything that wants to
//! read one, and nothing else. The arithmetic stays in libgcc, where gcc keeps it too.
//!
//! A decimal number is a sign, a coefficient of at most [`Width::digits`] decimal digits and an
//! exponent `q`, with the value `coefficient * 10^q`. Unlike a binary format the same value has
//! more than one encoding, and which one a constant gets is part of what it means: `1.20dd` is
//! the coefficient 120 with exponent -2, `1.2dd` is 12 with exponent -1, and the two compare
//! equal but print differently. The exponent a constant keeps is the one its text wrote, which is
//! IEEE's preferred exponent and what gcc gives it, and only a coefficient with too many digits
//! or an exponent out of range changes it.
//!
//! Rounding is to nearest with ties to even, the only mode a translation time constant uses.
//!
//! ```
//! use rucc_base::dfp::{self, Width};
//!
//! let (bits, status) = dfp::parse("1.20", Width::D64).expect("a number");
//! assert_eq!(dfp::decode(bits, Width::D64), dfp::Decoded::Finite { sign: false, coefficient: 120, exponent: -2 });
//! assert!(status.is_none());
//! ```

use crate::float::{ParseError, Status};

/// One of the three decimal interchange formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Width {
    /// `_Decimal32`, seven digits.
    D32,
    /// `_Decimal64`, sixteen digits.
    D64,
    /// `_Decimal128`, thirty four digits.
    D128,
}

impl Width {
    /// The size of the encoding in bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Width::D32 => 32,
            Width::D64 => 64,
            Width::D128 => 128,
        }
    }

    /// The most decimal digits a coefficient has, IEEE's `p`.
    #[must_use]
    pub const fn digits(self) -> u32 {
        match self {
            Width::D32 => 7,
            Width::D64 => 16,
            Width::D128 => 34,
        }
    }

    /// What is added to the exponent to store it, which makes the smallest exponent zero.
    #[must_use]
    pub const fn bias(self) -> i32 {
        match self {
            Width::D32 => 101,
            Width::D64 => 398,
            Width::D128 => 6176,
        }
    }

    /// The smallest exponent `q` a coefficient can have.
    #[must_use]
    pub const fn min_exponent(self) -> i32 {
        -self.bias()
    }

    /// The largest exponent `q` a coefficient can have, IEEE's `emax - p + 1`.
    #[must_use]
    pub const fn max_exponent(self) -> i32 {
        match self {
            Width::D32 => 90,
            Width::D64 => 369,
            Width::D128 => 6111,
        }
    }

    /// The bits the biased exponent is stored in.
    const fn exponent_bits(self) -> u32 {
        match self {
            Width::D32 => 8,
            Width::D64 => 10,
            Width::D128 => 14,
        }
    }

    /// The bits a coefficient is stored in when its top bits are not `100`, which is every
    /// coefficient below `2^(this)`.
    const fn coefficient_bits(self) -> u32 {
        self.bits() - 1 - self.exponent_bits()
    }

    /// The largest coefficient, `10^p - 1`.
    #[must_use]
    pub const fn max_coefficient(self) -> u128 {
        pow10(self.digits()) - 1
    }
}

/// `10^n`, for the `n` a coefficient of this module ever needs, which is at most thirty eight.
const fn pow10(n: u32) -> u128 {
    let mut value = 1u128;
    let mut i = 0;
    while i < n {
        value *= 10;
        i += 1;
    }
    value
}

/// What a decimal encoding holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    /// A number, zero included: `coefficient * 10^exponent`, negative when `sign` is set.
    Finite {
        /// The sign, which a zero has as well.
        sign: bool,
        /// The coefficient, at most [`Width::max_coefficient`].
        coefficient: u128,
        /// The exponent `q`.
        exponent: i32,
    },
    /// An infinity.
    Infinite {
        /// Which infinity.
        sign: bool,
    },
    /// A NaN, quiet or signaling.
    Nan {
        /// The sign bit, which a NaN has but which means nothing.
        sign: bool,
        /// Whether it is the signaling kind.
        signaling: bool,
    },
}

/// The sign bit of an encoding of this width.
#[must_use]
pub const fn sign_bit(width: Width) -> u128 {
    1u128 << (width.bits() - 1)
}

/// The bits of `sign * coefficient * 10^exponent`, which the caller has already brought into
/// range: the coefficient at most [`Width::max_coefficient`] and the exponent between
/// [`Width::min_exponent`] and [`Width::max_exponent`].
///
/// # Panics
///
/// If either is out of range, which is a caller that skipped the rounding.
#[must_use]
pub fn encode(sign: bool, coefficient: u128, exponent: i32, width: Width) -> u128 {
    assert!(coefficient <= width.max_coefficient(), "the coefficient was not rounded");
    assert!(
        (width.min_exponent()..=width.max_exponent()).contains(&exponent),
        "the exponent was not brought into range"
    );
    let biased = u128::try_from(exponent + width.bias()).expect("in range, so not negative");
    let small = width.coefficient_bits();
    let sign = if sign { sign_bit(width) } else { 0 };
    if coefficient < 1u128 << small {
        sign | (biased << small) | coefficient
    } else {
        // The coefficient's top three bits are `100`, which is not stored: the two bits after
        // the sign are `11`, the exponent follows them, and the rest of the coefficient after
        // that. Only the two narrower widths get here, since ten to the thirty fourth is below
        // two to the hundred and thirteenth.
        let rest = small - 2;
        sign | (0b11 << (width.bits() - 3))
            | (biased << rest)
            | (coefficient & ((1u128 << rest) - 1))
    }
}

/// An infinity of this width.
#[must_use]
pub const fn infinity(sign: bool, width: Width) -> u128 {
    let bits = 0b11110u128 << (width.bits() - 6);
    if sign { bits | sign_bit(width) } else { bits }
}

/// A quiet NaN of this width with nothing in its payload.
#[must_use]
pub const fn nan(width: Width) -> u128 {
    0b11_1110u128 << (width.bits() - 7)
}

/// What the bits of an encoding of this width mean.
///
/// A coefficient larger than [`Width::max_coefficient`] is a non-canonical encoding, which IEEE
/// says is read as zero, and it is read that way here.
#[must_use]
pub fn decode(bits: u128, width: Width) -> Decoded {
    let top = width.bits();
    let sign = bits & sign_bit(width) != 0;
    let combination = (bits >> (top - 6)) & 0b11111;
    if combination == 0b11110 {
        return Decoded::Infinite { sign };
    }
    if combination == 0b11111 {
        let signaling = (bits >> (top - 7)) & 1 != 0;
        return Decoded::Nan { sign, signaling };
    }
    let small = width.coefficient_bits();
    let field = (1u128 << width.exponent_bits()) - 1;
    let (biased, coefficient) = if (bits >> (top - 3)) & 0b11 == 0b11 {
        let rest = small - 2;
        ((bits >> rest) & field, (0b100 << rest) | (bits & ((1u128 << rest) - 1)))
    } else {
        ((bits >> small) & field, bits & ((1u128 << small) - 1))
    };
    // Fourteen bits at most, so the conversion cannot fail and the default is never read.
    let exponent = i32::try_from(biased).unwrap_or(0) - width.bias();
    let coefficient = if coefficient > width.max_coefficient() { 0 } else { coefficient };
    Decoded::Finite { sign, coefficient, exponent }
}

/// The bits of the decimal constant `text` in this width, and what had to be done to it to fit.
///
/// `text` is what a decimal floating constant has in front of its `df`, `dd` or `dl`: digits
/// with at most one point among them and an optional `e` exponent, and a sign in front if the
/// caller has one to give. A hexadecimal constant has no decimal meaning, and C23 says so, so
/// one is refused here as having a character a decimal number does not have.
///
/// # Errors
///
/// When the text has no digits, has an exponent marker with no digits after it, or has
/// anything else in it.
pub fn parse(text: &str, width: Width) -> Result<(u128, Status), ParseError> {
    let bytes = text.as_bytes();
    let (sign, rest) = match bytes.first() {
        Some(b'-') => (true, &bytes[1..]),
        Some(b'+') => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    let (mantissa, exponent_text) = match rest.iter().position(|&b| b | 32 == b'e') {
        Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        None => (rest, None),
    };

    // The significant digits, which start at the first one that is not zero, and how many
    // were written after the point, which is what makes `1.20` a different encoding from `1.2`.
    let mut digits = Vec::new();
    let mut seen_digit = false;
    let mut after_point: i64 = 0;
    let mut point = false;
    for &b in mantissa {
        match b {
            b'0'..=b'9' => {
                seen_digit = true;
                if point {
                    after_point += 1;
                }
                if b != b'0' || !digits.is_empty() {
                    digits.push(b - b'0');
                }
            }
            b'.' if !point => point = true,
            _ => return Err(ParseError::Invalid),
        }
    }
    if !seen_digit {
        return Err(ParseError::NoDigits);
    }

    let written = match exponent_text {
        None => 0,
        Some(text) => exponent(text)?,
    };
    // Clamped, since a written exponent of a billion and one of ten thousand both leave every
    // coefficient out of range, and the arithmetic below then cannot overflow.
    let limit = i64::from(width.max_exponent()) + 200;
    let mut exponent = (written - after_point).clamp(-limit * 2, limit * 2);

    let precision = width.digits() as usize;
    let mut status = Status::NONE;
    if digits.len() > precision {
        let dropped = digits.len() - precision;
        exponent += i64::try_from(dropped).unwrap_or(i64::MAX / 4).min(limit * 4);
        let up = rounds_up(&digits[..precision], &digits[precision..]);
        if digits[precision..].iter().any(|&d| d != 0) {
            status = status.with(Status::INEXACT);
        }
        digits.truncate(precision);
        let mut coefficient = value(&digits);
        if up {
            coefficient += 1;
            if coefficient > width.max_coefficient() {
                coefficient /= 10;
                exponent += 1;
            }
        }
        return Ok(fit(sign, coefficient, exponent, width, status));
    }
    Ok(fit(sign, value(&digits), exponent, width, status))
}

/// The number an exponent's text writes, saturated well past any exponent a format has.
fn exponent(text: &[u8]) -> Result<i64, ParseError> {
    let (negative, digits) = match text.first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() {
        return Err(ParseError::NoExponentDigits);
    }
    let mut value: i64 = 0;
    for &b in digits {
        if !b.is_ascii_digit() {
            return Err(ParseError::Invalid);
        }
        value = (value * 10 + i64::from(b - b'0')).min(1 << 40);
    }
    Ok(if negative { -value } else { value })
}

/// The integer a run of at most thirty eight digits writes.
fn value(digits: &[u8]) -> u128 {
    digits.iter().fold(0u128, |value, &d| value * 10 + u128::from(d))
}

/// Whether dropping `dropped` off the end of `kept` rounds the kept digits up, to nearest with
/// ties to even.
fn rounds_up(kept: &[u8], dropped: &[u8]) -> bool {
    match dropped.first() {
        Some(&first) if first > 5 => true,
        Some(5) => dropped[1..].iter().any(|&d| d != 0) || kept.last().is_some_and(|&d| d % 2 == 1),
        _ => false,
    }
}

/// The encoding of `coefficient * 10^exponent`, whose coefficient already has few enough
/// digits, with the exponent brought into range.
///
/// An exponent above the range is brought down by adding zeros to the coefficient while there
/// is room for them, which is IEEE's clamping and changes nothing about the value, and past that
/// the number is too large and is an infinity. An exponent below the range drops digits off the
/// coefficient, rounding, until it is in range, which is where subnormal numbers and underflow
/// to zero come from. A zero keeps its sign and takes the nearest exponent in range.
fn fit(
    sign: bool,
    mut coefficient: u128,
    mut exponent: i64,
    width: Width,
    mut status: Status,
) -> (u128, Status) {
    let min = i64::from(width.min_exponent());
    let max = i64::from(width.max_exponent());
    if coefficient == 0 {
        let exponent = i32::try_from(exponent.clamp(min, max)).expect("clamped into range");
        return (encode(sign, 0, exponent, width), status);
    }
    while exponent > max && coefficient * 10 <= width.max_coefficient() {
        coefficient *= 10;
        exponent -= 1;
    }
    if exponent > max {
        return (infinity(sign, width), status.with(Status::OVERFLOW).with(Status::INEXACT));
    }
    if exponent < min {
        let mut remainder_nonzero = false;
        let mut last = 0u128;
        let mut first = true;
        let mut steps = 0;
        while exponent < min {
            if !first && last != 0 {
                remainder_nonzero = true;
            }
            first = false;
            last = coefficient % 10;
            coefficient /= 10;
            exponent += 1;
            steps += 1;
            if coefficient == 0 && steps > 40 {
                exponent = min;
            }
        }
        // `last` is the most significant digit dropped and `remainder_nonzero` says whether
        // anything below it was not zero.
        let up = last > 5 || (last == 5 && (remainder_nonzero || coefficient % 2 == 1));
        if last != 0 || remainder_nonzero {
            status = status.with(Status::INEXACT).with(Status::UNDERFLOW);
        }
        if up {
            coefficient += 1;
        }
    }
    let exponent = i32::try_from(exponent).expect("brought into range");
    (encode(sign, coefficient, exponent, width), status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(text: &str, width: Width) -> u128 {
        parse(text, width).expect("a number").0
    }

    fn finite(text: &str, width: Width) -> (bool, u128, i32) {
        match decode(bits(text, width), width) {
            Decoded::Finite { sign, coefficient, exponent } => (sign, coefficient, exponent),
            other => panic!("{text} is {other:?}"),
        }
    }

    /// The encodings gcc 16 gives these constants on x86-64, printed from a program it compiled.
    #[test]
    fn the_bits_are_the_ones_gcc_writes() {
        assert_eq!(bits("0.", Width::D64), 0x31c0_0000_0000_0000);
        assert_eq!(bits("-0.", Width::D64), 0xb1c0_0000_0000_0000);
        assert_eq!(bits("1", Width::D64), 0x31c0_0000_0000_0001);
        assert_eq!(bits("1.5", Width::D32), 0x3200_000f);
        assert_eq!(bits("1", Width::D128), 0x3040_0000_0000_0000_0000_0000_0000_0001);
        assert_eq!(bits("9999999", Width::D32), 0x6cb8_967f);
        assert_eq!(bits("1.20", Width::D64), 0x3180_0000_0000_0078);
        assert_eq!(bits("12345675", Width::D32), 0x3312_d688);
        assert_eq!(bits("99999995", Width::D32), 0x338f_4240);
        assert_eq!(bits("1e96", Width::D32), 0x5f8f_4240);
        assert_eq!(bits("15e-102", Width::D32), 0x0000_0002);
        assert_eq!(bits("0e999", Width::D64), 0x5fe0_0000_0000_0000);
        assert_eq!(bits("0.000", Width::D32), 0x3100_0000);
    }

    #[test]
    fn a_constant_keeps_the_exponent_its_text_wrote() {
        assert_eq!(finite("1.20", Width::D64), (false, 120, -2));
        assert_eq!(finite("1.2", Width::D64), (false, 12, -1));
        assert_eq!(finite("12e3", Width::D64), (false, 12, 3));
        assert_eq!(finite("0.000", Width::D32), (false, 0, -3));
        assert_eq!(finite("-00012.5E-1", Width::D128), (true, 125, -2));
    }

    #[test]
    fn too_many_digits_round_to_nearest_with_ties_to_even() {
        assert_eq!(finite("12345675", Width::D32), (false, 1_234_568, 1));
        assert_eq!(finite("12345665", Width::D32), (false, 1_234_566, 1));
        assert_eq!(finite("123456650001", Width::D32), (false, 1_234_567, 5));
        assert_eq!(finite("99999995", Width::D32), (false, 1_000_000, 2));
        let (_, status) = parse("12345675", Width::D32).expect("a number");
        assert!(status.has(Status::INEXACT));
        let (_, status) = parse("12345670", Width::D32).expect("a number");
        assert!(status.is_none(), "only zeros were dropped");
    }

    #[test]
    fn an_exponent_out_of_range_is_clamped_overflows_or_underflows() {
        assert_eq!(finite("1e96", Width::D32), (false, 1_000_000, 90));
        let (value, status) = parse("1e97", Width::D32).expect("a number");
        assert_eq!(value, infinity(false, Width::D32));
        assert!(status.has(Status::OVERFLOW));
        assert_eq!(finite("1e-101", Width::D32), (false, 1, -101));
        let (value, status) = parse("15e-102", Width::D32).expect("a number");
        assert_eq!(
            decode(value, Width::D32),
            Decoded::Finite { sign: false, coefficient: 2, exponent: -101 }
        );
        assert!(status.has(Status::UNDERFLOW));
        let (value, _) = parse("1e-200", Width::D32).expect("a number");
        assert_eq!(
            decode(value, Width::D32),
            Decoded::Finite { sign: false, coefficient: 0, exponent: -101 }
        );
        assert_eq!(finite("0e999999999999", Width::D64), (false, 0, 369));
    }

    #[test]
    fn every_width_round_trips_its_largest_coefficient() {
        for width in [Width::D32, Width::D64, Width::D128] {
            for exponent in [width.min_exponent(), 0, width.max_exponent()] {
                for sign in [false, true] {
                    let max = width.max_coefficient();
                    let bits = encode(sign, max, exponent, width);
                    assert_eq!(
                        decode(bits, width),
                        Decoded::Finite { sign, coefficient: max, exponent }
                    );
                }
            }
        }
    }

    #[test]
    fn infinities_and_nans_read_back_as_themselves() {
        for width in [Width::D32, Width::D64, Width::D128] {
            assert_eq!(decode(infinity(true, width), width), Decoded::Infinite { sign: true });
            assert_eq!(decode(nan(width), width), Decoded::Nan { sign: false, signaling: false });
        }
        assert_eq!(infinity(false, Width::D64), 0x7800_0000_0000_0000);
        assert_eq!(nan(Width::D64), 0x7c00_0000_0000_0000);
    }

    #[test]
    fn text_that_is_not_a_decimal_number_is_refused() {
        assert_eq!(parse("", Width::D64), Err(ParseError::NoDigits));
        assert_eq!(parse(".", Width::D64), Err(ParseError::NoDigits));
        assert_eq!(parse("1e", Width::D64), Err(ParseError::NoExponentDigits));
        assert_eq!(parse("0x1p3", Width::D64), Err(ParseError::Invalid));
        assert_eq!(parse("1.2.3", Width::D64), Err(ParseError::Invalid));
    }
}
