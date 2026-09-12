/* Converting between a 128-bit integer and a float, which is the other pair of operations a
 * 64-bit machine has no instruction for.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8 and spec/cross-compile/10-runtime.md section
 * 10.2, which lists the conversions with unusual widths as wanted on most targets. The machine
 * converts between a float and an integer up to sixty four bits wide and no further, so a cast
 * between a float and an __int128 is a call, and these eight names are what it calls. They are
 * libgcc's names, for the reason every other name in this directory is.
 *
 * The widths here are float and double. The eighty bit float is left out because this machine has
 * no register that holds one and the backend says so, which is tamnd/rucc#326, and binary128 is
 * left out because the arithmetic underneath it is not written yet. __floattixf, __fixxfti and the
 * quad spellings arrive with those.
 *
 * # What is not handled
 *
 * A value the integer cannot hold, an infinity and a not a number, none of which have an answer.
 * C leaves all three undefined and libgcc returns whatever its arithmetic happened to produce, so
 * a check here would be this library inventing an answer the language says does not exist. That is
 * the same position div.c takes on a zero divisor and for the same reason.
 *
 * A negative value handed to one of the unsigned conversions is the same kind of nothing, and it
 * comes out as zero here rather than as whatever the arithmetic left behind, because the test that
 * catches a not a number catches it on the way past. That is an accident of the shape rather than a
 * promise, and the reference next door makes the same one so that the two can be compared at all.
 *
 * # Going up: round to odd, then let the machine round
 *
 * A value of more than sixty four significant bits cannot be handed to the machine's conversion,
 * so the top sixty four bits are taken and the bits below them are remembered in the lowest bit of
 * those sixty four: a value that lost nothing keeps whatever bit it had, and a value that lost
 * something comes out odd. An odd value is never halfway between two floats, so the machine's
 * rounding cannot go the wrong way on it, and rounding to odd and then to nearest gives the same
 * float as rounding to nearest once. That is what makes two roundings safe here when two
 * roundings are usually the bug. Putting the discarded bits back afterwards is multiplying by a
 * power of two, which is exact until the result stops fitting, and where it stops fitting the
 * answer is an infinity, which is what the multiplication produces.
 *
 * # Coming down: halve, truncate, subtract, truncate
 *
 * A float at least 2^64 is divided by 2^64, which is exact, and the integer part of that is the
 * top half of the answer. Multiplying it back and subtracting leaves the bottom half, and that
 * subtraction is exact too: the remainder is below 2^64 and is a multiple of the original value's
 * own spacing, so it needs no more mantissa than the value it came from. So the whole of this is
 * exact arithmetic and the truncation C asks for happens where C asks for it.
 */

typedef unsigned __int128 uwide;
typedef __int128 wide;

/* 2^64, which every format here holds exactly. */
#define TWO_TO_64 18446744073709551616.0

/* How many bits of `value` are significant, and zero for a value of zero.
 *
 * Halving the step rather than shifting one bit at a time, which is seven rounds instead of a
 * hundred and twenty eight for the same answer.
 */
static int significant(uwide value) {
    int bits = 0;
    int step;
    for (step = 64; step > 0; step >>= 1) {
        if ((value >> step) != 0) {
            value >>= step;
            bits += step;
        }
    }
    if (value == 0) {
        return 0;
    }
    return bits + 1;
}

/* The top sixty four bits of a value wider than that, with whatever fell off the bottom recorded
 * as the lowest bit of what comes back.
 *
 * `extra` is how many bits fell off and is between one and sixty four, so the shift that asks
 * whether any of them was set is between sixty four and a hundred and twenty seven and is a shift
 * this type has.
 */
static unsigned long long leading(uwide value, int extra) {
    unsigned long long top = (unsigned long long)(value >> extra);
    if ((value << (128 - extra)) != 0) {
        top |= 1;
    }
    return top;
}

/* `value` multiplied by 2^by, in steps, which is exact at every step until it overflows. */
static double scale_double(double value, int by) {
    while (by >= 16) {
        value *= 65536.0;
        by -= 16;
    }
    while (by > 0) {
        value *= 2.0;
        by -= 1;
    }
    return value;
}

static float scale_float(float value, int by) {
    while (by >= 16) {
        value *= 65536.0f;
        by -= 16;
    }
    while (by > 0) {
        value *= 2.0f;
        by -= 1;
    }
    return value;
}

double __floatuntidf(unsigned __int128 value) {
    int bits = significant(value);
    if (bits <= 64) {
        return (double)(unsigned long long)value;
    }
    return scale_double((double)leading(value, bits - 64), bits - 64);
}

float __floatuntisf(unsigned __int128 value) {
    int bits = significant(value);
    if (bits <= 64) {
        return (float)(unsigned long long)value;
    }
    return scale_float((float)leading(value, bits - 64), bits - 64);
}

/* A signed value is its magnitude and a sign, and the float carries the sign exactly.
 *
 * The magnitude is taken as an unsigned value, because the most negative value of the type has no
 * positive of its own and negating it as a signed value is undefined.
 */
double __floattidf(__int128 value) {
    if (value < 0) {
        return -__floatuntidf(-(uwide)value);
    }
    return __floatuntidf((uwide)value);
}

float __floattisf(__int128 value) {
    if (value < 0) {
        return -__floatuntisf(-(uwide)value);
    }
    return __floatuntisf((uwide)value);
}

unsigned __int128 __fixunsdfti(double value) {
    double high;
    unsigned long long top;
    /* Written as a negated comparison so that a not a number, which compares false with
     * everything, lands here rather than going on to be truncated.
     */
    if (!(value >= 1.0)) {
        return 0;
    }
    if (value < TWO_TO_64) {
        return (uwide)(unsigned long long)value;
    }
    high = value / TWO_TO_64;
    top = (unsigned long long)high;
    value = value - (double)top * TWO_TO_64;
    return ((uwide)top << 64) | (unsigned long long)value;
}

/* A float widens to a double exactly, so the two conversions down have one implementation and
 * there is nothing to get wrong twice.
 */
unsigned __int128 __fixunssfti(float value) {
    return __fixunsdfti((double)value);
}

/* The signed conversions truncate towards zero, which is what C says and what makes the magnitude
 * of the answer the magnitude of the value. Negating the unsigned result rather than the signed
 * one, so that the most negative value of the type comes out right instead of overflowing on the
 * way.
 */
__int128 __fixdfti(double value) {
    if (value < 0) {
        return (wide)(-__fixunsdfti(-value));
    }
    return (wide)__fixunsdfti(value);
}

__int128 __fixsfti(float value) {
    return __fixdfti((double)value);
}
