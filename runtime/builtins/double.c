/* Double precision arithmetic done in integers, for a target with no floating point unit.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8 and spec/cross-compile/10-runtime.md section 10.2.
 * This is the same work as runtime/builtins/float.c one format up: the four operations and the
 * negation, under libgcc's names, for a target where `a + b` on two doubles is a call rather than an
 * instruction. The comparisons and the conversions for this format are not written yet.
 *
 * # What is the same and what is not
 *
 * Everything about the shape is the same, and that is the reason the formats went in one at a time:
 * the mistakes in a routine this size are in its shape, and a shape that has been held against a
 * reference at one width is worth copying at the next. The field widths change, the three rounding
 * bits do not, the tie goes to the even significand here as well, and no routine here raises an
 * exception or reads a rounding mode for the reasons float.c gives.
 *
 * Two things genuinely change, and both are about a significand that no longer leaves room in a word.
 *
 * The multiplication cannot hold its own product. Two fifty three bit significands multiply into a
 * hundred and six bits, so the product is built out of the four products of their thirty two bit
 * halves and carried in two words. A `__int128` here would be this file calling `__multi3`, which is
 * the one thing it is not allowed to need, and a 32-bit target would not have the type at all.
 *
 * The division is the same bit at a time loop as one format down, which needs no more room because a
 * remainder stays below the divisor: what grows is the number of times round it, fifty seven rather
 * than twenty eight.
 *
 * # Reading the bits
 *
 * Through a union, which is what C says to write for this.
 */

typedef unsigned int u32;
typedef unsigned long long u64;

/* How many bits of the significand the format writes down, the other one being implied. */
#define FRACTION 52

/* What is added to an exponent before it is stored, so that the field holds no negative number. */
#define BIAS 1023

/* The exponent field of an infinity and of a not a number, which is every bit of it set. */
#define TOP 2047

#define SIGN 0x8000000000000000ull
#define IMPLICIT 0x0010000000000000ull
#define FRACTION_MASK 0x000FFFFFFFFFFFFFull

/* The top bit of the fraction, which is what tells a quiet not a number from a signalling one. */
#define QUIET 0x0008000000000000ull

/* The not a number an operation with no answer produces: quiet, positive, and nothing else. */
#define EMPTY_NAN 0x7FF8000000000000ull

/* Guard, round and sticky. The significands below are shifted up by this much. */
#define EXTRA 3

/* Where the leading one of a normalized significand sits once it has been shifted up. */
#define LEADING (FRACTION + EXTRA)

/* The low half of a word, for the multiplication that works in halves. */
#define HALF 0xFFFFFFFFull

union bits {
    double number;
    u64 pattern;
};

static u64 pattern_of(double value) {
    union bits at;
    at.number = value;
    return at.pattern;
}

static double double_of(u64 pattern) {
    union bits at;
    at.pattern = pattern;
    return at.number;
}

static int exponent_of(u64 pattern) {
    return (int)((pattern >> FRACTION) & TOP);
}

static u64 fraction_of(u64 pattern) {
    return pattern & FRACTION_MASK;
}

static int is_nan(u64 pattern) {
    return exponent_of(pattern) == TOP && fraction_of(pattern) != 0;
}

static int is_infinite(u64 pattern) {
    return exponent_of(pattern) == TOP && fraction_of(pattern) == 0;
}

/* The significand with the leading one written out where the format only implies it, and the
 * exponent the arithmetic wants.
 *
 * A subnormal gets the exponent of the smallest normal, which is the one it shares, and no leading
 * one, which is the only difference between the two cases.
 */
static void unpack(u64 pattern, u64 *significand, int *exponent) {
    int stored = exponent_of(pattern);
    u64 fraction = fraction_of(pattern);
    if (stored == 0) {
        *significand = fraction;
        *exponent = 1;
    } else {
        *significand = fraction | IMPLICIT;
        *exponent = stored;
    }
}

