//! Dividing an integer the machine has no instruction for, which is the reference for the twelve
//! entry points in `runtime/builtins/div.c`: six at 128 bits, which every 64-bit target reaches
//! from ordinary C, and six at 64 bits, which a 32-bit target reaches the same way.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8. The names and the conventions are libgcc's,
//! the same as everything else here.
//!
//! # Why this is a different algorithm on purpose
//!
//! The C is a bit at a time: 128 rounds of shift, compare and subtract, which is the version a
//! reader can check by reading. This is long division in base 2^32, four digits of a dividend
//! against four digits of a divisor, with the normalization step and the add-back that make a
//! single-digit estimate of each quotient digit safe. It is Knuth's algorithm D, and it is here
//! rather than a second copy of the bit loop because a reference that shares its reasoning with
//! the thing it checks only catches typing mistakes. These two can only agree by being right.
//!
//! # Why there is no `/` in it
//!
//! On a 128-bit value `/` is a call to `__udivti3`, and this crate *defines* `__udivti3`, so a
//! division written the obvious way here would be this function calling itself forever. Every
//! division below is on a `u64` or smaller, which every target the ladder names does in
//! instructions. That constraint is what picks base 2^32: the estimate step divides a two-digit
//! number by a one-digit number, and two digits of 32 bits is the 64 bits the machine has.

/// How many base 2^32 digits a 128-bit value takes.
const DIGITS: usize = 4;

/// The base, as the wider type the estimate and the multiply steps work in.
const BASE: u64 = 1 << 32;

/// A 128-bit value as digits, lowest first, which is the order the carries run in.
type Digits = [u32; DIGITS];

/// The digits of `value`, lowest first.
fn split(value: u128) -> Digits {
    let mut out = [0; DIGITS];
    for (at, digit) in out.iter_mut().enumerate() {
        *digit = (value >> (32 * at)) as u32;
    }
    out
}

/// The value those digits stand for.
fn join(digits: Digits) -> u128 {
    let mut out = 0;
    for (at, digit) in digits.iter().enumerate() {
        out |= u128::from(*digit) << (32 * at);
    }
    out
}

/// How many digits are significant, which is the index of the highest nonzero one plus one, and
/// zero for a value of zero.
fn used(digits: &Digits) -> usize {
    let mut n = DIGITS;
    while n > 0 && digits[n - 1] == 0 {
        n -= 1;
    }
    n
}

/// `digits` shifted up by `by` bits, into one more digit than it came in, so nothing falls off
/// the top.
///
/// `by` is below 32. Writing the other half of each digit as a separate case rather than shifting
/// by `32 - by`, because a shift by 32 of a `u32` is not zero in Rust, it is a panic.
fn shift_up(digits: &Digits, by: u32) -> [u32; DIGITS + 1] {
    let mut out = [0; DIGITS + 1];
    let mut carry = 0;
    for at in 0..DIGITS {
        out[at] = (digits[at] << by) | carry;
        carry = if by == 0 { 0 } else { digits[at] >> (32 - by) };
    }
    out[DIGITS] = carry;
    out
}

/// The low `DIGITS` digits of `digits` shifted back down by `by` bits, as a value.
fn shift_down(digits: &[u32; DIGITS + 1], by: u32) -> u128 {
    let mut out = [0; DIGITS];
    for at in 0..DIGITS {
        let above = if by == 0 { 0 } else { digits[at + 1] << (32 - by) };
        out[at] = (digits[at] >> by) | above;
    }
    join(out)
}

