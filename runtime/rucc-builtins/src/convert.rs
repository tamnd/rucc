//! Converting between a 128-bit integer and a float, which is the reference for the eight entry
//! points in `runtime/builtins/convert.c`.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8. The names and the conventions are libgcc's,
//! the same as everything else here.
//!
//! # Why this is a different algorithm on purpose
//!
//! The C takes the top sixty four bits of the integer, records what fell off as the lowest bit of
//! them, and hands that to the machine's own conversion, which is the version a reader can check by
//! reading: two roundings that provably come to the same answer as one. This builds the float out
//! of its bits instead. It finds the highest set bit, shifts the mantissa into place, works out
//! whether to round up by looking at the part that does not fit, and assembles the sign, the
//! exponent and the mantissa itself. Going the other way it reads the exponent and the mantissa out
//! of the bits and shifts them rather than dividing by 2^64 and subtracting.
//!
//! So neither side borrows the other's reasoning, which is the point of holding one against the
//! other. A reference that shares an algorithm with the thing it checks only catches typing
//! mistakes.
//!
//! # Why there is no `as` between the two widths in it
//!
//! A cast between a 128-bit integer and a float is a call to one of the names in this file, so a
//! conversion written the obvious way would be this function calling itself forever. Nothing below
//! converts anything: the floats are built from and taken apart as `u32` and `u64` bit patterns,
//! and every shift is on a `u128`.
//!
//! # What is not handled
//!
//! The same three things the C does not handle, and for the same reason: a value the integer cannot
//! hold, an infinity and a not a number are all undefined in C, so there is no answer for two
//! implementations to agree about. Zero is what comes out of each of them here, which is the shape
//! the C's own guard happens to have, and a negative value handed to an unsigned conversion is zero
//! for the same reason.

/// One binary float format, as the two numbers that describe where its fields are.
///
/// The bias follows from the width of the exponent, so it is worked out rather than written down
/// twice.
struct Format {
    /// How many bits the stored mantissa takes, which is one fewer than its precision because the
    /// leading bit of a normal value is not stored.
    mantissa: u32,
    /// How many bits the stored exponent takes.
    exponent: u32,
}

/// `f32`, as the format.
const SINGLE: Format = Format { mantissa: 23, exponent: 8 };

/// `f64`, as the format.
const DOUBLE: Format = Format { mantissa: 52, exponent: 11 };

impl Format {
    /// What is added to an exponent to store it.
    const fn bias(&self) -> i32 {
        (1 << (self.exponent - 1)) - 1
    }

    /// The largest exponent a finite value of this format has.
    const fn top(&self) -> i32 {
        self.bias()
    }

    /// The bit pattern of an infinity of this format, without the sign.
    const fn infinity(&self) -> u64 {
        ((1u64 << self.exponent) - 1) << self.mantissa
    }
}

/// The bit pattern of the float of this format nearest to `value`, ties to even.
///
/// Zero is its own case because there is no highest set bit to put an exponent around. Everything
/// else has one, and where it lands says both the exponent and how far the mantissa has to move:
/// down when the value has more significant bits than the format holds, which is the case that
/// rounds, and up when it has fewer, which is the case that is exact.
fn from_unsigned(value: u128, format: &Format) -> u64 {
    if value == 0 {
        return 0;
    }
    let highest = 127 - value.leading_zeros() as i32;
    let wanted = format.mantissa as i32;
    let (mut mantissa, up) = if highest > wanted {
        let by = (highest - wanted) as u32;
        let kept = value >> by;
        let dropped = value & ((1u128 << by) - 1);
        let half = 1u128 << (by - 1);
        // Ties to even: exactly half goes to whichever of the two neighbours has an even mantissa,
        // which is to say it only rounds up when the kept value is odd.
        (kept, dropped > half || (dropped == half && (kept & 1) == 1))
    } else {
        (value << (wanted - highest) as u32, false)
    };
    let mut exponent = highest;
    if up {
        mantissa += 1;
        // Rounding up can carry out of the mantissa, and when it does the value is the next power
        // of two, whose mantissa is all zeroes one exponent higher.
        if mantissa >> (format.mantissa + 1) != 0 {
            mantissa >>= 1;
            exponent += 1;
        }
    }
    if exponent > format.top() {
        return format.infinity();
    }
    let stored = (exponent + format.bias()) as u64;
    let fraction = (mantissa & ((1u128 << format.mantissa) - 1)) as u64;
    (stored << format.mantissa) | fraction
}

