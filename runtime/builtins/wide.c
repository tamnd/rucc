/* Multiplying and shifting a 128-bit integer, for a target whose back end calls for them.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8 and the WebAssembly notes, section 8.6. On a
 * 64-bit row the back end does a 128-bit multiply or shift in instructions over the two halves, so
 * nothing there calls these names. wasm32 is different. clang holds an `__int128` there in two
 * `i64` values and calls compiler-rt for a multiply and for a shift by a count that is not a
 * constant, and the wasm back end of this compiler calls the same names so that its objects link
 * with clang's. These are the five names, with compiler-rt's signatures.
 *
 * Each routine works on the two 64-bit halves and on nothing wider. The obvious C, `a * b` or
 * `a << count` on an `__int128`, is on wasm32 a call to the routine that is being written, so the
 * routine would call itself until the stack ran out. A shift by a constant and a conversion between
 * a half and the whole are done in instructions on every target, which is why the halves are taken
 * apart and put together with `>> 64` and `<< 64` and with nothing else.
 *
 * The product of two halves is four products of 32-bit quarters, the way double.c builds its
 * product, because a 64-bit by 64-bit product with all 128 bits of it is the multiply that is being
 * written here.
 */

#ifdef __SIZEOF_INT128__

typedef unsigned long long u64;
typedef unsigned __int128 uwide;
typedef __int128 wide;

static u64 high_of(uwide value) {
    return (u64)(value >> 64);
}

static u64 low_of(uwide value) {
    return (u64)value;
}

static uwide wide_of(u64 high, u64 low) {
    return ((uwide)high << 64) | (uwide)low;
}

/* All 128 bits of the product of two halves, from the four products of their 32-bit quarters. The
 * middle two can each carry into the top, and the sum of the three pieces in the middle column fits
 * in 64 bits because each of them is below 2^32.
 */
static uwide product_of_halves(u64 left, u64 right) {
    u64 left_low = left & 0xffffffffu;
    u64 left_high = left >> 32;
    u64 right_low = right & 0xffffffffu;
    u64 right_high = right >> 32;
    u64 low = left_low * right_low;
    u64 cross_one = left_high * right_low;
    u64 cross_two = left_low * right_high;
    u64 high = left_high * right_high;
    u64 middle = (low >> 32) + (cross_one & 0xffffffffu) + (cross_two & 0xffffffffu);
    high += (cross_one >> 32) + (cross_two >> 32) + (middle >> 32);
    low = (low & 0xffffffffu) | (middle << 32);
    return wide_of(high, low);
}

/* The low 128 bits of the product, which is the same for signed and unsigned operands. The two
 * cross products only reach the high half, and only their low 64 bits get there.
 */
wide __multi3(wide left, wide right) {
    uwide low = product_of_halves(low_of((uwide)left), low_of((uwide)right));
    u64 high = high_of(low) + low_of((uwide)left) * high_of((uwide)right)
               + high_of((uwide)left) * low_of((uwide)right);
    return (wide)wide_of(high, low_of(low));
}

/* The magnitude of a signed value, as an unsigned one, which is right for the most negative value
 * too, whose magnitude is one past the largest signed value.
 */
static uwide magnitude(wide value) {
    uwide bits = (uwide)value;
    return value < 0 ? wide_of(~high_of(bits), ~low_of(bits)) + 1 : bits;
}

/* The product with the overflow written through `overflow`, as compiler-rt has it: one when the
 * product does not fit in the type and zero when it does. The product of the magnitudes is taken
 * in full, in four halves, and it fits when its top two halves are zero and its bottom two are no
 * more than the largest magnitude that the sign of the answer allows.
 */
wide __muloti4(wide left, wide right, int *overflow) {
    uwide a = magnitude(left);
    uwide b = magnitude(right);
    uwide low = product_of_halves(low_of(a), low_of(b));
    uwide cross_one = product_of_halves(high_of(a), low_of(b));
    uwide cross_two = product_of_halves(low_of(a), high_of(b));
    uwide top = product_of_halves(high_of(a), high_of(b));
    /* The second half up from the bottom, with what it carries into the top two. */
    uwide second = (uwide)high_of(low) + low_of(cross_one) + low_of(cross_two);
    int too_wide = top != 0 || high_of(cross_one) != 0 || high_of(cross_two) != 0
                   || high_of(second) != 0;
    uwide whole = wide_of(low_of(second), low_of(low));
    int negative = (left < 0) != (right < 0);
    uwide limit = (uwide)1 << 127;
    *overflow = too_wide || (negative ? whole > limit : whole >= limit);
    return __multi3(left, right);
}

/* The three shifts. A count is from zero to 127, as compiler-rt asks, and a count of 64 or more
 * moves one half into the other with nothing left of it in its own place. A count of zero is kept
 * apart because the low half would be shifted right by 64, which C does not define.
 */
wide __ashlti3(wide value, int count) {
    u64 high = high_of((uwide)value);
    u64 low = low_of((uwide)value);
    if (count == 0) {
        return value;
    }
    if (count >= 64) {
        return (wide)wide_of(low << (count - 64), 0);
    }
    return (wide)wide_of((high << count) | (low >> (64 - count)), low << count);
}

wide __lshrti3(wide value, int count) {
    u64 high = high_of((uwide)value);
    u64 low = low_of((uwide)value);
    if (count == 0) {
        return value;
    }
    if (count >= 64) {
        return (wide)wide_of(0, high >> (count - 64));
    }
    return (wide)wide_of(high >> count, (low >> count) | (high << (64 - count)));
}

/* The sign is copied down from the top, which is what `>>` does on a signed 64-bit value with every
 * compiler this archive is built by, and what gcc documents.
 */
wide __ashrti3(wide value, int count) {
    u64 high = high_of((uwide)value);
    u64 low = low_of((uwide)value);
    u64 fill = (u64)((long long)high >> 63);
    if (count == 0) {
        return value;
    }
    if (count >= 64) {
        return (wide)wide_of(fill, (u64)((long long)high >> (count - 64)));
    }
    return (wide)wide_of((u64)((long long)high >> count), (low >> count) | (high << (64 - count)));
}

#endif
