/* Arithmetic on a 128-bit integer, run rather than read.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8. The pass this is about is
 * crates/rucc-codegen/src/wide.rs, which turns every value of that width into the two registers the
 * convention holds it in and every operation over one into operations over the halves. Its own tests
 * read the IR it produced, which is the same kind of evidence as a stub writer reading its own bytes
 * back, so what this file is for is the other kind: the same program compiled by this compiler and
 * by the system compiler, run, and the two outputs held against each other.
 *
 * Every group prints a digest rather than the values, because the output is read by a task
 * comparing two runs and not by a person. A group is an operation and a round, so a difference names
 * the operation rather than only saying that something somewhere came out different.
 *
 * The inputs are a seed and an xorshift rather than the C library's rand, so the two runs see the
 * same numbers whoever compiled them and whatever library they were linked against. The widths are
 * random as well as the bits: both compilers have a path for a value that fits in sixty four bits
 * and a path for one that does not, and a stream of full width values would only ever ask about one
 * of them.
 *
 * Nothing here divides by zero and nothing divides the most negative value by minus one. Both are
 * undefined in C, so a program that did either would be comparing two compilers' guesses. The
 * conversions to and from a float keep the same rule: a value with no float to become and a float
 * whose integer part the type cannot hold are both undefined, so the values are built to stay inside
 * the range rather than tested there and excluded afterwards.
 */

typedef unsigned __int128 uwide;
typedef __int128 wide;

/* Declared rather than included, so that both sides of the comparison are reading the same
 * declaration and neither is reading a header this compiler ships and the other one does not.
 */
int printf(const char *format, ...);

/* A float and its bits are the same bytes read two ways, and this is how a program says so without
 * asking what either compiler does about reading a union back the other way round. It is also how
 * the digest gets at a float at all, since what a conversion rounded to is a question about bits.
 */
void *memcpy(void *to, const void *from, unsigned long size);

/* How many rounds of random cases, and how many cases in one. The product is what the pairs number
 * in the output is, and the split into rounds is so that a difference says which round it started
 * at, which is what makes a run reproducible from the seed.
 */
#define ROUNDS 8
#define CASES 1024

/* The bit positions a corner value is built around: the ends of the type, the boundary between the
 * halves this compiler splits at, and the digit boundaries of a long division in base 2^32.
 */
static const int CORNER_SHIFTS[] = {0, 1, 31, 32, 33, 63, 64, 65, 95, 96, 97, 126, 127};

#define CORNER_SHIFT_COUNT ((int)(sizeof CORNER_SHIFTS / sizeof CORNER_SHIFTS[0]))

/* Three values per position and two that belong to no position. */
#define CORNERS (3 * CORNER_SHIFT_COUNT + 2)

static uwide corners[CORNERS];

/* How many cases ran, printed at the end so that a run that stopped early is a different thing from
 * a run that disagreed.
 */
static long cases = 0;

/* xorshift64, seeded away from the one state it cannot leave. */
static unsigned long long state = 0x9E3779B97F4A7C15ull;

static unsigned long long next_random(void) {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    return state;
}

/* FNV-1a over the sixteen bytes of a value, low byte first. */
static unsigned long long mix(unsigned long long digest, uwide value) {
    for (int i = 0; i < 16; i++) {
        digest ^= (unsigned char)(value >> (8 * i));
        digest *= 1099511628211ull;
    }
    return digest;
}

/* The first value of each integer type that the type cannot hold, both of them exact as a double.
 * They are the bounds a conversion coming down is kept inside, since a float above them has no
 * integer to truncate to.
 */
#define TWO_TO_127 170141183460469231731687303715884105728.0
#define TWO_TO_128 340282366920938463463374607431768211456.0

static unsigned long long bits_of_double(double value) {
    unsigned long long bits;
    memcpy(&bits, &value, sizeof bits);
    return bits;
}

static unsigned long long bits_of_float(float value) {
    unsigned int bits;
    memcpy(&bits, &value, sizeof bits);
    return (unsigned long long)bits;
}

static double double_of_bits(unsigned long long bits) {
    double value;
    memcpy(&value, &bits, sizeof value);
    return value;
}

