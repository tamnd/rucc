/* Quad precision arithmetic done in integers, which is what binary128 is on every target.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8 and spec/cross-compile/10-runtime.md section 10.2.
 * The four operations, the negation and the eight comparisons on a `_Float128`, under libgcc's names.
 *
 * # Why this one is not about a target without a floating point unit
 *
 * runtime/builtins/float.c and runtime/builtins/double.c are there for a target whose hardware cannot
 * add two floats. This file is not: no machine anyone compiles for has an instruction that adds two
 * binary128 values, so `a + b` on a pair of them is a call here on every row of the target matrix,
 * including the ones with the largest floating point units on them. That is why section 10.2 calls it
 * the largest single piece of the runtime rather than an option for small targets, and it is why the
 * back end had to learn to hold one of these in a register before any of this could be reached.
 *
 * # What is the same as one format down and what is not
 *
 * The shape is the same, which is the reason the formats went in one at a time: three rounding bits
 * under the significand, a tie that goes to the even one, and no exception raised and no rounding mode
 * read anywhere, for the reasons float.c gives.
 *
 * What changes is that a significand no longer fits in a word at all. A hundred and thirteen bits of
 * it, with three rounding bits under that, is a hundred and sixteen, so every significand here is a
 * pair of words and every operation on one is written out: the add, the subtract, the comparison and
 * the two shifts. `__int128` would say all of that in one line and is not allowed, for two reasons
 * rather than one. A 32-bit target does not have the type, and on a target that does, a division or a
 * multiplication at that width is a call to the very archive this file is part of.
 *
 * So the multiplication is four word sized products rather than one, the way double.c builds its
 * product out of four half word ones, and it carries two hundred and twenty six bits of answer in four
 * words. The division is the same bit at a time loop as both formats below, over a pair of words, and
 * it needs no more room than the operands do because a remainder stays below the divisor.
 *
 * # Which not a number comes out
 *
 * Two of these answers are a choice rather than a result, and both were held against libgcc before
 * they were written down. When an operation has two not a numbers in front of it, the one that comes
 * back is the left one, made quiet, which is float.c and double.c's rule as well. When an operation
 * has no answer at all, what comes back is a quiet not a number with an empty payload and a clear
 * sign, which is the same in all three files. libgcc answers both of those out of a per architecture
 * header, so it picks the right operand on x86 and the left one on arm and a clear sign on one and a
 * set sign on the other, and since one archive here serves every row of the matrix it has one rule
 * instead. The sign of a subtraction's not a number is the one thing in this area that is not a per
 * machine choice in libgcc, and that one this file matches.
 *
 * # Reading the bits
 *
 * Through a union, which is what C says to write for this, and then through the two halves of it. Which
 * half holds the sign is a property of the target rather than of the format, so it is asked about once,
 * at the top, and nothing below here mentions a byte order again.
 */

typedef unsigned long long u64;

/* How many bits of the significand the format writes down, the other one being implied. */
#define FRACTION 112

/* How many of those are in the word the sign and the exponent are in, which is where every field of
 * the format except the low fraction bits lives.
 */
#define FRACTION_HIGH (FRACTION - 64)

/* What is added to an exponent before it is stored, so that the field holds no negative number. */
#define BIAS 16383

/* The exponent field of an infinity and of a not a number, which is every bit of it set. */
#define TOP 32767

#define SIGN_HIGH 0x8000000000000000ull
#define IMPLICIT_HIGH 0x0001000000000000ull
#define FRACTION_MASK_HIGH 0x0000FFFFFFFFFFFFull

/* The top bit of the fraction, which is what tells a quiet not a number from a signalling one. */
#define QUIET_HIGH 0x0000800000000000ull

/* The not a number an operation with no answer produces: quiet, positive, and nothing else. */
#define EMPTY_NAN_HIGH 0x7FFF800000000000ull

/* Guard, round and sticky. The significands below are shifted up by this much. */
#define EXTRA 3

/* Where the leading one of a normalized significand sits once it has been shifted up, as a bit of the
 * high word. It is a hundred and fifteen bits up, so it is in that word and never in the low one,
 * which is why the two tests against it below are one word's comparison rather than a pair's.
 */
#define LEADING (FRACTION + EXTRA)
#define LEADING_HIGH ((u64)1 << (LEADING - 64))

/* The low half of a word, for the multiplication that works in halves. */
#define HALF 0xFFFFFFFFull

