/* Single precision arithmetic done in integers, for a target with no floating point unit.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8 and spec/cross-compile/10-runtime.md section
 * 10.2, which lists soft float as wanted on armv7 soft-float and on any target without an FPU. On
 * such a target an addition of two floats is not an instruction, so the front end emits a call and
 * the names in here are what it calls: the four operations and the negation, the eight comparisons
 * further down, since `a < b` on two floats is a call there as much as `a + b` is, and the eight
 * conversions after them, since a cast between a float and an integer is one too, and the pair at the
 * very bottom that crosses between this format and the next one up. They are libgcc's names, for the
 * reason every other name in this directory is: an object we produced gets linked against objects GCC
 * produced and one of us has to give way.
 *
 * This is the single precision half. The double precision set is the same routines over a wider field
 * layout and is in double.c next door, and binary128 is the large piece section 10.2 calls the largest
 * in the document, which wanted this one to exist first because it is where the shape got settled.
 *
 * # What correct means here
 *
 * Every routine returns the float nearest the exact mathematical answer, with a tie going to the
 * even significand, which is IEEE 754 round to nearest even and is the only mode C requires a
 * program to get without asking. No other rounding mode is implemented: `fesetround` on a soft
 * float target would have to reach these routines through a variable they all read, and the
 * variable is a thing to add when a target needs it rather than a thing to carry unused.
 *
 * Nothing here raises a floating point exception or sets a flag, because there is no status word on
 * a machine with no FPU and inventing one in a global would be this library describing a register
 * the target does not have.
 *
 * # Three bits below the significand, which is all it takes
 *
 * Each routine builds a significand with the twenty four bits the format keeps and three more below
 * them: a guard bit, a round bit, and one that says something was lost further down. Three is not a
 * guess, it is the known bound. Two values whose exponents differ by more than one cannot cancel to
 * nothing, so the case that needs many bits of the smaller operand is the case that keeps the
 * leading bits of the larger, and the case that loses the leading bits is the case where no bits
 * were dropped at all. So the rounding only ever has to know the bit below the result, the bit
 * below that, and whether anything at all was underneath, and that is what the three bits are.
 *
 * # The quiet bit and which not a number comes back
 *
 * An input that is a not a number comes back quieted, and the left one where both are. That is what
 * libgcc does, it is what this machine's own addition does with the operands in the same order, and
 * it is the convention the reference in runtime/rucc-builtins makes too so that the two can be
 * compared bit for bit rather than through a canonicalization that would hide a real difference.
 * An operation with no answer at all, which is an infinity less an infinity, a zero times an
 * infinity, and a zero over a zero, produces the quiet not a number with an empty payload, which is
 * the one every implementation agrees on.
 *
 * # Reading the bits
 *
 * Through a union, which is what C says to write for this. A cast through a pointer would be an
 * aliasing violation in a file whose whole purpose is to be correct about representations.
 */

typedef unsigned int u32;
typedef unsigned long long u64;

/* How many bits of the significand the format writes down, the other one being implied. */
#define FRACTION 23

/* What is added to an exponent before it is stored, so that the field holds no negative number. */
#define BIAS 127

/* The exponent field of an infinity and of a not a number, which is every bit of it set. */
#define TOP 255

#define SIGN 0x80000000u
#define IMPLICIT 0x00800000u
#define FRACTION_MASK 0x007fffffu

/* The top bit of the fraction, which is what tells a quiet not a number from a signalling one. */
#define QUIET 0x00400000u

/* The not a number an operation with no answer produces: quiet, positive, and nothing else. */
#define EMPTY_NAN 0x7fc00000u

/* Guard, round and sticky. The significands below are shifted up by this much. */
#define EXTRA 3

/* Where the leading one of a normalized significand sits once it has been shifted up. */
#define LEADING (FRACTION + EXTRA)

union bits {
    float number;
    u32 pattern;
};

static u32 pattern_of(float value) {
    union bits at;
    at.number = value;
    return at.pattern;
}

static float float_of(u32 pattern) {
    union bits at;
    at.pattern = pattern;
    return at.number;
}