/* Shifts a significand down by `down` bits, keeping whatever falls off in the lowest bit, which is
 * the sticky bit and means "and there was more below this".
 */
static u64 shift_down(u64 significand, int down) {
    if (down >= 64) {
        return significand != 0;
    }
    u64 lost = significand & (((u64)1 << down) - 1);
    return (significand >> down) | (lost != 0);
}

/* The double nearest the value this sign, exponent and significand describe.
 *
 * What it is handed is a significand whose leading one is at `LEADING` with the three rounding bits
 * below it, and the exponent that significand goes with. The two departures the callers produce are
 * allowed: an exponent at or below zero, which is a result too small to hold normally, and an
 * exponent of one with the leading one missing, which is what a cancellation leaves at the bottom of
 * the range. Both are a subnormal and both are handled by getting the stored exponent to zero.
 *
 * The rounding is done on the packed result rather than on the significand, so that an increment
 * which carries out of the fraction carries into the exponent on its own and one that carries out of
 * the largest finite number lands on the infinity that is the right answer.
 */
static double round_and_pack(u64 sign, int exponent, u64 significand) {
    if (exponent <= 0) {
        significand = shift_down(significand, 1 - exponent);
        exponent = 0;
    } else if (significand < ((u64)1 << LEADING)) {
        exponent = 0;
    }
    if (exponent >= TOP) {
        return double_of(sign | ((u64)TOP << FRACTION));
    }
    u64 below = significand & (((u64)1 << EXTRA) - 1);
    u64 result = sign | ((u64)exponent << FRACTION);
    result |= (significand >> EXTRA) & FRACTION_MASK;
    if (below > ((u64)1 << (EXTRA - 1))) {
        result += 1;
    } else if (below == ((u64)1 << (EXTRA - 1))) {
        /* Exactly halfway, so the even one of the two neighbours wins, which is the one whose lowest
         * bit is already zero.
         */
        result += result & 1;
    }
    return double_of(result);
}

/* A significand with its leading one brought up to where the multiplication and the division want
 * it, with the exponent moved down to match. This is only ever a subnormal: anything else arrives
 * from `unpack` already there.
 */
static void normalize(u64 *significand, int *exponent) {
    while ((*significand >> FRACTION) == 0) {
        *significand <<= 1;
        *exponent -= 1;
    }
}

static double quiet(u64 pattern) {
    return double_of(pattern | QUIET);
}

static double add(u64 left, u64 right) {
    if (is_nan(left)) {
        return quiet(left);
    }
    if (is_nan(right)) {
        return quiet(right);
    }
    if (is_infinite(left)) {
        /* An infinity less an infinity has no answer. Anything else with an infinity in it is that
         * infinity, because no finite number moves one.
         */
        if (is_infinite(right) && ((left ^ right) & SIGN) != 0) {
            return double_of(EMPTY_NAN);
        }
        return double_of(left);
    }
    if (is_infinite(right)) {
        return double_of(right);
    }

    u64 left_significand;
    u64 right_significand;
    int left_exponent;
    int right_exponent;
    unpack(left, &left_significand, &left_exponent);
    unpack(right, &right_significand, &right_exponent);

    if (left_significand == 0 && right_significand == 0) {
        /* Two zeros, and the answer is negative only when both of them are, which is what rounding
         * to nearest asks for.
         */
        return double_of(left & right & SIGN);
    }
    if (left_significand == 0) {
        return double_of(right);
    }
    if (right_significand == 0) {
        return double_of(left);
    }

    /* The larger magnitude first, because the sign of the answer is its sign and because the smaller
     * one is the one that gets lined up underneath.
     */
    u64 larger = left_significand << EXTRA;
    u64 smaller = right_significand << EXTRA;
    int high = left_exponent;
    int low = right_exponent;
    u64 sign = left & SIGN;
    int opposite = ((left ^ right) & SIGN) != 0;
    if (right_exponent > left_exponent
        || (right_exponent == left_exponent && smaller > larger)) {
        u64 swap = larger;
        larger = smaller;
        smaller = swap;
        high = right_exponent;
        low = left_exponent;
        sign = right & SIGN;
    }

    /* Line the smaller one up under the larger, keeping what falls off in the sticky bit. */
    smaller = shift_down(smaller, high - low);
    int exponent = high;

    u64 significand;
    if (opposite) {
        significand = larger - smaller;
        if (significand == 0) {
            /* A number less itself is a positive zero in every rounding mode but one this does not
             * have.
             */
            return double_of(0);
        }
        while (significand < ((u64)1 << LEADING) && exponent > 1) {
            significand <<= 1;
            exponent -= 1;
        }
    } else {
        significand = larger + smaller;
        if (significand >= ((u64)1 << (LEADING + 1))) {
            significand = shift_down(significand, 1);
            exponent += 1;
        }
    }
    return round_and_pack(sign, exponent, significand);
}