/* A hundred and twenty eight bits, as the two words it is made of. Every significand, every pattern
 * and every intermediate value in this file is one of these.
 */
struct wide {
    u64 high;
    u64 low;
};

/* Which half of a pair in memory holds the sign and the exponent.
 *
 * A little endian target writes the low word first and a big endian one writes the high word first,
 * and this file is compiled for both. gcc and this compiler both define the two names below, and a
 * target that defined neither would be read as little endian, which is what every row of the matrix
 * that has got this far is.
 */
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) \
    && __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
#define HIGH_WORD 0
#define LOW_WORD 1
#else
#define HIGH_WORD 1
#define LOW_WORD 0
#endif

union bits {
    _Float128 number;
    u64 words[2];
};

static struct wide make(u64 high, u64 low) {
    struct wide out;
    out.high = high;
    out.low = low;
    return out;
}

static struct wide pattern_of(_Float128 value) {
    union bits at;
    at.number = value;
    return make(at.words[HIGH_WORD], at.words[LOW_WORD]);
}

static _Float128 quad_of(struct wide pattern) {
    union bits at;
    at.words[HIGH_WORD] = pattern.high;
    at.words[LOW_WORD] = pattern.low;
    return at.number;
}

static int is_zero_wide(struct wide value) {
    return value.high == 0 && value.low == 0;
}

static int same(struct wide left, struct wide right) {
    return left.high == right.high && left.low == right.low;
}

/* Whether the left one is at or above the right one, read as one unsigned number in two words. */
static int at_least(struct wide left, struct wide right) {
    if (left.high != right.high) {
        return left.high > right.high;
    }
    return left.low >= right.low;
}

/* And whether it is above it, which is what the two callers that have already ruled equality out of
 * their way would get wrong with the test above.
 */
static int above(struct wide left, struct wide right) {
    if (left.high != right.high) {
        return left.high > right.high;
    }
    return left.low > right.low;
}

static struct wide add_wide(struct wide left, struct wide right) {
    u64 low = left.low + right.low;
    /* The carry out of the low word, which is there exactly when the sum came out below one of the
     * two things that went into it.
     */
    return make(left.high + right.high + (low < left.low), low);
}

static struct wide subtract_wide(struct wide left, struct wide right) {
    return make(left.high - right.high - (left.low < right.low), left.low - right.low);
}

/* Shifts up by `up`, which is between one and sixty three in every caller, so neither shift here is
 * the undefined one. Nothing above the top is kept, and no caller asks it to be: every value that
 * goes up has room above it by construction.
 */
static struct wide shift_up(struct wide value, int up) {
    return make((value.high << up) | (value.low >> (64 - up)), value.low << up);
}

/* Shifts down by `down` and keeps nothing, for the caller that has already read the bits it is
 * dropping. `down` is three in the one place this is used, which is where `round_and_pack` takes the
 * rounding bits off a significand it has just looked at.
 */
static struct wide drop_low(struct wide value, int down) {
    return make(value.high >> down, (value.low >> down) | (value.high << (64 - down)));
}

/* Shifts down by `down`, keeping whatever falls off in the lowest bit, which is the sticky bit and
 * means "and there was more below this".
 *
 * Three cases rather than one because a shift by sixty four or more is undefined in C and this is
 * handed a distance between two exponents, which can be anything at all.
 */
static struct wide shift_down(struct wide value, int down) {
    if (down <= 0) {
        return value;
    }
    if (down >= 128) {
        return make(0, !is_zero_wide(value));
    }
    if (down >= 64) {
        int across = down - 64;
        u64 lost = value.low | (value.high & (((u64)1 << across) - 1));
        return make(0, (value.high >> across) | (lost != 0));
    }
    u64 lost = value.low & (((u64)1 << down) - 1);
    struct wide kept = make(value.high >> down, (value.low >> down) | (value.high << (64 - down)));
    kept.low |= (lost != 0);
    return kept;
}

/* The pattern with the sign taken off, which is the magnitude and is what the comparisons order and
 * what the tests for a zero read.
 */
static struct wide magnitude_of(struct wide pattern) {
    return make(pattern.high & ~SIGN_HIGH, pattern.low);
}

static int exponent_of(struct wide pattern) {
    return (int)((pattern.high >> FRACTION_HIGH) & TOP);
}

static int fraction_is_set(struct wide pattern) {
    return (pattern.high & FRACTION_MASK_HIGH) != 0 || pattern.low != 0;
}

static int is_nan(struct wide pattern) {
    return exponent_of(pattern) == TOP && fraction_is_set(pattern);
}