/// The quotient and the remainder of `top` divided by `bottom`.
///
/// Dividing by zero is undefined in C and nothing asks for it here. A divisor of zero has no
/// digits, and the loops below are indexed by how many it has, so the early return is there to
/// keep that indexing sound rather than to hand back an answer the language says exists.
pub fn divide(top: u128, bottom: u128) -> (u128, u128) {
    let a = split(top);
    let b = split(bottom);
    let m = used(&a);
    let n = used(&b);

    if n == 0 {
        return (0, 0);
    }
    // One digit of divisor, so there is nothing to estimate: each step is a two-digit number
    // divided by a one-digit number, which is the `u64` division the machine has.
    if n == 1 {
        let by = u64::from(b[0]);
        let mut quotient = [0; DIGITS];
        let mut rest: u64 = 0;
        for at in (0..DIGITS).rev() {
            let here = (rest << 32) | u64::from(a[at]);
            quotient[at] = (here / by) as u32;
            rest = here % by;
        }
        return (join(quotient), u128::from(rest));
    }
    // A shorter dividend than divisor divides to nothing and is its own remainder. This is also
    // what keeps `m - n` below from going negative.
    if m < n {
        return (0, top);
    }

    // Normalize, which means shift both until the divisor's top digit has its high bit set. That
    // is the condition under which the one-digit estimate below is never more than one too big,
    // and the shift is the same on both sides so the quotient does not change. The remainder
    // comes back out of it at the end.
    let by = b[n - 1].leading_zeros();
    let mut u = shift_up(&a, by);
    let v = shift_up(&b, by);
    let mut quotient = [0; DIGITS];

    for j in (0..=m - n).rev() {
        // The estimate: the top two digits of what is left over the top digit of the divisor.
        let here = (u64::from(u[j + n]) << 32) | u64::from(u[j + n - 1]);
        let mut guess = here / u64::from(v[n - 1]);
        let mut rest = here % u64::from(v[n - 1]);
        // Knuth's correction, which brings an estimate that is one or two too big back down by
        // looking at one more digit. The first test has to come first: past it the multiply below
        // would not fit in a `u64`.
        while guess >= BASE || guess * u64::from(v[n - 2]) > (rest << 32) | u64::from(u[j + n - 2])
        {
            guess -= 1;
            rest += u64::from(v[n - 1]);
            if rest >= BASE {
                break;
            }
        }

        // Multiply the divisor by the estimate and take it off the top of what is left. The
        // borrow is signed and shifted arithmetically, so it is 0 or -1 and adding it is what
        // carries the shortfall into the next digit.
        let mut carry: u64 = 0;
        let mut borrow: i64 = 0;
        for at in 0..n {
            let product = guess * u64::from(v[at]) + carry;
            carry = product >> 32;
            let diff = i64::from(u[at + j]) - i64::from(product as u32) + borrow;
            u[at + j] = diff as u32;
            borrow = diff >> 32;
        }
        let diff = i64::from(u[j + n]) - carry as i64 + borrow;
        u[j + n] = diff as u32;

        // The subtraction went below zero, so the estimate was one too big after all. Knuth says
        // this happens about twice in 2^32 digits, which is why the loop above is worth having
        // and why this branch is still needed.
        if diff >> 32 != 0 {
            guess -= 1;
            let mut carry: u64 = 0;
            for at in 0..n {
                let sum = u64::from(u[at + j]) + u64::from(v[at]) + carry;
                u[at + j] = sum as u32;
                carry = sum >> 32;
            }
            u[j + n] = (u64::from(u[j + n]) + carry) as u32;
        }
        quotient[j] = guess as u32;
    }

    (join(quotient), shift_down(&u, by))
}

/// The quotient and the remainder of two signed values.
///
/// C truncates towards zero, so the quotient's sign is the two signs multiplied and the
/// remainder's sign is the dividend's, and neither needs a correction afterwards.
///
/// The magnitudes are taken as unsigned, because the most negative value of the type has no
/// positive of its own and negating it as a signed value is an overflow. As an unsigned value it
/// is the wrap the language defines and the magnitude is right.
pub fn divide_signed(top: i128, bottom: i128) -> (i128, i128) {
    let (top_negative, bottom_negative) = (top < 0, bottom < 0);
    let magnitude = |value: i128| {
        if value < 0 { (value as u128).wrapping_neg() } else { value as u128 }
    };
    let (quotient, rest) = divide(magnitude(top), magnitude(bottom));
    let quotient = if top_negative == bottom_negative {
        quotient as i128
    } else {
        (quotient.wrapping_neg()) as i128
    };
    let rest = if top_negative { (rest.wrapping_neg()) as i128 } else { rest as i128 };
    (quotient, rest)
}

/// The quotient and the remainder at the width libgcc calls `di`, which is the same division one
/// width down and is what a 32-bit target calls for a `long long`.
///
/// The long division above, run over values that happen to fit in 64 bits. That is not laziness
/// about the reference, it is what keeps the two sides different: the C at this width is the bit at
/// a time loop with a 32-bit shortcut, and this is base 2^32 with Knuth's normalization and his add
/// back, which is the same split as at 128 bits. Narrowing is exact, because the quotient and the
/// remainder of two values that fit in 64 bits both fit in 64 bits.
///
/// A divisor below 2^32 takes the single-digit path up there, which is the machine's own 64-bit
/// divide, and that is the best reference this width has.
pub fn divide_long(top: u64, bottom: u64) -> (u64, u64) {
    let (quotient, rest) = divide(u128::from(top), u128::from(bottom));
    (quotient as u64, rest as u64)
}

