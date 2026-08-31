//! An exact decimal number that can be halved and doubled, which is all a correctly rounded
//! conversion to binary needs.
//!
//! The number is a run of decimal digits and a position for the point, so the value is
//! `0.d0d1d2... * 10^point`. The only operations are multiplying and dividing by a power of
//! two and reading the integer part back out, and between them they are enough to convert a
//! decimal string to the nearest binary float without ever dividing one long number by
//! another. That is the algorithm Go's `strconv` uses and the one Rust's own parser falls back
//! to, and it is here rather than either of those because a compiler cannot borrow the host's
//! answer: the same source has to produce the same bits on every machine.
//!
//! The digit buffer has a ceiling. Digits past it are dropped and remembered as [`truncated`],
//! which is enough to round correctly, because rounding needs the exact value only to break a
//! tie and a tie is a number whose decimal expansion is finite and shorter than the ceiling.
//! The longest such expansion belongs to the smallest subnormal of the widest format here,
//! which is under five thousand digits.
//!
//! [`truncated`]: Decimal::truncated

/// The most digits kept. Past this the value is rounded down and remembered as truncated.
///
/// A halfway case for `binary128` is an odd multiple of a power of two no smaller than
/// 2^-16495, and the exact decimal expansion of that has 4966 significant digits, so a
/// buffer this size can always tell a tie from a value just above or below one.
const MAX_DIGITS: usize = 5120;

/// The largest shift a single pass can do, chosen so the arithmetic stays inside a `u64`.
///
/// A digit multiplied by 2^60 and carried is at most `10 * 2^60`, which is under `u64::MAX`.
const MAX_SHIFT: i32 = 60;

/// Where a value sits between the two integers around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fraction {
    /// The value is an integer.
    Zero,
    /// Nearer the integer below.
    BelowHalf,
    /// Exactly between the two, which is the case rounding to even is for.
    Half,
    /// Nearer the integer above.
    AboveHalf,
}

/// A decimal number, as digits and a decimal point position.
#[derive(Debug, Clone)]
pub(crate) struct Decimal {
    /// The significant digits, each 0 to 9, with no leading and no trailing zero.
    digits: Vec<u8>,
    /// The value is `0.digits * 10^point`.
    point: i32,
    /// Whether a nonzero digit was dropped off the end.
    truncated: bool,
    /// Scratch, so that a shift does not allocate on every pass.
    scratch: Vec<u8>,
}

impl Decimal {
    /// Builds a decimal from digit values and a point position, trimming the zeros that do not
    /// mean anything.
    pub(crate) fn new(digits: Vec<u8>, point: i32) -> Decimal {
        let mut decimal = Decimal { digits, point, truncated: false, scratch: Vec::new() };
        decimal.trim();
        decimal
    }

    /// Whether the value is zero, which is the one value with no digits.
    pub(crate) fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    /// The point position, so that the value is `0.digits * 10^point`.
    pub(crate) fn point(&self) -> i32 {
        self.point
    }

    /// The leading digit, which is never zero unless the value is.
    pub(crate) fn first_digit(&self) -> u8 {
        self.digits.first().copied().unwrap_or(0)
    }

    /// Multiplies by 2^`k`, or divides when `k` is negative, in passes small enough to stay in
    /// a `u64`.
    pub(crate) fn shift(&mut self, mut k: i32) {
        while k > 0 && !self.is_zero() {
            let pass = k.min(MAX_SHIFT);
            self.shift_left(pass as u32);
            k -= pass;
        }
        while k < 0 && !self.is_zero() {
            let pass = (-k).min(MAX_SHIFT);
            self.shift_right(pass as u32);
            k += pass;
        }
    }

