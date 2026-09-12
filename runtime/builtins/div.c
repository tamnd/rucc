/* Dividing an integer wider than a register, which is the one arithmetic a target cannot do in
 * instructions at all.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8 and spec/cross-compile/10-runtime.md section
 * 10.2. The backend splits a 128-bit add or shift into two instructions over the halves, which
 * crates/rucc-codegen/src/wide.rs does, and there is no such rewrite for a divide: a quotient is
 * not a function of the halves of its operands taken apart. So the divide becomes a call, and the
 * names below are what it calls. They are libgcc's names because an object we produced gets
 * linked against objects GCC produced, which is the same reason the block routines next door are
 * called memcpy and memmove.
 *
 * Dividing by zero is not handled and cannot be. It is undefined in C, the machine traps on it at
 * sixty four bits, and a value returned here instead of a trap would be this library inventing an
 * answer the language says does not exist. So the loop below treats a zero divisor as nothing
 * special and what comes out of it is whatever the arithmetic produces, the same as libgcc.
 *
 * One shortcut and then the long way. Two values that both fit in sixty four bits are divided by
 * the instruction the machine has, which is the case almost every program is in: an __int128 in a
 * real program is usually a small number that needed the range rather than a number that used it.
 * Everything else goes a bit at a time, 128 iterations of shift, compare and subtract. That is the
 * slow version and it is the correct one, and a faster one is a measurement rather than an
 * opinion: the textbook answer is division in base 2^32 with a normalization step, which is four
 * times the code, and what it wants is the benchmark runtime/builtins/mem.c is also waiting for.
 * The reference implementation in runtime/rucc-builtins is the base 2^32 version, which is on
 * purpose: two algorithms that agree on millions of randomized pairs is the point of holding one
 * against the other at all.
 *
 * Two widths, and the second one is the same work one width down. A 32-bit target divides two
 * sixty four bit values with a call for the same reason a 64-bit target divides two 128-bit ones
 * with a call, so i686, armv7 and wasm32 need the six names below with di in them the way every
 * 64-bit row needs the six with ti. They are a second copy of the loop rather than one loop behind
 * a macro, because what this file is for is being read against the routine it stands in for, and a
 * macro that expands into two divisions is read twice and checked neither time.
 */

typedef unsigned __int128 uwide;
typedef __int128 wide;

/* The narrower pair. libgcc spells this width di, for double integer, because it is two of the
 * sixteen bit ints the naming scheme was written around.
 */
typedef unsigned long long ulong;
typedef long long slong;

/* How many bits a half is, which is also the width the machine divides in. */
#define HALF 64

/* And the same for the narrower pair, where a half is a register on the targets that need it. */
#define HALF_LONG 32

/* The quotient, with the remainder written through `rest` when it is asked for.
 *
 * Everything else in the file is this function and a sign.
 */
static uwide divide(uwide top, uwide bottom, uwide *rest) {
    uwide quotient = 0;
    uwide remainder = 0;
    int at;

    if ((top >> HALF) == 0 && (bottom >> HALF) == 0) {
        unsigned long long small = (unsigned long long)top;
        unsigned long long by = (unsigned long long)bottom;
        if (rest != 0) {
            *rest = small % by;
        }
        return small / by;
    }

    /* The top bit first, because the remainder carries downward: each step brings one more bit of
     * the dividend into a remainder that is always less than the divisor, and the quotient bit is
     * whether the divisor fits once it has.
     */
    for (at = 2 * HALF - 1; at >= 0; at -= 1) {
        remainder = (remainder << 1) | ((top >> at) & 1);
        quotient = quotient << 1;
        if (remainder >= bottom) {
            remainder = remainder - bottom;
            quotient = quotient | 1;
        }
    }
    if (rest != 0) {
        *rest = remainder;
    }
    return quotient;
}

/* A signed value as its magnitude, with the sign written through `negative`.
 *
 * Negating the unsigned copy rather than the signed one, because the most negative value of the
 * type has no positive of its own and negating it as a signed value is undefined. As an unsigned
 * value it is the wrap the language defines, and the magnitude that comes out is right.
 */
static uwide magnitude(wide value, int *negative) {
    *negative = value < 0;
    if (value < 0) {
        return -(uwide)value;
    }
    return (uwide)value;
}

unsigned __int128 __udivti3(unsigned __int128 top, unsigned __int128 bottom) {
    return divide(top, bottom, 0);
}