/// The value of the float `bits` stands for, truncated towards zero, or zero where there is no
/// value to speak of.
///
/// The mantissa with its leading bit put back is an integer scaled by 2^(exponent - mantissa bits),
/// so the whole of this is that scaling: up when the value is large enough to have no fraction, and
/// down when it has one, where shifting out the low bits is exactly the truncation C asks for.
fn to_unsigned(bits: u64, format: &Format) -> u128 {
    let sign = (bits >> (format.mantissa + format.exponent)) & 1;
    let stored = (bits >> format.mantissa) & ((1 << format.exponent) - 1);
    let fraction = bits & ((1 << format.mantissa) - 1);
    // An exponent of all ones is an infinity or a not a number, and one of all zeroes is a zero or
    // a subnormal, which is below one and therefore truncates to zero anyway.
    if sign == 1 || stored == 0 || stored == (1 << format.exponent) - 1 {
        return 0;
    }
    let exponent = stored as i32 - format.bias();
    // A value of 2^128 or more is past the type, which is the undefined case, and a value below one
    // has nothing left after the truncation.
    if exponent >= 128 {
        return 0;
    }
    if exponent < 0 {
        return 0;
    }
    let mantissa = (1u128 << format.mantissa) | u128::from(fraction);
    let by = exponent - format.mantissa as i32;
    if by >= 0 {
        mantissa << by as u32
    } else if -by >= 128 {
        0
    } else {
        mantissa >> (-by) as u32
    }
}

/// A signed value as its magnitude, taken as an unsigned one so that the most negative value of the
/// type has a magnitude at all.
fn magnitude(value: i128) -> (bool, u128) {
    if value < 0 { (true, (value as u128).wrapping_neg()) } else { (false, value as u128) }
}

/// The bit pattern with the sign bit set, which is what a negative magnitude comes out as.
fn negate(bits: u64, format: &Format) -> u64 {
    bits | (1u64 << (format.mantissa + format.exponent))
}

// The C names. Not compiled under `cargo test`, where a cast between a 128-bit integer and a float
// in the tests themselves is a call to these, and the host's own copies are the ones that should
// answer it.

/// `double __floatuntidf(unsigned __int128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatuntidf(value: u128) -> f64 {
    f64::from_bits(from_unsigned(value, &DOUBLE))
}

/// `float __floatuntisf(unsigned __int128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floatuntisf(value: u128) -> f32 {
    f32::from_bits(from_unsigned(value, &SINGLE) as u32)
}

/// `double __floattidf(__int128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floattidf(value: i128) -> f64 {
    let (negative, magnitude) = magnitude(value);
    let bits = from_unsigned(magnitude, &DOUBLE);
    f64::from_bits(if negative { negate(bits, &DOUBLE) } else { bits })
}

/// `float __floattisf(__int128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __floattisf(value: i128) -> f32 {
    let (negative, magnitude) = magnitude(value);
    let bits = from_unsigned(magnitude, &SINGLE);
    f32::from_bits(if negative { negate(bits, &SINGLE) } else { bits } as u32)
}

/// `unsigned __int128 __fixunsdfti(double)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixunsdfti(value: f64) -> u128 {
    to_unsigned(value.to_bits(), &DOUBLE)
}

/// `unsigned __int128 __fixunssfti(float)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixunssfti(value: f32) -> u128 {
    to_unsigned(u64::from(value.to_bits()), &SINGLE)
}

/// `__int128 __fixdfti(double)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixdfti(value: f64) -> i128 {
    let bits = value.to_bits();
    let magnitude = to_unsigned(bits & !(1u64 << 63), &DOUBLE);
    if bits >> 63 == 1 { magnitude.wrapping_neg() as i128 } else { magnitude as i128 }
}