static int is_infinite(struct wide pattern) {
    return exponent_of(pattern) == TOP && !fraction_is_set(pattern);
}

/* The significand with the leading one written out where the format only implies it, and the exponent
 * the arithmetic wants. A subnormal gets the exponent of the smallest normal, which is the one it
 * shares, and no leading one, which is the only difference between the two cases.
 */
static void unpack(struct wide pattern, struct wide *significand, int *exponent) {
    int stored = exponent_of(pattern);
    struct wide fraction = make(pattern.high & FRACTION_MASK_HIGH, pattern.low);
    if (stored == 0) {
        *significand = fraction;
        *exponent = 1;
    } else {
        *significand = make(fraction.high | IMPLICIT_HIGH, fraction.low);
        *exponent = stored;
    }
}

static _Float128 infinity(u64 sign) {
    return quad_of(make(sign | ((u64)TOP << FRACTION_HIGH), 0));
}

static _Float128 quiet(struct wide pattern) {
    return quad_of(make(pattern.high | QUIET_HIGH, pattern.low));
}

/* The quad nearest the value this sign, exponent and significand describe.
 *
 * The same three departures the narrower files allow are allowed here: an exponent at or below zero,
 * which is a result too small to hold normally, and an exponent of one with the leading one missing,
 * which is what a cancellation leaves at the bottom of the range. Both are a subnormal and both are
 * handled by getting the stored exponent to zero.
 *
 * The rounding is done on the packed result rather than on the significand, so that an increment which
 * carries out of the fraction carries into the exponent on its own and one that carries out of the
 * largest finite number lands on the infinity that is the right answer. Here that increment is a
 * hundred and twenty eight bit one, so it is the same add the arithmetic above uses.
 */
static _Float128 round_and_pack(u64 sign, int exponent, struct wide significand) {
    if (exponent <= 0) {
        significand = shift_down(significand, 1 - exponent);
        exponent = 0;
    } else if (significand.high < LEADING_HIGH) {
        exponent = 0;
    }
    if (exponent >= TOP) {
        return infinity(sign);
    }
    u64 below = significand.low & (((u64)1 << EXTRA) - 1);
    struct wide result = drop_low(significand, EXTRA);
    result.high = sign | ((u64)exponent << FRACTION_HIGH) | (result.high & FRACTION_MASK_HIGH);
    if (below > ((u64)1 << (EXTRA - 1))) {
        result = add_wide(result, make(0, 1));
    } else if (below == ((u64)1 << (EXTRA - 1))) {
        /* Exactly halfway, so the even one of the two neighbours wins, which is the one whose lowest
         * bit is already zero.
         */
        result = add_wide(result, make(0, result.low & 1));
    }
    return quad_of(result);
}

/* A significand with its leading one brought up to where the multiplication and the division want it,
 * with the exponent moved down to match. This is only ever a subnormal: anything else arrives from
 * `unpack` already there.
 */
static void normalize(struct wide *significand, int *exponent) {
    while ((significand->high >> FRACTION_HIGH) == 0) {
        *significand = shift_up(*significand, 1);
        *exponent -= 1;
    }
}

static _Float128 add(struct wide left, struct wide right) {
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
        if (is_infinite(right) && ((left.high ^ right.high) & SIGN_HIGH) != 0) {
            return quad_of(make(EMPTY_NAN_HIGH, 0));
        }
        return quad_of(left);
    }
    if (is_infinite(right)) {
        return quad_of(right);
    }

    struct wide left_significand;
    struct wide right_significand;
    int left_exponent;
    int right_exponent;
    unpack(left, &left_significand, &left_exponent);
    unpack(right, &right_significand, &right_exponent);

    if (is_zero_wide(left_significand) && is_zero_wide(right_significand)) {
        /* Two zeros, and the answer is negative only when both of them are, which is what rounding to
         * nearest asks for.
         */
        return quad_of(make(left.high & right.high & SIGN_HIGH, 0));
    }
    if (is_zero_wide(left_significand)) {
        return quad_of(right);
    }
    if (is_zero_wide(right_significand)) {
        return quad_of(left);
    }

    /* The larger magnitude first, because the sign of the answer is its sign and because the smaller
     * one is the one that gets lined up underneath.
     */
    struct wide larger = shift_up(left_significand, EXTRA);
    struct wide smaller = shift_up(right_significand, EXTRA);
    int high = left_exponent;
    int low = right_exponent;
    u64 sign = left.high & SIGN_HIGH;
    int opposite = ((left.high ^ right.high) & SIGN_HIGH) != 0;
    if (right_exponent > left_exponent
        || (right_exponent == left_exponent && above(smaller, larger))) {
        struct wide swap = larger;
        larger = smaller;
        smaller = swap;
        high = right_exponent;
        low = left_exponent;
        sign = right.high & SIGN_HIGH;
    }

    /* Line the smaller one up under the larger, keeping what falls off in the sticky bit. */
    smaller = shift_down(smaller, high - low);
    int exponent = high;

    struct wide significand;
    if (opposite) {
        significand = subtract_wide(larger, smaller);
        if (is_zero_wide(significand)) {
            /* A number less itself is a positive zero in every rounding mode but one this does not
             * have.
             */
            return quad_of(make(0, 0));
        }
        while (significand.high < LEADING_HIGH && exponent > 1) {
            significand = shift_up(significand, 1);
            exponent -= 1;
        }
    } else {
        significand = add_wide(larger, smaller);
        if (significand.high >= (LEADING_HIGH << 1)) {
            significand = shift_down(significand, 1);
            exponent += 1;
        }
    }
    return round_and_pack(sign, exponent, significand);
}