double __adddf3(double left, double right) {
    return add(pattern_of(left), pattern_of(right));
}

/* Subtraction is addition with the sign of the right operand flipped, which is exact: a sign is a
 * bit and flipping it is the negation of the value it belongs to, at every magnitude.
 */
double __subdf3(double left, double right) {
    return add(pattern_of(left), pattern_of(right) ^ SIGN);
}

/* The product of two significands, which does not fit in a word.
 *
 * Four products of thirty two bit halves, added up with the carries written out. This is the one
 * routine in the two soft float files that has no counterpart one format down, because there the
 * product of two twenty four bit significands fits in a word and a single multiplication does it.
 */
static void multiply_wide(u64 left, u64 right, u64 *high, u64 *low) {
    u64 left_low = left & HALF;
    u64 left_high = left >> 32;
    u64 right_low = right & HALF;
    u64 right_high = right >> 32;
    u64 low_low = left_low * right_low;
    u64 cross_one = left_high * right_low;
    u64 cross_two = left_low * right_high;
    u64 high_high = left_high * right_high;
    /* The middle column: what the low product carried out of its own half, and the low halves of the
     * two cross products. Its own carry goes on to the high word.
     */
    u64 middle = (low_low >> 32) + (cross_one & HALF) + (cross_two & HALF);
    *low = (low_low & HALF) | (middle << 32);
    *high = high_high + (cross_one >> 32) + (cross_two >> 32) + (middle >> 32);
}

/* The top bits of a two word value, shifted down by `down` with everything below them in the sticky
 * bit. `down` is a constant in the one caller and is between one and sixty three, so neither shift
 * here is the undefined one.
 */
static u64 shift_down_wide(u64 high, u64 low, int down) {
    u64 kept = (high << (64 - down)) | (low >> down);
    u64 lost = low & (((u64)1 << down) - 1);
    return kept | (lost != 0);
}