static int exponent_of(u32 pattern) {
    return (int)((pattern >> FRACTION) & TOP);
}

static u32 fraction_of(u32 pattern) {
    return pattern & FRACTION_MASK;
}

static int is_nan(u32 pattern) {
    return exponent_of(pattern) == TOP && fraction_of(pattern) != 0;
}

static int is_infinite(u32 pattern) {
    return exponent_of(pattern) == TOP && fraction_of(pattern) == 0;
}

/* The significand with the leading one written out where the format only implies it, and the
 * exponent the arithmetic wants.
 *
 * A subnormal gets the exponent of the smallest normal, which is the one it shares: the format says
 * a stored exponent of zero means the same power of two as a stored one, with no leading one added.
 * So the only difference between the two cases here is that leading one.
 */
static void unpack(u32 pattern, u32 *significand, int *exponent) {
    int stored = exponent_of(pattern);
    u32 fraction = fraction_of(pattern);
    if (stored == 0) {
        *significand = fraction;
        *exponent = 1;
    } else {
        *significand = fraction | IMPLICIT;
        *exponent = stored;
    }
}

/* Shifts a significand down by `down` bits, keeping whatever falls off in the lowest bit.
 *
 * That lowest bit is the sticky one, and what it means is "and there was more below this". A shift
 * wide enough to lose everything is not undefined here the way the shift itself would be: the
 * answer is whether anything was there at all.
 */
static u64 shift_down(u64 significand, int down) {
    if (down >= 64) {
        return significand != 0;
    }
    u64 lost = significand & (((u64)1 << down) - 1);
    return (significand >> down) | (lost != 0);
}

/* The float nearest the value this sign, exponent and significand describe.
 *
 * What it is handed is a significand whose leading one is at `LEADING`, with the three rounding
 * bits below it, and the exponent that significand goes with. Two departures from that are allowed
 * because they are what the callers produce: an exponent at or below zero, which is a result too
 * small for the format to hold normally, and an exponent of exactly one with the leading one
 * missing, which is what a cancellation leaves at the bottom of the range. Both of those are a
 * subnormal and both are handled by getting the stored exponent to zero.
 *
 * The rounding is done on the packed result rather than on the significand, which is the trick
 * worth knowing in this file: the leading one is masked off and the exponent added in first, so an
 * increment that carries out of the fraction carries into the exponent on its own, and one that
 * carries out of the largest finite number lands on the infinity that is the right answer.
 */
static float round_and_pack(u32 sign, int exponent, u64 significand) {
    if (exponent <= 0) {
        significand = shift_down(significand, 1 - exponent);
        exponent = 0;
    } else if (significand < ((u64)1 << LEADING)) {
        exponent = 0;
    }
    if (exponent >= TOP) {
        return float_of(sign | ((u32)TOP << FRACTION));
    }
    u32 below = (u32)(significand & (((u32)1 << EXTRA) - 1));
    u32 result = sign | ((u32)exponent << FRACTION);
    result |= (u32)(significand >> EXTRA) & FRACTION_MASK;
    if (below > ((u32)1 << (EXTRA - 1))) {
        result += 1;
    } else if (below == ((u32)1 << (EXTRA - 1))) {
        /* Exactly halfway, so the even one of the two neighbours wins, which is the one whose
         * lowest bit is already zero.
         */
        result += result & 1;
    }
    return float_of(result);
}

/* A significand with its leading one brought up to where the multiplication and the division below
 * want it, with the exponent moved down to match.
 *
 * This is only ever a subnormal: anything else arrives from `unpack` already there. A loop rather
 * than a count of the leading zeros, because the count is a builtin and the point of this file is
 * that it needs nothing underneath it.
 */
static void normalize(u32 *significand, int *exponent) {
    while ((*significand >> FRACTION) == 0) {
        *significand <<= 1;
        *exponent -= 1;
    }
}

static float quiet(u32 pattern) {
    return float_of(pattern | QUIET);
}