/// The same for two signed values, with C's truncation towards zero.
///
/// The sign rules and the reason the magnitudes are taken as unsigned are [`divide_signed`]'s, one
/// width down.
pub fn divide_long_signed(top: i64, bottom: i64) -> (i64, i64) {
    let (top_negative, bottom_negative) = (top < 0, bottom < 0);
    let magnitude = |value: i64| {
        if value < 0 { (value as u64).wrapping_neg() } else { value as u64 }
    };
    let (quotient, rest) = divide_long(magnitude(top), magnitude(bottom));
    let quotient = if top_negative == bottom_negative {
        quotient as i64
    } else {
        (quotient.wrapping_neg()) as i64
    };
    let rest = if top_negative { (rest.wrapping_neg()) as i64 } else { rest as i64 };
    (quotient, rest)
}

// The C names. Not compiled under `cargo test`, where a `/` on a 128-bit value in the tests
// themselves is a call to these, and the host's own copies are the ones that should answer it.

/// `unsigned __int128 __udivti3(unsigned __int128, unsigned __int128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __udivti3(top: u128, bottom: u128) -> u128 {
    divide(top, bottom).0
}

/// `unsigned __int128 __umodti3(unsigned __int128, unsigned __int128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __umodti3(top: u128, bottom: u128) -> u128 {
    divide(top, bottom).1
}

/// `unsigned __int128 __udivmodti4(unsigned __int128, unsigned __int128, unsigned __int128 *)`.
///
/// # Safety
///
/// `rest` must be writable, which the C contract says it is. Unlike libgcc's older relatives of
/// this name it is never null.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __udivmodti4(top: u128, bottom: u128, rest: *mut u128) -> u128 {
    let (quotient, remainder) = divide(top, bottom);
    // SAFETY: the caller promises somewhere to write the remainder.
    unsafe { rest.write(remainder) };
    quotient
}

/// `__int128 __divti3(__int128, __int128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __divti3(top: i128, bottom: i128) -> i128 {
    divide_signed(top, bottom).0
}

/// `__int128 __modti3(__int128, __int128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __modti3(top: i128, bottom: i128) -> i128 {
    divide_signed(top, bottom).1
}

/// `__int128 __divmodti4(__int128, __int128, __int128 *)`.
///
/// # Safety
///
/// `rest` must be writable, which the C contract says it is.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __divmodti4(top: i128, bottom: i128, rest: *mut i128) -> i128 {
    let (quotient, remainder) = divide_signed(top, bottom);
    // SAFETY: the caller promises somewhere to write the remainder.
    unsafe { rest.write(remainder) };
    quotient
}

// And the same six at sixty four bits. These are defined here on every host, including the ones
// whose instructions divide at this width, because the harness calls them by name: what is under
// test is the routine a 32-bit target will call, and the only way to ask it anything on a machine
// that has the instruction is to say its name.

/// `unsigned long long __udivdi3(unsigned long long, unsigned long long)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __udivdi3(top: u64, bottom: u64) -> u64 {
    divide_long(top, bottom).0
}

/// `unsigned long long __umoddi3(unsigned long long, unsigned long long)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __umoddi3(top: u64, bottom: u64) -> u64 {
    divide_long(top, bottom).1
}

/// `unsigned long long __udivmoddi4(unsigned long long, unsigned long long, unsigned long long *)`.
///
/// # Safety
///
/// `rest` must be writable, which the C contract says it is. Unlike libgcc's own routine of this
/// name it is never null.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __udivmoddi4(top: u64, bottom: u64, rest: *mut u64) -> u64 {
    let (quotient, remainder) = divide_long(top, bottom);
    // SAFETY: the caller promises somewhere to write the remainder.
    unsafe { rest.write(remainder) };
    quotient
}

/// `long long __divdi3(long long, long long)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __divdi3(top: i64, bottom: i64) -> i64 {
    divide_long_signed(top, bottom).0
}

/// `long long __moddi3(long long, long long)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __moddi3(top: i64, bottom: i64) -> i64 {
    divide_long_signed(top, bottom).1
}