double __muldf3(double left, double right) {
    u64 left_pattern = pattern_of(left);
    u64 right_pattern = pattern_of(right);
    if (is_nan(left_pattern)) {
        return quiet(left_pattern);
    }
    if (is_nan(right_pattern)) {
        return quiet(right_pattern);
    }
    u64 sign = (left_pattern ^ right_pattern) & SIGN;
    int left_zero = (left_pattern & ~SIGN) == 0;
    int right_zero = (right_pattern & ~SIGN) == 0;
    if (is_infinite(left_pattern) || is_infinite(right_pattern)) {
        /* A zero times an infinity has no answer. An infinity times anything else is an infinity,
         * and the sign is the two signs multiplied whatever the magnitudes were.
         */
        if (left_zero || right_zero) {
            return double_of(EMPTY_NAN);
        }
        return double_of(sign | ((u64)TOP << FRACTION));
    }
    if (left_zero || right_zero) {
        return double_of(sign);
    }

    u64 left_significand;
    u64 right_significand;
    int left_exponent;
    int right_exponent;
    unpack(left_pattern, &left_significand, &left_exponent);
    unpack(right_pattern, &right_significand, &right_exponent);
    normalize(&left_significand, &left_exponent);
    normalize(&right_significand, &right_exponent);

    /* The product is a hundred and five or a hundred and six bits and what the rounding wants is
     * fifty six, so everything below those goes into the sticky bit. The exponent that goes with the
     * rest is the two exponents added with one bias taken back out.
     */
    u64 high;
    u64 low;
    multiply_wide(left_significand, right_significand, &high, &low);
    int exponent = left_exponent + right_exponent - BIAS;
    u64 significand = shift_down_wide(high, low, FRACTION - EXTRA);
    if (significand >= ((u64)1 << (LEADING + 1))) {
        significand = shift_down(significand, 1);
        exponent += 1;
    }
    return round_and_pack(sign, exponent, significand);
}

double __divdf3(double left, double right) {
    u64 left_pattern = pattern_of(left);
    u64 right_pattern = pattern_of(right);
    if (is_nan(left_pattern)) {
        return quiet(left_pattern);
    }
    if (is_nan(right_pattern)) {
        return quiet(right_pattern);
    }
    u64 sign = (left_pattern ^ right_pattern) & SIGN;
    int left_zero = (left_pattern & ~SIGN) == 0;
    int right_zero = (right_pattern & ~SIGN) == 0;
    if (is_infinite(left_pattern)) {
        /* An infinity over an infinity has no answer. Over anything else it is an infinity. */
        if (is_infinite(right_pattern)) {
            return double_of(EMPTY_NAN);
        }
        return double_of(sign | ((u64)TOP << FRACTION));
    }
    if (is_infinite(right_pattern)) {
        return double_of(sign);
    }
    if (right_zero) {
        /* A zero over a zero has no answer. Anything else over a zero is an infinity, which is the
         * one division by zero that is defined, because IEEE 754 defines it and C leaves it to the
         * implementation rather than calling it undefined the way it does the integer one.
         */
        if (left_zero) {
            return double_of(EMPTY_NAN);
        }
        return double_of(sign | ((u64)TOP << FRACTION));
    }
    if (left_zero) {
        return double_of(sign);
    }

    u64 left_significand;
    u64 right_significand;
    int left_exponent;
    int right_exponent;
    unpack(left_pattern, &left_significand, &left_exponent);
    unpack(right_pattern, &right_significand, &right_exponent);
    normalize(&left_significand, &left_exponent);
    normalize(&right_significand, &right_exponent);

    /* Long division, a bit at a time, fifty seven bits of it. Two normalized significands give a
     * quotient between a half and two, so fifty seven bits put the leading one at LEADING or one
     * above it, and what is left in the remainder at the end is the sticky bit. The remainder stays
     * below the divisor, which is why this needs no more room than one format down did.
     */
    u64 remainder = left_significand;
    u64 divisor = right_significand;
    u64 quotient = 0;
    for (int at = 0; at < LEADING + 2; at++) {
        quotient <<= 1;
        if (remainder >= divisor) {
            remainder -= divisor;
            quotient |= 1;
        }
        remainder <<= 1;
    }
    int exponent = left_exponent - right_exponent + BIAS - 1;
    u64 sticky = remainder != 0;
    if (quotient >= ((u64)1 << (LEADING + 1))) {
        sticky |= quotient & 1;
        quotient >>= 1;
        exponent += 1;
    }
    return round_and_pack(sign, exponent, quotient | sticky);
}

/* Negation is the sign bit and nothing else, which is true of a not a number too: the sign of one
 * says nothing, and flipping it rather than quieting it is what libgcc does here.
 */
double __negdf2(double value) {
    return double_of(pattern_of(value) ^ SIGN);
}