static float add(u32 left, u32 right) {
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
            return float_of(EMPTY_NAN);
        }
        return float_of(left);
    }
    if (is_infinite(right)) {
        return float_of(right);
    }

    u32 left_significand;
    u32 right_significand;
    int left_exponent;
    int right_exponent;
    unpack(left, &left_significand, &left_exponent);
    unpack(right, &right_significand, &right_exponent);

    if (left_significand == 0 && right_significand == 0) {
        /* Two zeros, and the answer is negative only when both of them are: rounding to nearest
         * makes a positive zero out of the mixed pair, which is what IEEE 754 asks for.
         */
        return float_of(left & right & SIGN);
    }
    if (left_significand == 0) {
        return float_of(right);
    }
    if (right_significand == 0) {
        return float_of(left);
    }

    /* The larger magnitude first, because the sign of the answer is its sign and because the
     * smaller one is the one that gets lined up underneath. Equal exponents are decided by the
     * significands, which at equal exponents is the whole of the magnitude.
     */
    u64 larger = (u64)left_significand << EXTRA;
    u64 smaller = (u64)right_significand << EXTRA;
    int high = left_exponent;
    int low = right_exponent;
    u32 sign = left & SIGN;
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
            /* A number less itself is a positive zero, in every rounding mode but one this does
             * not have.
             */
            return float_of(0);
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

float __addsf3(float left, float right) {
    return add(pattern_of(left), pattern_of(right));
}

/* Subtraction is addition with the sign of the right operand flipped, which is exact: a sign is a
 * bit and flipping it is the negation of the value it belongs to, at every magnitude including zero
 * and infinity.
 */
float __subsf3(float left, float right) {
    return add(pattern_of(left), pattern_of(right) ^ SIGN);
}

float __mulsf3(float left, float right) {
    u32 left_pattern = pattern_of(left);
    u32 right_pattern = pattern_of(right);
    if (is_nan(left_pattern)) {
        return quiet(left_pattern);
    }
    if (is_nan(right_pattern)) {
        return quiet(right_pattern);
    }
    u32 sign = (left_pattern ^ right_pattern) & SIGN;
    int left_zero = (left_pattern & ~SIGN) == 0;
    int right_zero = (right_pattern & ~SIGN) == 0;
    if (is_infinite(left_pattern) || is_infinite(right_pattern)) {
        /* A zero times an infinity has no answer. An infinity times anything else is an infinity,
         * and the sign is the two signs multiplied whatever the magnitudes were.
         */
        if (left_zero || right_zero) {
            return float_of(EMPTY_NAN);
        }
        return float_of(sign | ((u32)TOP << FRACTION));
    }
    if (left_zero || right_zero) {
        return float_of(sign);
    }

    u32 left_significand;
    u32 right_significand;
    int left_exponent;
    int right_exponent;
    unpack(left_pattern, &left_significand, &left_exponent);
    unpack(right_pattern, &right_significand, &right_exponent);
    normalize(&left_significand, &left_exponent);
    normalize(&right_significand, &right_exponent);

    /* The product of two twenty four bit significands is forty seven or forty eight bits, and what
     * the rounding wants is twenty seven. So the bottom twenty go into the sticky bit, and the
     * exponent that goes with the rest is the two exponents added with one bias taken back out.
     */
    u64 product = (u64)left_significand * (u64)right_significand;
    int exponent = left_exponent + right_exponent - BIAS;
    u64 significand = shift_down(product, FRACTION - EXTRA);
    if (significand >= ((u64)1 << (LEADING + 1))) {
        significand = shift_down(significand, 1);
        exponent += 1;
    }
    return round_and_pack(sign, exponent, significand);
}