unsigned __int128 __umodti3(unsigned __int128 top, unsigned __int128 bottom) {
    uwide rest;
    divide(top, bottom, &rest);
    return rest;
}

unsigned __int128 __udivmodti4(unsigned __int128 top, unsigned __int128 bottom,
                               unsigned __int128 *rest) {
    return divide(top, bottom, rest);
}

/* The quotient of two signed values, which is the quotient of their magnitudes and a sign. C says
 * the division truncates towards zero, so the sign is the two signs multiplied and there is
 * nothing to correct afterwards.
 */
__int128 __divti3(__int128 top, __int128 bottom) {
    int top_negative;
    int bottom_negative;
    uwide quotient = divide(magnitude(top, &top_negative), magnitude(bottom, &bottom_negative), 0);
    if (top_negative != bottom_negative) {
        return (wide)(-quotient);
    }
    return (wide)quotient;
}

/* The remainder takes the sign of the dividend, which is what follows from truncation towards
 * zero: the quotient lost the fraction, so what is left over is on the dividend's side of zero.
 */
__int128 __modti3(__int128 top, __int128 bottom) {
    int top_negative;
    int bottom_negative;
    uwide rest;
    divide(magnitude(top, &top_negative), magnitude(bottom, &bottom_negative), &rest);
    if (top_negative) {
        return (wide)(-rest);
    }
    return (wide)rest;
}

__int128 __divmodti4(__int128 top, __int128 bottom, __int128 *rest) {
    *rest = __modti3(top, bottom);
    return __divti3(top, bottom);
}

/* The same division at sixty four bits, for a target whose registers are thirty two.
 *
 * Nothing here is allowed to write a / or a % on a value of this width, because that is a call to
 * __udivdi3 on the target this code is for and __udivdi3 is the function below. The shortcut is on
 * unsigned int instead, which is a register on such a target and is either one instruction or a
 * call to a narrower routine, and either way it is not this one.
 */
static ulong divide_long(ulong top, ulong bottom, ulong *rest) {
    ulong quotient = 0;
    ulong remainder = 0;
    int at;

    if ((top >> HALF_LONG) == 0 && (bottom >> HALF_LONG) == 0) {
        unsigned int small = (unsigned int)top;
        unsigned int by = (unsigned int)bottom;
        if (rest != 0) {
            *rest = small % by;
        }
        return small / by;
    }

    for (at = 2 * HALF_LONG - 1; at >= 0; at -= 1) {
        remainder = (remainder << 1) | ((top >> at) & 1);
        quotient = quotient << 1;
        if (remainder >= bottom) {
            remainder = remainder - bottom;
            quotient = quotient | 1;
        }
    }
    if (rest != 0) {
        *rest = remainder;
    }
    return quotient;
}

static ulong magnitude_long(slong value, int *negative) {
    *negative = value < 0;
    if (value < 0) {
        return -(ulong)value;
    }
    return (ulong)value;
}

unsigned long long __udivdi3(unsigned long long top, unsigned long long bottom) {
    return divide_long(top, bottom, 0);
}

unsigned long long __umoddi3(unsigned long long top, unsigned long long bottom) {
    ulong rest;
    divide_long(top, bottom, &rest);
    return rest;
}

unsigned long long __udivmoddi4(unsigned long long top, unsigned long long bottom,
                                unsigned long long *rest) {
    return divide_long(top, bottom, rest);
}

long long __divdi3(long long top, long long bottom) {
    int top_negative;
    int bottom_negative;
    ulong top_magnitude = magnitude_long(top, &top_negative);
    ulong bottom_magnitude = magnitude_long(bottom, &bottom_negative);
    ulong quotient = divide_long(top_magnitude, bottom_magnitude, 0);
    if (top_negative != bottom_negative) {
        return (slong)(-quotient);
    }
    return (slong)quotient;
}

long long __moddi3(long long top, long long bottom) {
    int top_negative;
    int bottom_negative;
    ulong rest;
    divide_long(magnitude_long(top, &top_negative), magnitude_long(bottom, &bottom_negative),
                &rest);
    if (top_negative) {
        return (slong)(-rest);
    }
    return (slong)rest;
}

long long __divmoddi4(long long top, long long bottom, long long *rest) {
    *rest = __moddi3(top, bottom);
    return __divdi3(top, bottom);
}