/* A random double whose magnitude is at least 2^-8 and below 2^top, negative half the time when
 * signs is set.
 *
 * The bits are built rather than a value converted, so that what goes into a conversion did not come
 * out of one. The low end is below one on purpose: a float that truncates to nothing is the answer a
 * conversion is most likely to get wrong in a way no other case notices.
 */
static double random_double(int top, int signs) {
    unsigned long long bits = next_random();
    unsigned long long exponent = 1015 + next_random() % (unsigned long long)(top + 8);
    bits = (bits & 0xFFFFFFFFFFFFFull) | (exponent << 52);
    if (signs && (next_random() & 1)) {
        bits |= (unsigned long long)1 << 63;
    }
    return double_of_bits(bits);
}

/* A random value of a random width, from nothing at all up to the whole type. */
static uwide random_wide(void) {
    int width = (int)(next_random() % 129);
    uwide value = ((uwide)next_random() << 64) | next_random();
    if (width == 0) {
        return 0;
    }
    if (width < 128) {
        value >>= 128 - width;
    }
    return value;
}

/* The corner table: one at each position, one below it and one above it, and then nothing and
 * everything.
 */
static void make_corners(void) {
    int at = 0;
    for (int i = 0; i < CORNER_SHIFT_COUNT; i++) {
        uwide one = (uwide)1 << CORNER_SHIFTS[i];
        corners[at++] = one;
        corners[at++] = one - 1;
        corners[at++] = one + 1;
    }
    corners[at++] = 0;
    corners[at++] = ~(uwide)0;
}

static void report(const char *what, int round, unsigned long long digest) {
    printf("%s %d %llu\n", what, round, digest);
}

/* Adding, subtracting, multiplying and the three bitwise operations, which are the operations the
 * pass rewrites into the same operation over each half with whatever crossed put back.
 *
 * Unsigned throughout. Signed overflow is undefined and the low hundred and twenty eight bits of a
 * sum or a product are the same bits whichever way the operands are read, so the signed version of
 * these would be the same arithmetic with a reason to be careful about it.
 */
static void arithmetic(int round) {
    unsigned long long sums = 0, differences = 0, products = 0, bits = 0;
    for (int i = 0; i < CASES; i++) {
        uwide a = random_wide();
        uwide b = random_wide();
        sums = mix(sums, a + b);
        differences = mix(differences, a - b);
        products = mix(products, a * b);
        bits = mix(bits, (a & b) ^ (a | b) ^ ~a);
        cases += 4;
    }
    report("add", round, sums);
    report("sub", round, differences);
    report("mul", round, products);
    report("bitwise", round, bits);
}

/* The three shifts, at a count from nothing to one less than the width.
 *
 * The count is where this gets interesting rather than the value: a count below sixty four moves
 * both halves and a count of sixty four or more empties one of them, and the two are different code.
 * A count of the width or more is undefined and is not asked about.
 */
static void shifts(int round) {
    unsigned long long up = 0, down = 0, signed_down = 0;
    for (int i = 0; i < CASES; i++) {
        uwide a = random_wide();
        int count = (int)(next_random() % 128);
        up = mix(up, a << count);
        down = mix(down, a >> count);
        signed_down = mix(signed_down, (uwide)((wide)a >> count));
        cases += 3;
    }
    report("shl", round, up);
    report("lshr", round, down);
    report("ashr", round, signed_down);
}

/* The twelve comparisons, packed a bit each into one number per pair.
 *
 * Packed rather than digested one at a time because what a comparison at this width gets wrong is a
 * relation between the halves, and a pair that one predicate is wrong about is usually a pair
 * several of them are wrong about. One line per round says that much and the seed says which pair.
 */
static void comparisons(int round) {
    unsigned long long answers = 0;
    for (int i = 0; i < CASES; i++) {
        uwide a = random_wide();
        uwide b = random_wide();
        wide sa = (wide)a;
        wide sb = (wide)b;
        unsigned long long packed = 0;
        packed |= (unsigned long long)(a == b) << 0;
        packed |= (unsigned long long)(a != b) << 1;
        packed |= (unsigned long long)(a < b) << 2;
        packed |= (unsigned long long)(a <= b) << 3;
        packed |= (unsigned long long)(a > b) << 4;
        packed |= (unsigned long long)(a >= b) << 5;
        packed |= (unsigned long long)(sa == sb) << 6;
        packed |= (unsigned long long)(sa != sb) << 7;
        packed |= (unsigned long long)(sa < sb) << 8;
        packed |= (unsigned long long)(sa <= sb) << 9;
        packed |= (unsigned long long)(sa > sb) << 10;
        packed |= (unsigned long long)(sa >= sb) << 11;
        answers = mix(answers, (uwide)packed);
        cases += 12;
    }
    report("compare", round, answers);
}