float __divsf3(float left, float right) {
    u32 left_pattern = pattern_of(left);
    u32 right_pattern = pattern_of(right);
    if (is_nan(left_pattern)) {
        return quiet(left_pattern);
    }
    if (is_nan(right_pattern)) {
        return quiet(right_pattern);
    }
    u32 sign = (left_pattern ^ right_pattern) & SIGN;
    int left_zero = (left_pattern & ~SIGN) == 0;
    int right_zero = (right_pattern & ~SIGN) == 0;
    if (is_infinite(left_pattern)) {
        /* An infinity over an infinity has no answer. Over anything else it is an infinity. */
        if (is_infinite(right_pattern)) {
            return float_of(EMPTY_NAN);
        }
        return float_of(sign | ((u32)TOP << FRACTION));
    }
    if (is_infinite(right_pattern)) {
        return float_of(sign);
    }
    if (right_zero) {
        /* A zero over a zero has no answer. Anything else over a zero is an infinity, which is the
         * one division by zero that is defined, because IEEE 754 defines it and C leaves it to the
         * implementation rather than calling it undefined the way it does the integer one.
         */
        if (left_zero) {
            return float_of(EMPTY_NAN);
        }
        return float_of(sign | ((u32)TOP << FRACTION));
    }
    if (left_zero) {
        return float_of(sign);
    }

    u32 left_significand;
    u32 right_significand;
    int left_exponent;
    int right_exponent;
    unpack(left_pattern, &left_significand, &left_exponent);
    unpack(right_pattern, &right_significand, &right_exponent);
    normalize(&left_significand, &left_exponent);
    normalize(&right_significand, &right_exponent);

    /* Long division, a bit at a time, twenty eight bits of it. Two normalized significands give a
     * quotient between a half and two, so twenty eight bits put the leading one at LEADING or one
     * above it, and what is left in the remainder at the end is the sticky bit.
     */
    u64 remainder = (u64)left_significand;
    u64 divisor = (u64)right_significand;
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
float __negsf2(float value) {
    return float_of(pattern_of(value) ^ SIGN);
}

/* The comparisons.
 *
 * On a machine with no floating point unit, `a < b` on two floats is a call as much as `a + b` is,
 * and libgcc answers it with eight routines over one piece of work. What they share is this
 * function, and what differs between them is one number: what to hand back when an operand is a not
 * a number, which is an answer that has to make the caller's test fail.
 *
 * Only the sign of what these return is specified, not its magnitude, which is why the caller
 * compares the result against zero rather than against minus one. The order below is the order of
 * the bit patterns: two floats of the same sign compare the way their patterns do as integers, which
 * is the property the format was designed to have, so the whole of the work after the zeros and the
 * signs are out of the way is one comparison of two unsigned integers.
 */
static int compare(u32 left, u32 right, int unordered) {
    if (is_nan(left) || is_nan(right)) {
        return unordered;
    }
    u32 left_magnitude = left & ~SIGN;
    u32 right_magnitude = right & ~SIGN;
    /* A negative zero equals a positive zero, which is the one place the sign is not read. */
    if (left_magnitude == 0 && right_magnitude == 0) {
        return 0;
    }
    int left_negative = (left & SIGN) != 0;
    int right_negative = (right & SIGN) != 0;
    if (left_negative != right_negative) {
        return left_negative ? -1 : 1;
    }
    if (left_magnitude == right_magnitude) {
        return 0;
    }
    int ordered = left_magnitude > right_magnitude ? 1 : -1;
    /* Both negative, so the larger magnitude is the smaller number. */
    return left_negative ? -ordered : ordered;
}

/* The three way comparison. Minus one, zero or one, and one for a not a number as well, which the
 * documentation says not to rely on and which is here because the compiler emits this routine only
 * where it has already ruled a not a number out.
 */
int __cmpsf2(float left, float right) {
    return compare(pattern_of(left), pattern_of(right), 1);
}

/* Zero when the two are equal, and anything else when they are not, so the caller tests against
 * zero. A not a number is unequal to everything including itself, so the answer there is not zero.
 */
int __eqsf2(float left, float right) {
    return compare(pattern_of(left), pattern_of(right), 1);
}

/* The same work as the one above it, since a caller testing for inequality tests the same answer
 * against zero the other way round. libgcc has both names because a compiler emits both, and they
 * are two functions here rather than one alias because an alias is a linker feature and this file is
 * meant to compile with nothing underneath it.
 */
int __nesf2(float left, float right) {
    return compare(pattern_of(left), pattern_of(right), 1);
}

/* At or above zero when the left one is greater or equal, so a not a number has to come back below
 * zero for the test to fail.
 */
int __gesf2(float left, float right) {
    return compare(pattern_of(left), pattern_of(right), -1);
}

/* Above zero when the left one is greater, so a not a number comes back at or below zero. */
int __gtsf2(float left, float right) {
    return compare(pattern_of(left), pattern_of(right), -1);
}

/* At or below zero when the left one is less or equal, so a not a number comes back above zero. */
int __lesf2(float left, float right) {
    return compare(pattern_of(left), pattern_of(right), 1);
}

/* Below zero when the left one is less, so a not a number comes back at or above zero. */
int __ltsf2(float left, float right) {
    return compare(pattern_of(left), pattern_of(right), 1);
}

/* Not zero when the two cannot be ordered, which is when either of them is a not a number. This is
 * the one the others are defined in terms of: each of them is its own comparison and this answer.
 */
int __unordsf2(float left, float right) {
    return is_nan(pattern_of(left)) || is_nan(pattern_of(right));
}

/* The conversions between a float and an integer.
 *
 * These are in this file rather than in convert.c, which is also full of conversions, because
 * convert.c works at 128 bits by handing sixty four of them to the machine's own conversion
 * instruction: it exists for a target that has a floating point unit and no 128-bit integer. A
 * target with no floating point unit has the opposite problem, so these are done in integers the
 * way everything else here is, and they share this file's rounding.
 *
 * What C leaves undefined is a float the integer type cannot hold, an infinity, a not a number, and
 * a negative value handed to an unsigned conversion. All four answer zero, on both sides. That is
 * the convention section 12.8 already records for the conversions at 128 bits, where it is written
 * down as an accident of the shape rather than a promise. It has to be shared to be comparable.
 */

/* How many bits an integer needs, which is one more than the power of two its highest bit is worth.
 * A loop for the same reason `normalize` above is a loop.
 */
static int width_of(u64 value) {
    int bits = 0;
    while (value != 0) {
        value >>= 1;
        bits += 1;
    }
    return bits;
}

/* The magnitude of a signed value, taken through an unsigned type on purpose: the most negative
 * value of a type has no positive counterpart in it, and negating it there is exactly the case a
 * conversion has to get right rather than the case it may overflow on.
 */
static u64 magnitude_of(long long value) {
    return value < 0 ? (u64)0 - (u64)value : (u64)value;
}

/* The float nearest an integer, with the sign handed in separately because the magnitude came
 * through the routine above.
 *
 * Nothing here is special: the integer is already the significand, and what the routine does is
 * move its highest bit to where `round_and_pack` wants it, keeping what falls off the bottom in the
 * sticky bit, which is what makes the rounding of a value with more than twenty four significant
 * bits the right one rather than a truncation.
 */
static float float_of_integer(u32 sign, u64 magnitude) {
    if (magnitude == 0) {
        return float_of(sign);
    }
    int top = width_of(magnitude) - 1;
    u64 significand;
    if (top <= LEADING) {
        significand = magnitude << (LEADING - top);
    } else {
        significand = shift_down(magnitude, top - LEADING);
    }
    return round_and_pack(sign, top + BIAS, significand);
}

float __floatsisf(int value) {
    return float_of_integer(value < 0 ? SIGN : 0, magnitude_of(value));
}

float __floatunsisf(unsigned int value) {
    return float_of_integer(0, value);
}

float __floatdisf(long long value) {
    return float_of_integer(value < 0 ? SIGN : 0, magnitude_of(value));
}

float __floatundisf(unsigned long long value) {
    return float_of_integer(0, value);
}

/* The part of a float that is on the integer side of the point, with the sign and the magnitude
 * handed back separately so that each caller can hold the answer to the bounds of its own type.
 *
 * Zero rather than a refusal where the whole value is below one, since truncating 0.5 to an integer
 * is an answer and not an overflow. Zero and a refusal where there is no answer at all, which is an
 * infinity, a not a number, and a magnitude past what any of the four types hold.
 */
static int integer_of_float(float value, u32 *sign, u64 *magnitude) {
    u32 pattern = pattern_of(value);
    *sign = pattern & SIGN;
    *magnitude = 0;
    if (exponent_of(pattern) == TOP) {
        return 0;
    }
    u32 significand;
    int stored;
    unpack(pattern, &significand, &stored);
    int exponent = stored - BIAS;
    if (exponent < 0) {
        return 1;
    }
    if (exponent > 63) {
        return 0;
    }
    if (exponent >= FRACTION) {
        *magnitude = (u64)significand << (exponent - FRACTION);
    } else {
        *magnitude = (u64)significand >> (FRACTION - exponent);
    }
    return 1;
}

/* The largest magnitude each of the four types holds. The signed ones hold one more going down than
 * going up, which is the whole reason the magnitude and the sign travel separately above.
 */
#define SIGNED_32 2147483648ull
#define SIGNED_64 9223372036854775808ull
#define UNSIGNED_32 4294967295ull
#define UNSIGNED_64 18446744073709551615ull

int __fixsfsi(float value) {
    u32 sign;
    u64 magnitude;
    if (!integer_of_float(value, &sign, &magnitude)) {
        return 0;
    }
    if (sign != 0 && magnitude != 0) {
        if (magnitude > SIGNED_32) {
            return 0;
        }
        /* One less than the magnitude, negated, and one more taken off, because the most negative
         * value is not the negation of anything an `int` holds and writing it that way would be the
         * overflow this is here to avoid.
         */
        return -(int)(magnitude - 1) - 1;
    }
    if (magnitude > SIGNED_32 - 1) {
        return 0;
    }
    return (int)magnitude;
}

unsigned int __fixunssfsi(float value) {
    u32 sign;
    u64 magnitude;
    if (!integer_of_float(value, &sign, &magnitude)) {
        return 0;
    }
    /* A negative value is undefined here, and a negative one that truncates to zero is not: the
     * answer for that one is zero and it is the value rather than the convention.
     */
    if (sign != 0 && magnitude != 0) {
        return 0;
    }
    if (magnitude > UNSIGNED_32) {
        return 0;
    }
    return (unsigned int)magnitude;
}

long long __fixsfdi(float value) {
    u32 sign;
    u64 magnitude;
    if (!integer_of_float(value, &sign, &magnitude)) {
        return 0;
    }
    if (sign != 0 && magnitude != 0) {
        if (magnitude > SIGNED_64) {
            return 0;
        }
        return -(long long)(magnitude - 1) - 1;
    }
    if (magnitude > SIGNED_64 - 1) {
        return 0;
    }
    return (long long)magnitude;
}

unsigned long long __fixunssfdi(float value) {
    u32 sign;
    u64 magnitude;
    if (!integer_of_float(value, &sign, &magnitude)) {
        return 0;
    }
    if (sign != 0 && magnitude != 0) {
        return 0;
    }
    if (magnitude > UNSIGNED_64) {
        return 0;
    }
    return magnitude;
}

/* The two conversions between this format and the next one up, which are `__extendsfdf2` and
 * `__truncdfsf2`. They are the first routines in this directory that needed both formats to exist,
 * which is why they waited for double.c next door.
 *
 * They are here rather than there, and the reason is the same for both directions. A widening never
 * rounds, so all it needs of the wider format is where its fields are, and a narrowing is a rounding
 * into this format, so it wants this file's own `round_and_pack` and nothing of the wider format but
 * where its fields are again. So the pair needs the double's field widths and none of the double's
 * arithmetic, and a file that already knows how to round a float is the place for it.
 *
 * Why a widening never rounds is worth one sentence, because it is the whole of the easy direction:
 * every exponent a float holds is inside the double's range and fifty three bits hold twenty four, so
 * the fields move and nothing is lost. A float subnormal is the one case that is not a field move,
 * and it is not a rounding either: it is a normal double, since the double's range reaches far below
 * the smallest float.
 */

/* The wider format's fields, which is all of it this file needs. */
#define DOUBLE_FRACTION 52
#define DOUBLE_BIAS 1023
#define DOUBLE_TOP 2047
#define DOUBLE_SIGN 0x8000000000000000ull
#define DOUBLE_IMPLICIT 0x0010000000000000ull
#define DOUBLE_FRACTION_MASK 0x000fffffffffffffull

/* How far a fraction moves between the two formats, which is also how far a payload moves and is why
 * a not a number keeps its quiet bit without either direction saying anything about that bit: the
 * float's quiet bit is twenty two and the double's is fifty one, and the distance between the two
 * fractions is twenty nine.
 */
#define BETWEEN (DOUBLE_FRACTION - FRACTION)

union double_bits {
    double number;
    u64 pattern;
};

static u64 double_pattern_of(double value) {
    union double_bits at;
    at.number = value;
    return at.pattern;
}

static double double_of_pattern(u64 pattern) {
    union double_bits at;
    at.pattern = pattern;
    return at.number;
}

double __extendsfdf2(float value) {
    u32 pattern = pattern_of(value);
    u64 sign = (u64)(pattern & SIGN) << 32;
    int stored = exponent_of(pattern);
    u32 fraction = fraction_of(pattern);
    if (stored == TOP) {
        if (fraction == 0) {
            return double_of_pattern(sign | ((u64)DOUBLE_TOP << DOUBLE_FRACTION));
        }
        /* The payload moves up with the fraction, which carries the quiet bit along with it, and the
         * bit is set here as well for the one input it was not already set in, the way every other
         * routine in this file hands a not a number back quieted.
         */
        u64 payload = (u64)fraction << BETWEEN;
        return double_of_pattern(sign | ((u64)DOUBLE_TOP << DOUBLE_FRACTION) | payload
                                 | ((u64)QUIET << BETWEEN));
    }
    if (stored == 0) {
        if (fraction == 0) {
            return double_of_pattern(sign);
        }
        /* A float subnormal, which is a normal double. The loop brings the leading one up to where
         * the format implies it and takes the exponent down to match, which is the same work
         * `normalize` above does and is written out here because the exponent it moves is the wider
         * format's.
         */
        u32 significand = fraction;
        int exponent = 1 - BIAS + DOUBLE_BIAS;
        while ((significand & IMPLICIT) == 0) {
            significand <<= 1;
            exponent -= 1;
        }
        u64 result = sign | ((u64)exponent << DOUBLE_FRACTION);
        result |= ((u64)significand << BETWEEN) & DOUBLE_FRACTION_MASK;
        return double_of_pattern(result);
    }
    u64 result = sign | ((u64)(stored - BIAS + DOUBLE_BIAS) << DOUBLE_FRACTION);
    result |= (u64)fraction << BETWEEN;
    return double_of_pattern(result);
}

float __truncdfsf2(double value) {
    u64 pattern = double_pattern_of(value);
    u32 sign = (u32)(pattern >> 32) & SIGN;
    int stored = (int)((pattern >> DOUBLE_FRACTION) & DOUBLE_TOP);
    u64 fraction = pattern & DOUBLE_FRACTION_MASK;
    if (stored == DOUBLE_TOP) {
        if (fraction == 0) {
            return float_of(sign | ((u32)TOP << FRACTION));
        }
        /* The payload moves down, which loses the bottom twenty nine bits of it, so a not a number
         * whose payload was only in those bits would come back with an empty one. The quiet bit is
         * set afterwards, so what comes back is a not a number either way and never an infinity,
         * which is the thing that would be wrong rather than merely lossy.
         */
        return float_of(sign | ((u32)TOP << FRACTION) | (u32)(fraction >> BETWEEN) | QUIET);
    }
    if (stored == 0 && fraction == 0) {
        return float_of(sign);
    }
    /* A rounding, and the only one of the four cases that is. The significand arrives with its
     * leading one at bit fifty two and `round_and_pack` wants it at LEADING, so the shift down is
     * the distance between those two with the sticky bit keeping what falls off. The exponent is
     * rebiased into this format and may land anywhere, including far above the top and far below
     * zero, which is exactly the two departures `round_and_pack` already allows for: a value too
     * large becomes an infinity and one too small becomes a subnormal or a signed zero.
     */
    u64 significand = fraction;
    int exponent;
    if (stored == 0) {
        exponent = 1 - DOUBLE_BIAS + BIAS;
    } else {
        significand |= DOUBLE_IMPLICIT;
        exponent = stored - DOUBLE_BIAS + BIAS;
    }
    return round_and_pack(sign, exponent, shift_down(significand, DOUBLE_FRACTION - LEADING));
}