/// `long long __divmoddi4(long long, long long, long long *)`.
///
/// # Safety
///
/// `rest` must be writable, which the C contract says it is.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __divmoddi4(top: i64, bottom: i64, rest: *mut i64) -> i64 {
    let (quotient, remainder) = divide_long_signed(top, bottom);
    // SAFETY: the caller promises somewhere to write the remainder.
    unsafe { rest.write(remainder) };
    quotient
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;

    /// A pseudorandom stream, so the pairs below are the same pairs on every machine and a
    /// failure is a failure anyone can reproduce. This is xorshift64 and nothing rests on its
    /// quality beyond covering the digit lengths.
    struct Stream(u64);

    impl Stream {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// A value whose significant bits are a random width up to 128, which is what makes the
        /// single-digit path, the short-dividend path and the full loop all get used.
        fn value(&mut self) -> u128 {
            let width = (self.next() % 129) as u32;
            if width == 0 {
                return 0;
            }
            let whole = u128::from(self.next()) << 64 | u128::from(self.next());
            whole >> (128 - width)
        }

        /// The same at sixty four bits, which is the width where the shortcut in the C and the
        /// single-digit path in here are each reached by half the pairs.
        fn long(&mut self) -> u64 {
            let width = (self.next() % 65) as u32;
            if width == 0 {
                return 0;
            }
            self.next() >> (64 - width)
        }
    }

    /// The values worth asking about by name rather than by luck: the ends of the type, the digit
    /// boundaries, and the ones either side of them.
    fn corners() -> Vec<u128> {
        let mut out = Vec::new();
        for shift in [0, 1, 31, 32, 33, 63, 64, 65, 95, 96, 97, 126, 127] {
            let at = 1u128 << shift;
            out.push(at);
            out.push(at - 1);
            out.push(at + 1);
        }
        out.push(u128::MAX);
        out.push(u128::MAX - 1);
        out.push(1);
        out
    }

    #[test]
    fn long_division_in_base_four_billion_agrees_with_the_machine() {
        let mut stream = Stream(0x2131_0926_1064_0001);
        for _ in 0..200_000 {
            let (top, bottom) = (stream.value(), stream.value());
            if bottom == 0 {
                continue;
            }
            assert_eq!(divide(top, bottom), (top / bottom, top % bottom), "{top} by {bottom}");
        }
    }

    #[test]
    fn the_corners_of_the_type_divide_the_way_the_machine_says_too() {
        for top in corners() {
            for bottom in corners() {
                // One below the lowest corner is zero, and there is nothing to hold a division by
                // it against: the `/` below is the one that would take the test process down.
                if bottom == 0 {
                    continue;
                }
                assert_eq!(divide(top, bottom), (top / bottom, top % bottom), "{top} by {bottom}");
            }
        }
    }

    #[test]
    fn a_quotient_and_its_remainder_rebuild_the_dividend() {
        let mut stream = Stream(0x5c5c_aa55_0000_1111);
        for _ in 0..50_000 {
            let (top, bottom) = (stream.value(), stream.value());
            if bottom == 0 {
                continue;
            }
            let (quotient, rest) = divide(top, bottom);
            assert!(rest < bottom, "a remainder of {rest} is not below {bottom}");
            assert_eq!(quotient.wrapping_mul(bottom).wrapping_add(rest), top, "{top} by {bottom}");
        }
    }

    #[test]
    fn the_signs_are_the_ones_c_truncating_towards_zero_asks_for() {
        let mut stream = Stream(0x0931_0012_dead_beef);
        for _ in 0..100_000 {
            let (top, bottom) = (stream.value() as i128, stream.value() as i128);
            // Not `MIN / -1`, which overflows and is the one case below rather than here.
            if bottom == 0 || (top == i128::MIN && bottom == -1) {
                continue;
            }
            assert_eq!(divide_signed(top, bottom), (top / bottom, top % bottom), "{top} {bottom}");
        }
    }

    #[test]
    fn the_most_negative_value_over_minus_one_comes_back_as_itself() {
        // The quotient does not exist: its magnitude is one past the type. C calls that undefined
        // and libgcc hands back the wrap, which is what the magnitudes here produce on their own
        // without a case for it. This test is here to pin what comes out, not to bless it.
        assert_eq!(divide_signed(i128::MIN, -1), (i128::MIN, 0));
    }

    /// The branch a quarter of a million random pairs never reaches.
    ///
    /// One less than the divisor, where the divisor has its top bit set and its lowest digit is not
    /// zero. The quotient is zero and the two-digit estimate says one, because the top digits of the
    /// two are equal. Knuth's correction then looks at one more digit, sees a tie rather than an
    /// excess, and leaves the estimate alone, so the only thing that brings it back down is the add
    /// back after the subtraction has already gone below zero. He puts this at about two in 2^32 for
    /// random operands, which is what makes it a case to construct rather than a case to sample, and
    /// that was measured rather than assumed: with the branch taken out by hand, the two hundred
    /// thousand random pairs the tests above draw all still agreed, and what noticed was this and the
    /// corner table, which has one less than a power of two next to it in the same way.
    #[test]
    fn the_estimate_knuths_correction_misses_is_caught_by_adding_the_divisor_back() {
        let mut stream = Stream(0x1064_0002_0000_0003);
        // Three digits of divisor and then four, which are the two lengths this can happen at.
        for top_digit in [95, 127] {
            for _ in 0..2_000 {
                let by = (1u128 << top_digit) | (stream.value() >> (128 - top_digit)) | 1;
                assert_eq!(divide(by - 1, by), (0, by - 1), "one less than {by}");
            }
        }
        // And the same shape without the high bit set, where the normalization shift moves the
        // dividend into a digit of its own on the way in.
        for _ in 0..20_000 {
            let by = stream.value() | 1;
            assert_eq!(divide(by - 1, by), (0, by - 1), "one less than {by}");
        }
    }

    /// Undefined is not the same as allowed to hang. This crate has no unwinder and its panic
    /// handler is a loop, so a zero divisor reaching a `/` in here would be a routine that never
    /// returns, and the early return is what keeps it an answer nobody should ask for instead.
    #[test]
    fn a_zero_divisor_comes_back_rather_than_stopping_the_program() {
        assert_eq!(divide(0, 0), (0, 0));
        assert_eq!(divide(u128::MAX, 0), (0, 0));
        assert_eq!(divide_signed(-5, 0), (0, 0));
    }

    /// The corners of the narrower type, which are its ends and the boundaries of both the 32-bit
    /// half the C's shortcut tests and the digits this file works in.
    fn long_corners() -> Vec<u64> {
        let mut out = Vec::new();
        for shift in [0, 1, 15, 16, 17, 31, 32, 33, 62, 63] {
            let at = 1u64 << shift;
            out.push(at);
            out.push(at - 1);
            out.push(at + 1);
        }
        out.push(u64::MAX);
        out.push(u64::MAX - 1);
        out
    }

    #[test]
    fn the_narrower_division_agrees_with_the_machine_over_random_pairs() {
        let mut stream = Stream(0x1064_0003_0000_0005);
        for _ in 0..200_000 {
            let (top, bottom) = (stream.long(), stream.long());
            if bottom == 0 {
                continue;
            }
            assert_eq!(divide_long(top, bottom), (top / bottom, top % bottom), "{top} by {bottom}");
        }
    }

    #[test]
    fn the_corners_of_the_narrower_type_divide_the_way_the_machine_says_too() {
        for top in long_corners() {
            for bottom in long_corners() {
                if bottom == 0 {
                    continue;
                }
                assert_eq!(
                    divide_long(top, bottom),
                    (top / bottom, top % bottom),
                    "{top} by {bottom}"
                );
            }
        }
    }

    #[test]
    fn the_narrower_signs_are_the_ones_c_asks_for_and_the_ends_behave_the_same_way() {
        let mut stream = Stream(0x1064_0004_ab01_cd02);
        for _ in 0..100_000 {
            let (top, bottom) = (stream.long() as i64, stream.long() as i64);
            if bottom == 0 || (top == i64::MIN && bottom == -1) {
                continue;
            }
            assert_eq!(
                divide_long_signed(top, bottom),
                (top / bottom, top % bottom),
                "{top} {bottom}"
            );
        }
        // The same two cases the wider width is pinned on: the quotient that is one past the type,
        // which C leaves undefined and libgcc hands back as the wrap, and a divisor of zero, which
        // has to come back rather than loop in a crate with no unwinder.
        assert_eq!(divide_long_signed(i64::MIN, -1), (i64::MIN, 0));
        assert_eq!(divide_long(7, 0), (0, 0));
        assert_eq!(divide_long_signed(-5, 0), (0, 0));
        assert_eq!(divide_long_signed(-7, 2), (-3, -1));
        assert_eq!(divide_long_signed(7, -2), (-3, 1));
        assert_eq!(divide_long_signed(-7, -2), (3, -1));
    }

    #[test]
    fn dividing_something_by_something_bigger_keeps_all_of_it() {
        assert_eq!(divide(5, u128::MAX), (0, 5));
        assert_eq!(divide(0, 7), (0, 0));
        assert_eq!(divide(u128::MAX, u128::MAX), (1, 0));
        assert_eq!(divide_signed(-7, 2), (-3, -1));
        assert_eq!(divide_signed(7, -2), (-3, 1));
        assert_eq!(divide_signed(-7, -2), (3, -1));
    }
}