/* The four divisions, which are the operations that are a call into the compiler runtime rather
 * than arithmetic over the halves.
 *
 * A zero divisor is replaced by one rather than skipped, so that the two runs get through the same
 * number of cases whatever the seed. The most negative value over minus one is the one pair that is
 * skipped, because it is the one signed division whose answer the type has no room for.
 */
static void divisions(int round) {
    unsigned long long quotients = 0, remainders = 0;
    unsigned long long signed_quotients = 0, signed_remainders = 0;
    for (int i = 0; i < CASES; i++) {
        uwide a = random_wide();
        uwide b = random_wide();
        if (b == 0) {
            b = 1;
        }
        quotients = mix(quotients, a / b);
        remainders = mix(remainders, a % b);
        wide sa = (wide)a;
        wide sb = (wide)b;
        cases += 2;
        if (sa == (wide)((uwide)1 << 127) && sb == -1) {
            continue;
        }
        signed_quotients = mix(signed_quotients, (uwide)(sa / sb));
        signed_remainders = mix(signed_remainders, (uwide)(sa % sb));
        cases += 2;
    }
    report("udiv", round, quotients);
    report("umod", round, remainders);
    report("sdiv", round, signed_quotients);
    report("smod", round, signed_remainders);
}

/* Dividing by a constant, which is a path of its own in both compilers.
 *
 * A divisor the compiler can see is a division the optimizer is allowed to turn into something else,
 * and at this width what it turns into is either a call anyway or arithmetic nobody checked. Three
 * of them: a small one, one that is a power of two, and one that is a power of two above the
 * boundary between the halves.
 */
static void constants(int round) {
    unsigned long long small = 0, power = 0, high = 0;
    for (int i = 0; i < CASES; i++) {
        uwide a = random_wide();
        small = mix(small, a / 10);
        small = mix(small, a % 10);
        power = mix(power, a / 256);
        power = mix(power, a % 256);
        high = mix(high, a / ((uwide)1 << 70));
        high = mix(high, a % ((uwide)1 << 70));
        cases += 6;
    }
    report("const-small", round, small);
    report("const-power", round, power);
    report("const-high", round, high);
}

/* The conversions to and from a float, which are the other operation at this width that is a call
 * into the compiler runtime rather than arithmetic over the halves.
 *
 * Four names go up and four come down, for a signed and an unsigned integer against each of the two
 * formats. The digest is over the bits of the float rather than over the float, because what a
 * conversion going up has to get right is which way it rounded and that is one bit of the answer.
 *
 * A value within a rounding step of 2^128 has no single precision float to become, so the single
 * precision cases take the value with its top bit gone, whose largest float is 2^127 exactly. Coming
 * down the bound is on the exponent, one for the signed type and a wider one for the unsigned, and a
 * little lower again where the double is narrowed to a float first, since that narrowing is allowed
 * to round upwards into the value the type cannot hold.
 */
static void conversions(int round) {
    unsigned long long up = 0, up_signed = 0, down = 0, down_signed = 0;
    for (int i = 0; i < CASES; i++) {
        uwide a = random_wide();
        uwide narrow = a >> 1;
        up = mix(up, (uwide)bits_of_double((double)a));
        up = mix(up, (uwide)bits_of_float((float)narrow));
        up_signed = mix(up_signed, (uwide)bits_of_double((double)(wide)a));
        up_signed = mix(up_signed, (uwide)bits_of_float((float)-(wide)narrow));
        cases += 4;

        double big = random_double(128, 0);
        double small = random_double(127, 1);
        down = mix(down, (uwide)big);
        down = mix(down, (uwide)(float)random_double(127, 0));
        down_signed = mix(down_signed, (uwide)(wide)small);
        down_signed = mix(down_signed, (uwide)(wide)(float)random_double(126, 1));
        cases += 4;
    }
    report("convert-up", round, up);
    report("convert-up-signed", round, up_signed);
    report("convert-down", round, down);
    report("convert-down-signed", round, down_signed);
}