_Float128 __addtf3(_Float128 left, _Float128 right) {
    return add(pattern_of(left), pattern_of(right));
}

/* Subtraction is addition with the sign of the right operand flipped, which is exact: a sign is a bit
 * and flipping it is the negation of the value it belongs to, at every magnitude.
 *
 * A not a number is the one pattern that is not flipped, and not because the arithmetic would be
 * wrong. It is the answer itself, and the answer a subtraction gives back is the one that came in,
 * so flipping it would make a program print minus where GCC prints nothing. soft-fp negates after it
 * has dealt with a not a number, on every target and not as a per machine convention, so this is the
 * one place the sign of one is worth matching.
 */
_Float128 __subtf3(_Float128 left, _Float128 right) {
    struct wide right_pattern = pattern_of(right);
    if (!is_nan(right_pattern)) {
        right_pattern.high ^= SIGN_HIGH;
    }
    return add(pattern_of(left), right_pattern);
}

/* The product of two words, which does not fit in one. Four products of thirty two bit halves, added
 * up with the carries written out, which is double.c's routine unchanged: the arithmetic a word wide
 * machine can do is the same whatever the format above it is.
 */
static void multiply_words(u64 left, u64 right, u64 *high, u64 *low) {
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

/* The product of two significands, which is two hundred and twenty six bits and so is four words.
 *
 * Four word sized products, one per pair of halves, added into the four columns. Each column is added
 * up with its own carry written out rather than through a wider type, since a wider type is the thing
 * this file does not have. The top column cannot carry anywhere: a significand is a hundred and
 * thirteen bits, so the product of the two high halves is at most ninety eight and has room above it.
 */
static void multiply_wide(struct wide left, struct wide right, u64 product[4]) {
    u64 low_high;
    u64 low_low;
    u64 cross_one_high;
    u64 cross_one_low;
    u64 cross_two_high;
    u64 cross_two_low;
    u64 high_high;
    u64 high_low;
    multiply_words(left.low, right.low, &low_high, &low_low);
    multiply_words(left.high, right.low, &cross_one_high, &cross_one_low);
    multiply_words(left.low, right.high, &cross_two_high, &cross_two_low);
    multiply_words(left.high, right.high, &high_high, &high_low);

    product[0] = low_low;

    u64 column = low_high;
    u64 carry = 0;
    column += cross_one_low;
    carry += (column < cross_one_low);
    column += cross_two_low;
    carry += (column < cross_two_low);
    product[1] = column;

    column = high_low;
    u64 carry_up = 0;
    column += cross_one_high;
    carry_up += (column < cross_one_high);
    column += cross_two_high;
    carry_up += (column < cross_two_high);
    column += carry;
    carry_up += (column < carry);
    product[2] = column;

    product[3] = high_high + carry_up;
}

/* The top bits of a four word product, shifted down by `down` with everything below them in the
 * sticky bit. The one caller passes a hundred and nine, which is past the low word and not past the
 * second one, so the shifts here are all of them ones that do something.
 */
static struct wide shift_down_product(const u64 product[4], int down) {
    int across = down - 64;
    u64 lost = product[0] | (product[1] & (((u64)1 << across) - 1));
    struct wide kept = make((product[2] >> across) | (product[3] << (64 - across)),
                            (product[1] >> across) | (product[2] << (64 - across)));
    kept.low |= (lost != 0);
    return kept;
}

_Float128 __multf3(_Float128 left, _Float128 right) {
    struct wide left_pattern = pattern_of(left);
    struct wide right_pattern = pattern_of(right);
    if (is_nan(left_pattern)) {
        return quiet(left_pattern);
    }
    if (is_nan(right_pattern)) {
        return quiet(right_pattern);
    }
    u64 sign = (left_pattern.high ^ right_pattern.high) & SIGN_HIGH;
    int left_zero = is_zero_wide(magnitude_of(left_pattern));
    int right_zero = is_zero_wide(magnitude_of(right_pattern));
    if (is_infinite(left_pattern) || is_infinite(right_pattern)) {
        /* A zero times an infinity has no answer. An infinity times anything else is an infinity, and
         * the sign is the two signs multiplied whatever the magnitudes were.
         */
        if (left_zero || right_zero) {
            return quad_of(make(EMPTY_NAN_HIGH, 0));
        }
        return infinity(sign);
    }
    if (left_zero || right_zero) {
        return quad_of(make(sign, 0));
    }

    struct wide left_significand;
    struct wide right_significand;
    int left_exponent;
    int right_exponent;
    unpack(left_pattern, &left_significand, &left_exponent);
    unpack(right_pattern, &right_significand, &right_exponent);
    normalize(&left_significand, &left_exponent);
    normalize(&right_significand, &right_exponent);

    /* The product is two hundred and twenty five or two hundred and twenty six bits and what the
     * rounding wants is a hundred and sixteen, so everything below those goes into the sticky bit. The
     * exponent that goes with the rest is the two exponents added with one bias taken back out.
     */
    u64 product[4];
    multiply_wide(left_significand, right_significand, product);
    int exponent = left_exponent + right_exponent - BIAS;
    struct wide significand = shift_down_product(product, FRACTION - EXTRA);
    if (significand.high >= (LEADING_HIGH << 1)) {
        significand = shift_down(significand, 1);
        exponent += 1;
    }
    return round_and_pack(sign, exponent, significand);
}

_Float128 __divtf3(_Float128 left, _Float128 right) {
    struct wide left_pattern = pattern_of(left);
    struct wide right_pattern = pattern_of(right);
    if (is_nan(left_pattern)) {
        return quiet(left_pattern);
    }
    if (is_nan(right_pattern)) {
        return quiet(right_pattern);
    }
    u64 sign = (left_pattern.high ^ right_pattern.high) & SIGN_HIGH;
    int left_zero = is_zero_wide(magnitude_of(left_pattern));
    int right_zero = is_zero_wide(magnitude_of(right_pattern));
    if (is_infinite(left_pattern)) {
        /* An infinity over an infinity has no answer. Over anything else it is an infinity. */
        if (is_infinite(right_pattern)) {
            return quad_of(make(EMPTY_NAN_HIGH, 0));
        }
        return infinity(sign);
    }
    if (is_infinite(right_pattern)) {
        return quad_of(make(sign, 0));
    }
    if (right_zero) {
        /* A zero over a zero has no answer. Anything else over a zero is an infinity, which is the one
         * division by zero that is defined, because IEEE 754 defines it and C leaves it to the
         * implementation rather than calling it undefined the way it does the integer one.
         */
        if (left_zero) {
            return quad_of(make(EMPTY_NAN_HIGH, 0));
        }
        return infinity(sign);
    }
    if (left_zero) {
        return quad_of(make(sign, 0));
    }

    struct wide left_significand;
    struct wide right_significand;
    int left_exponent;
    int right_exponent;
    unpack(left_pattern, &left_significand, &left_exponent);
    unpack(right_pattern, &right_significand, &right_exponent);
    normalize(&left_significand, &left_exponent);
    normalize(&right_significand, &right_exponent);

    /* Long division, a bit at a time, a hundred and seventeen bits of it. Two normalized significands
     * give a quotient between a half and two, so a hundred and seventeen bits put the leading one at
     * LEADING or one above it, and what is left in the remainder at the end is the sticky bit. The
     * remainder stays below the divisor, which is why this needs no more room than the operands do.
     */
    struct wide remainder = left_significand;
    struct wide divisor = right_significand;
    struct wide quotient = make(0, 0);
    for (int at = 0; at < LEADING + 2; at++) {
        quotient = shift_up(quotient, 1);
        if (at_least(remainder, divisor)) {
            remainder = subtract_wide(remainder, divisor);
            quotient.low |= 1;
        }
        remainder = shift_up(remainder, 1);
    }
    int exponent = left_exponent - right_exponent + BIAS - 1;
    u64 sticky = !is_zero_wide(remainder);
    if (quotient.high >= (LEADING_HIGH << 1)) {
        sticky |= quotient.low & 1;
        quotient = drop_low(quotient, 1);
        exponent += 1;
    }
    quotient.low |= sticky;
    return round_and_pack(sign, exponent, quotient);
}

/* Negation is the sign bit and nothing else, which is true of a not a number too: the sign of one says
 * nothing, and flipping it rather than quieting it is what libgcc does here.
 */
_Float128 __negtf2(_Float128 value) {
    struct wide pattern = pattern_of(value);
    pattern.high ^= SIGN_HIGH;
    return quad_of(pattern);
}

/* The comparisons, which are the same eight routines over the same one piece of work the two narrower
 * files have, with a pattern that is now a pair of words.
 *
 * Only the sign of what these hand back is specified and never its magnitude, so the caller tests the
 * answer against zero, and what differs between the eight is one number: what to answer when an
 * operand is a not a number, which has to be whichever sign makes the caller's own test come out
 * false. The work itself is a comparison of two unsigned integers, because the format orders two
 * values of the same sign the way it orders their patterns, and that holds at this width for the same
 * reason it holds at the narrower ones: the exponent sits above the fraction and both are unsigned.
 */
static int compare(struct wide left, struct wide right, int unordered) {
    if (is_nan(left) || is_nan(right)) {
        return unordered;
    }
    struct wide left_magnitude = magnitude_of(left);
    struct wide right_magnitude = magnitude_of(right);
    /* A negative zero equals a positive zero, which is the one place the sign is not read. */
    if (is_zero_wide(left_magnitude) && is_zero_wide(right_magnitude)) {
        return 0;
    }
    int left_negative = (left.high & SIGN_HIGH) != 0;
    int right_negative = (right.high & SIGN_HIGH) != 0;
    if (left_negative != right_negative) {
        return left_negative ? -1 : 1;
    }
    if (same(left_magnitude, right_magnitude)) {
        return 0;
    }
    int ordered = above(left_magnitude, right_magnitude) ? 1 : -1;
    /* Both negative, so the larger magnitude is the smaller number. */
    return left_negative ? -ordered : ordered;
}

/* The three way comparison, which answers one for a not a number as well. The documentation says not
 * to rely on that and the compiler emits this routine only where it has ruled one out already.
 */
int __cmptf2(_Float128 left, _Float128 right) {
    return compare(pattern_of(left), pattern_of(right), 1);
}

/* Zero when the two are equal and anything else when they are not. A not a number is unequal to
 * everything including itself, so the answer there is not zero.
 */
int __eqtf2(_Float128 left, _Float128 right) {
    return compare(pattern_of(left), pattern_of(right), 1);
}

/* The same work as the one above it, since a caller testing for inequality tests the same answer
 * against zero the other way round. Two functions rather than one alias for float.c's reason: an alias
 * is a linker feature and this file is meant to compile with nothing underneath it.
 */
int __netf2(_Float128 left, _Float128 right) {
    return compare(pattern_of(left), pattern_of(right), 1);
}

/* At or above zero when the left one is greater or equal, so a not a number has to come back below
 * zero for the test to fail.
 */
int __getf2(_Float128 left, _Float128 right) {
    return compare(pattern_of(left), pattern_of(right), -1);
}

/* Above zero when the left one is greater, so a not a number comes back at or below zero. */
int __gttf2(_Float128 left, _Float128 right) {
    return compare(pattern_of(left), pattern_of(right), -1);
}

/* At or below zero when the left one is less or equal, so a not a number comes back above zero. */
int __letf2(_Float128 left, _Float128 right) {
    return compare(pattern_of(left), pattern_of(right), 1);
}

/* Below zero when the left one is less, so a not a number comes back at or above zero. */
int __lttf2(_Float128 left, _Float128 right) {
    return compare(pattern_of(left), pattern_of(right), 1);
}

/* Not zero when the two cannot be ordered, which is when either of them is a not a number. This is the
 * one the others are defined in terms of: each of them is its own comparison and this answer.
 */
int __unordtf2(_Float128 left, _Float128 right) {
    return is_nan(pattern_of(left)) || is_nan(pattern_of(right));
}
