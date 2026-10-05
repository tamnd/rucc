//! The 128-bit multiply and shifts, which a target that holds an `__int128` in two halves calls.
//!
//! `runtime/builtins/wide.c` is what ships, and wasm32 is the row that calls it: clang and rucc
//! both call compiler-rt's names there for a 128-bit multiply and for a shift by a count that is
//! not a constant. This is the reference for it, and it uses a different method. The C builds the
//! product out of 32-bit quarters and finds the overflow in the top half of the full product. Here
//! the product is the host's own 128-bit multiply, and the overflow is found the way compiler-rt
//! finds it, by dividing the product by one operand and comparing the answer with the other. The
//! division is this crate's `__divti3`, which is a third algorithm.
//!
//! `checked_mul` would say the overflow in one call and is not used. On a host whose word is 64
//! bits, LLVM lowers a checked 128-bit multiply to a call to `__muloti4`, which is a function in
//! this file, so it would call itself.

/// The low 128 bits of the product.
fn multiply(left: i128, right: i128) -> i128 {
    left.wrapping_mul(right)
}

/// The product and whether it overflowed. A product that did not overflow divides back to the
/// other operand. The one pair that a division cannot check is minus one times the most negative
/// value, whose product wraps to the most negative value, which divides back to itself.
fn multiply_checked(left: i128, right: i128) -> (i128, bool) {
    let product = left.wrapping_mul(right);
    let overflow =
        left != 0 && (product.wrapping_div(left) != right || (left == -1 && right == i128::MIN));
    (product, overflow)
}

/// The three shifts, for a count from zero to 127.
fn shift_left(value: i128, count: u32) -> i128 {
    value << count
}

fn shift_right_logical(value: i128, count: u32) -> i128 {
    ((value as u128) >> count) as i128
}

fn shift_right_arithmetic(value: i128, count: u32) -> i128 {
    value >> count
}

/// The count as the shift takes it. The C contract says it is from zero to 127, and the mask
/// keeps a count that broke the contract from being a panic in a debug build.
fn count_of(count: i32) -> u32 {
    count as u32 & 127
}

// The C names. Not compiled under `cargo test`, where a multiply on a 128-bit value in the tests
// is code that may call these names, and the host's own copies are the ones that should answer.

/// `__int128 __multi3(__int128, __int128)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __multi3(left: i128, right: i128) -> i128 {
    multiply(left, right)
}

/// `__int128 __muloti4(__int128, __int128, int *)`.
///
/// # Safety
///
/// `overflow` must be writable, which the C contract says it is.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __muloti4(left: i128, right: i128, overflow: *mut i32) -> i128 {
    let (product, overflowed) = multiply_checked(left, right);
    // SAFETY: the caller gives somewhere to write the flag.
    unsafe { overflow.write(i32::from(overflowed)) };
    product
}

/// `__int128 __ashlti3(__int128, int)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __ashlti3(value: i128, count: i32) -> i128 {
    shift_left(value, count_of(count))
}

/// `__int128 __lshrti3(__int128, int)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __lshrti3(value: i128, count: i32) -> i128 {
    shift_right_logical(value, count_of(count))
}

/// `__int128 __ashrti3(__int128, int)`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn __ashrti3(value: i128, count: i32) -> i128 {
    shift_right_arithmetic(value, count_of(count))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overflow flag agrees with the host's own checked multiply, on the corners where it is
    /// decided. The host's checked multiply is fine here, because under `cargo test` the C names
    /// above are not compiled and the call goes to the host's runtime.
    #[test]
    fn the_overflow_is_the_one_that_the_host_finds() {
        let mut corners = std::vec::Vec::new();
        for shift in [0, 1, 31, 32, 33, 63, 64, 65, 95, 96, 97, 126, 127] {
            let one = 1i128.wrapping_shl(shift);
            for value in [one.wrapping_sub(1), one, one.wrapping_add(1)] {
                corners.extend([value, value.wrapping_neg()]);
            }
        }
        corners.extend([i128::MAX, i128::MIN, -1, 0]);
        for &left in &corners {
            for &right in &corners {
                let (product, overflow) = multiply_checked(left, right);
                assert_eq!(product, multiply(left, right));
                assert_eq!(overflow, left.checked_mul(right).is_none(), "{left} * {right}");
            }
        }
    }

    #[test]
    fn a_shift_moves_bits_across_the_halves_and_keeps_the_sign_where_it_should() {
        let value = i128::MIN | 0x8000_0000_0000_0001;
        assert_eq!(shift_left(value, 64), 0x8000_0000_0000_0001 << 64);
        assert_eq!(shift_right_logical(value, 127), 1);
        assert_eq!(shift_right_arithmetic(value, 127), -1);
        assert_eq!(shift_right_arithmetic(value, 0), value);
        assert_eq!(count_of(-1), 127);
    }
}