/* Every corner value up to a float, and the floats they became back down again.
 *
 * Going up, a corner is where the rounding has to decide something: a value one below a power of two
 * has more significant bits than either format holds and the lowest of them is what decides the
 * answer. Coming down, the float a corner became is a power of two or one step away from one, which
 * is where a conversion that shifted by the wrong amount is still right about half the table.
 */
static void corner_conversions(void) {
    unsigned long long up = 0, down = 0;
    for (int i = 0; i < CORNERS; i++) {
        uwide a = corners[i];
        uwide narrow = a >> 1;
        double whole = (double)a;
        double halved = (double)narrow;
        up = mix(up, (uwide)bits_of_double(whole));
        up = mix(up, (uwide)bits_of_float((float)narrow));
        up = mix(up, (uwide)bits_of_double((double)(wide)a));
        up = mix(up, (uwide)bits_of_float((float)-(wide)narrow));
        cases += 4;
        /* Back down only where there is an answer. The double a value at the top of the type became
         * can be 2^128 exactly, and the double the half of it became can be 2^127, and neither of
         * those is a number its own type holds.
         */
        if (whole < TWO_TO_128) {
            down = mix(down, (uwide)whole);
            cases += 1;
        }
        if (halved < TWO_TO_127) {
            down = mix(down, (uwide)(wide)halved);
            down = mix(down, (uwide)(wide)-halved);
            cases += 2;
        }
    }
    report("corner-convert-up", 0, up);
    report("corner-convert-down", 0, down);
}

/* Every pair of corner values through every operation.
 *
 * The pairs are where the answers are decided by something other than the bits: equal high halves,
 * a divisor one above the dividend, a shift count that lands exactly on the boundary. A random pair
 * of full width values is almost never any of those.
 */
static void corner_pairs(void) {
    unsigned long long arithmetic_digest = 0, shift_digest = 0;
    unsigned long long compare_digest = 0, division_digest = 0;
    for (int i = 0; i < CORNERS; i++) {
        for (int j = 0; j < CORNERS; j++) {
            uwide a = corners[i];
            uwide b = corners[j];
            wide sa = (wide)a;
            wide sb = (wide)b;
            arithmetic_digest = mix(arithmetic_digest, a + b);
            arithmetic_digest = mix(arithmetic_digest, a - b);
            arithmetic_digest = mix(arithmetic_digest, a * b);
            int count = CORNER_SHIFTS[j % CORNER_SHIFT_COUNT];
            shift_digest = mix(shift_digest, a << count);
            shift_digest = mix(shift_digest, a >> count);
            shift_digest = mix(shift_digest, (uwide)(sa >> count));
            compare_digest = mix(compare_digest, (uwide)(unsigned long long)(a < b));
            compare_digest = mix(compare_digest, (uwide)(unsigned long long)(a >= b));
            compare_digest = mix(compare_digest, (uwide)(unsigned long long)(sa < sb));
            compare_digest = mix(compare_digest, (uwide)(unsigned long long)(sa >= sb));
            cases += 10;
            if (b == 0) {
                continue;
            }
            division_digest = mix(division_digest, a / b);
            division_digest = mix(division_digest, a % b);
            cases += 2;
            if (sa == (wide)((uwide)1 << 127) && sb == -1) {
                continue;
            }
            division_digest = mix(division_digest, (uwide)(sa / sb));
            division_digest = mix(division_digest, (uwide)(sa % sb));
            cases += 2;
        }
    }
    report("corner-arithmetic", 0, arithmetic_digest);
    report("corner-shift", 0, shift_digest);
    report("corner-compare", 0, compare_digest);
    report("corner-division", 0, division_digest);
}

int main(void) {
    make_corners();
    corner_pairs();
    corner_conversions();
    for (int round = 0; round < ROUNDS; round++) {
        arithmetic(round);
        shifts(round);
        comparisons(round);
        divisions(round);
        constants(round);
        conversions(round);
    }
    printf("cases %ld\n", cases);
    return 0;
}