/// `__int128 __fixsfti(float)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __fixsfti(value: f32) -> i128 {
    let bits = value.to_bits();
    let magnitude = to_unsigned(u64::from(bits & !(1u32 << 31)), &SINGLE);
    if bits >> 31 == 1 { magnitude.wrapping_neg() as i128 } else { magnitude as i128 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pseudorandom stream, so the values below are the same on every machine and a failure is
    /// one anybody can reproduce. xorshift64, and nothing rests on its quality.
    struct Stream(u64);

    impl Stream {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// A value whose significant bits are a random width up to 128, which is what makes the
        /// rounding path and the exact path both get used.
        fn value(&mut self) -> u128 {
            let width = (self.next() % 129) as u32;
            if width == 0 {
                return 0;
            }
            let whole = u128::from(self.next()) << 64 | u128::from(self.next());
            whole >> (128 - width)
        }
    }

    /// The host's own conversion is the expectation here, which is the one place in this crate
    /// where that is available: under `cargo test` the entry points above are not compiled, so
    /// `as` is the compiler's own lowering rather than a call into this file.
    #[test]
    fn a_random_value_becomes_the_float_the_host_makes_of_it() {
        let mut stream = Stream(0x9E3779B97F4A7C15);
        for _ in 0..20000 {
            let value = stream.value();
            assert_eq!(f64::from_bits(from_unsigned(value, &DOUBLE)), value as f64, "{value}");
            assert_eq!(
                f32::from_bits(from_unsigned(value, &SINGLE) as u32),
                value as f32,
                "{value}"
            );
        }
    }

    /// The values where the rounding is decided by something other than the bits: a power of two,
    /// one either side of it, and the halfway cases just past the precision of each format.
    #[test]
    fn the_values_rounding_is_decided_at_come_out_the_same_way() {
        for shift in 0..128u32 {
            for step in [0u128, 1, 2, 3] {
                let value = (1u128 << shift).wrapping_add(step);
                assert_eq!(f64::from_bits(from_unsigned(value, &DOUBLE)), value as f64, "{value}");
                assert_eq!(
                    f32::from_bits(from_unsigned(value, &SINGLE) as u32),
                    value as f32,
                    "{value}"
                );
                let value = (1u128 << shift).wrapping_sub(step);
                assert_eq!(f64::from_bits(from_unsigned(value, &DOUBLE)), value as f64, "{value}");
                assert_eq!(
                    f32::from_bits(from_unsigned(value, &SINGLE) as u32),
                    value as f32,
                    "{value}"
                );
            }
        }
    }

    /// A value that does not fit in the format at all, which is every `u128` above what a `f32`
    /// holds, becomes an infinity rather than the largest finite value.
    #[test]
    fn a_value_too_large_for_the_format_becomes_an_infinity() {
        assert!(f32::from_bits(from_unsigned(u128::MAX, &SINGLE) as u32).is_infinite());
        assert_eq!(f32::from_bits(from_unsigned(u128::MAX, &SINGLE) as u32), u128::MAX as f32);
        assert!(f64::from_bits(from_unsigned(u128::MAX, &DOUBLE)).is_finite());
    }

    /// Coming back down, over the floats a round trip makes and over the ones with a fraction in
    /// them, which is where the truncation towards zero is decided.
    #[test]
    fn a_float_becomes_the_integer_the_host_truncates_it_to() {
        let mut stream = Stream(0x243F6A8885A308D3);
        for _ in 0..20000 {
            let value = stream.value();
            let as_double = value as f64;
            // Only where the float is back inside the type, since a value above it is undefined in
            // C and the two implementations have no answer to agree about.
            if as_double < 340282366920938463463374607431768211456.0 {
                assert_eq!(to_unsigned(as_double.to_bits(), &DOUBLE), as_double as u128, "{value}");
            }
            let scaled = as_double / 3.0;
            assert_eq!(to_unsigned(scaled.to_bits(), &DOUBLE), scaled as u128, "{scaled}");
            // Every finite `f32` is inside a `u128` already, since the largest one is just under
            // 2^128, so there is no range check to make here beyond the one for an infinity.
            let small = f32::from_bits(stream.next() as u32);
            if small.is_finite() && small >= 0.0 {
                assert_eq!(
                    to_unsigned(u64::from(small.to_bits()), &SINGLE),
                    small as u128,
                    "{small}"
                );
            }
        }
    }

    /// The three values that have no answer come out as zero rather than as whatever the shifts
    /// left behind, which is what the C does and therefore what the comparison needs.
    #[test]
    fn a_value_with_no_answer_comes_out_as_zero() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, -0.0, 0.5, -0.5] {
            assert_eq!(to_unsigned(value.to_bits(), &DOUBLE), 0, "{value}");
        }
        assert_eq!(to_unsigned(f64::MAX.to_bits(), &DOUBLE), 0, "above the type");
    }

    /// A negative value keeps its magnitude and its sign through both directions, the most negative
    /// value of the type included, which is the one that has no positive of its own.
    #[test]
    fn the_most_negative_value_survives_both_directions() {
        let lowest = i128::MIN;
        let (negative, magnitude) = magnitude(lowest);
        assert!(negative);
        assert_eq!(magnitude, 1u128 << 127);
        let bits = negate(from_unsigned(magnitude, &DOUBLE), &DOUBLE);
        assert_eq!(f64::from_bits(bits), lowest as f64);
        let back = to_unsigned(from_unsigned(magnitude, &DOUBLE), &DOUBLE);
        assert_eq!(back.wrapping_neg() as i128, lowest);
    }
}