    /// The integer part, and where the rest of the value sits between it and the next integer.
    ///
    /// The integer part is expected to be small, because the caller has already scaled the
    /// value down to the width of the format it is converting to. A value too large for a
    /// `u128` saturates rather than wrapping, which no caller here can reach.
    pub(crate) fn round_to_u128(&self) -> (u128, Fraction) {
        let taken = self.point.clamp(0, self.digits.len() as i32) as usize;
        let mut integer: u128 = 0;
        for &digit in &self.digits[..taken] {
            integer = integer.saturating_mul(10).saturating_add(u128::from(digit));
        }
        // A point past the last digit means trailing zeros that were trimmed away.
        for _ in taken as i32..self.point {
            integer = integer.saturating_mul(10);
        }

        let rest = &self.digits[taken.min(self.digits.len())..];
        if self.point < 0 || rest.is_empty() {
            // Every remaining digit is below the tenths place, or there is none at all.
            let anything = !rest.is_empty() || self.truncated;
            return (integer, if anything { Fraction::BelowHalf } else { Fraction::Zero });
        }
        let leading = rest[0];
        let more = rest[1..].iter().any(|&digit| digit != 0) || self.truncated;
        let fraction = match leading {
            6..=9 => Fraction::AboveHalf,
            5 if more => Fraction::AboveHalf,
            5 => Fraction::Half,
            0 if !more => Fraction::Zero,
            _ => Fraction::BelowHalf,
        };
        (integer, fraction)
    }

    /// Multiplies by 2^`k` for a `k` small enough that a digit times 2^`k` fits in a `u64`.
    fn shift_left(&mut self, k: u32) {
        let mut carry: u64 = 0;
        self.scratch.clear();
        for &digit in self.digits.iter().rev() {
            let value = (u64::from(digit) << k) + carry;
            self.scratch.push((value % 10) as u8);
            carry = value / 10;
        }
        while carry > 0 {
            self.scratch.push((carry % 10) as u8);
            carry /= 10;
        }
        self.scratch.reverse();
        self.point += (self.scratch.len() - self.digits.len()) as i32;
        std::mem::swap(&mut self.digits, &mut self.scratch);
        self.trim();
    }

    /// Divides by 2^`k`, long division in base ten with the remainder carried digit to digit.
    fn shift_right(&mut self, k: u32) {
        let divisor: u64 = 1 << k;
        let mask = divisor - 1;
        self.scratch.clear();

        // Take digits until there is something to divide, which is what moves the point.
        let mut read = 0;
        let mut remainder: u64 = 0;
        while remainder >> k == 0 {
            if read >= self.digits.len() {
                if remainder == 0 {
                    // Cannot happen for a trimmed nonzero number, and spins forever if it did.
                    self.digits.clear();
                    self.point = 0;
                    return;
                }
                while remainder >> k == 0 {
                    remainder *= 10;
                    read += 1;
                }
                break;
            }
            remainder = remainder * 10 + u64::from(self.digits[read]);
            read += 1;
        }
        self.point -= read as i32 - 1;

        while read < self.digits.len() {
            let digit = u64::from(self.digits[read]);
            self.scratch.push((remainder >> k) as u8);
            remainder = (remainder & mask) * 10 + digit;
            read += 1;
        }
        while remainder > 0 {
            let digit = (remainder >> k) as u8;
            if self.scratch.len() < MAX_DIGITS {
                self.scratch.push(digit);
            } else if digit > 0 {
                self.truncated = true;
            }
            remainder = (remainder & mask) * 10;
        }

        std::mem::swap(&mut self.digits, &mut self.scratch);
        self.trim();
    }

    /// Drops leading zeros, which move the point, and trailing zeros and excess digits, which
    /// do not, and notes when a dropped digit meant something.
    fn trim(&mut self) {
        let leading = self.digits.iter().take_while(|&&digit| digit == 0).count();
        if leading > 0 {
            self.digits.drain(..leading);
            self.point -= leading as i32;
        }
        while self.digits.last() == Some(&0) {
            self.digits.pop();
        }
        if self.digits.len() > MAX_DIGITS {
            if self.digits[MAX_DIGITS..].iter().any(|&digit| digit != 0) {
                self.truncated = true;
            }
            self.digits.truncate(MAX_DIGITS);
            while self.digits.last() == Some(&0) {
                self.digits.pop();
            }
        }
        if self.digits.is_empty() && !self.truncated {
            self.point = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decimal(digits: &str, point: i32) -> Decimal {
        Decimal::new(digits.bytes().map(|byte| byte - b'0').collect(), point)
    }

    /// The digits and the point, as a string, so that a test reads like the number it is about.
    fn spell(value: &Decimal) -> String {
        let digits: String =
            value.digits.iter().map(|&digit| char::from(b'0' + digit)).collect::<String>();
        format!("0.{digits}e{}", value.point)
    }

    #[test]
    fn zeros_that_do_not_mean_anything_are_dropped() {
        assert_eq!(spell(&decimal("00123400", 5)), "0.1234e3");
        assert!(decimal("0000", 7).is_zero());
        assert_eq!(spell(&decimal("1", 1)), "0.1e1");
    }

    #[test]
    fn doubling_and_halving_give_back_the_number() {
        let mut value = decimal("123456789", 5);
        let before = spell(&value);
        value.shift(60);
        value.shift(-60);
        assert_eq!(spell(&value), before);
    }

    #[test]
    fn halving_is_exact_however_long_it_takes() {
        // A halving adds a digit every time, and the digits are the ones long division gives.
        let mut value = decimal("1", 1);
        value.shift(-1);
        assert_eq!(spell(&value), "0.5e0");
        value.shift(-1);
        assert_eq!(spell(&value), "0.25e0");
        value.shift(-2);
        assert_eq!(spell(&value), "0.625e-1");
    }

    #[test]
    fn doubling_carries_across_the_whole_number() {
        let mut value = decimal("5", 1);
        value.shift(1);
        assert_eq!(spell(&value), "0.1e2");
        let mut big = decimal("999999999999999999999999", 24);
        big.shift(1);
        assert_eq!(spell(&big), "0.1999999999999999999999998e25");
    }

    #[test]
    fn a_shift_larger_than_one_pass_is_still_one_number() {
        let mut value = decimal("1", 1);
        value.shift(200);
        // 2^200, which is the number every arbitrary precision library is tested with.
        assert_eq!(
            spell(&value),
            "0.1606938044258990275541962092341162602522202993782792835301376e61"
        );
    }

    #[test]
    fn the_integer_part_comes_back_with_where_the_rest_sits() {
        assert_eq!(decimal("125", 1).round_to_u128(), (1, Fraction::BelowHalf));
        assert_eq!(decimal("15", 1).round_to_u128(), (1, Fraction::Half));
        assert_eq!(decimal("1500001", 1).round_to_u128(), (1, Fraction::AboveHalf));
        assert_eq!(decimal("175", 1).round_to_u128(), (1, Fraction::AboveHalf));
        assert_eq!(decimal("1", 1).round_to_u128(), (1, Fraction::Zero));
        assert_eq!(decimal("1", 3).round_to_u128(), (100, Fraction::Zero));
        assert_eq!(decimal("9", 0).round_to_u128(), (0, Fraction::AboveHalf));
        assert_eq!(decimal("9", -3).round_to_u128(), (0, Fraction::BelowHalf));
    }

    #[test]
    fn a_number_longer_than_the_buffer_is_remembered_as_truncated() {
        let long = "1".repeat(MAX_DIGITS + 10);
        let value = decimal(&long, 1);
        assert!(value.truncated);
        assert_eq!(value.digits.len(), MAX_DIGITS);
        // Which is what turns an apparent tie into the number just above one.
        let mut tie = "5".to_string();
        tie.push_str(&"0".repeat(MAX_DIGITS));
        tie.push('1');
        assert_eq!(decimal(&tie, 0).round_to_u128(), (0, Fraction::AboveHalf));
    }

    #[test]
    fn trailing_zeros_past_the_buffer_are_not_a_truncation() {
        let mut padded = "5".to_string();
        padded.push_str(&"0".repeat(MAX_DIGITS * 2));
        let value = decimal(&padded, 0);
        assert!(!value.truncated);
        assert_eq!(value.round_to_u128(), (0, Fraction::Half));
    }
}
