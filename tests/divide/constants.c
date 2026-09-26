/* Division by a constant, run rather than read.
 *
 * Design: spec/optimizer/19-reassociation-and-arithmetic.md section 19.5. The pass this is about is
 * crates/rucc-codegen/src/divide.rs, which turns a division or a remainder by a constant into a
 * multiply by a magic number and some shifts. Its own tests run the rewrite it plans on numbers,
 * over every dividend at eight bits and every divisor at sixteen. This file is the other half: the
 * same divisions compiled by this compiler and by the system compiler, run, and the digests held
 * against each other, so what is checked is the code that came out rather than the plan for it.
 *
 * Every width a program divides at is here, with both signs, divisors whose magic number needs the
 * extra add and divisors whose number does not, powers of two and their negatives, and the largest
 * divisor each type holds. The eight and sixteen bit types go through every value they have. The
 * wider ones go through their edges and a random sample from a fixed seed. An exact division is a
 * pointer subtraction over a structure whose size is not a power of two, since that is where the
 * front end promises one.
 *
 * Nothing here divides by zero and nothing divides the most negative value by minus one. Both are
 * undefined in C, so a program that did either would be comparing two compilers' guesses.
 */

int printf(const char *format, ...);

/* How many rounds of random cases the wide types get, and how many cases in one. */
#define ROUNDS 4
#define PER_ROUND 32768

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

/* FNV-1a over the eight bytes of a value, low byte first. */
static unsigned long long mix(unsigned long long digest, unsigned long long value) {
    for (int i = 0; i < 8; i++) {
        digest ^= (unsigned char)(value >> (8 * i));
        digest *= 1099511628211ull;
    }
    return digest;
}

static void report(const char *what, int round, unsigned long long digest) {
    printf("%s %d %llu\n", what, round, digest);
}

/* The divisors. Seven and nineteen need the extra add at thirty two bits and a hundred does not, a
 * power of two is a shift with a bias when the division is signed, and the large ones are past what
 * a narrow dividend can hold, which is where the pass leaves the division alone and the answer
 * still has to be right.
 */
#define SMALL(X) \
    X(2) X(3) X(4) X(5) X(6) X(7) X(8) X(9) X(10) X(11) X(12) X(13) X(14) X(15) X(16) X(17) X(19) \
    X(25) X(31) X(32) X(60) X(64) X(100) X(125) X(127)
#define NEGATIVE_SMALL(X) X(-2) X(-3) X(-4) X(-7) X(-8) X(-10) X(-16) X(-100) X(-127)
#define MEDIUM(X) X(128) X(255) X(256) X(641) X(1000) X(3600) X(32767)
#define NEGATIVE_MEDIUM(X) X(-128) X(-1000) X(-32767) X(-32768)
#define LARGE(X) X(32768) X(65535) X(65536) X(65537) X(1000000) X(1000000007) X(2147483647)
#define NEGATIVE_LARGE(X) X(-65537) X(-1000000007) X(-2147483647) X(-2147483647 - 1)
#define UNSIGNED_LARGE(X) X(2147483648u) X(4294967291u) X(4294967295u)
#define WIDE(X) X(1ull << 32) X(1ull << 40) X(1ull << 63)
#define NEGATIVE_WIDE(X) X(-(1ll << 32)) X(-(1ll << 40)) X(-(1ll << 62))

/* One quotient and one remainder of `x`, into the two digests every group keeps. */
#define Q(D) quotients = mix(quotients, (unsigned long long)(x / (D)));
#define R(D) remainders = mix(remainders, (unsigned long long)(x % (D)));

/* The values at the ends of a thirty two bit integer and either side of them, which random numbers
 * almost never land on.
 */
static const unsigned edges[] = {
    0u, 1u, 2u, 3u, 7u, 100u, 0x7FFFFFFEu, 0x7FFFFFFFu, 0x80000000u, 0x80000001u, 0xFFFFFFF9u,
    0xFFFFFFFEu, 0xFFFFFFFFu,
};
#define EDGES (sizeof edges / sizeof edges[0])

/* A thirty two bit value for case `at` of a round: the edges first, then random ones. */
static unsigned value_at(int at) {
    return at < (int)EDGES ? edges[at] : (unsigned)next_random();
}

static void unsigned_chars(void) {
    unsigned long long quotients = 0, remainders = 0;
    for (int i = 0; i < 256; i++) {
        unsigned char x = (unsigned char)i;
        SMALL(Q) MEDIUM(Q) NEGATIVE_SMALL(Q)
        SMALL(R) MEDIUM(R) NEGATIVE_SMALL(R)
        cases++;
    }
    report("uchar-quotient", 0, quotients);
    report("uchar-remainder", 0, remainders);
}

static void signed_chars(void) {
    unsigned long long quotients = 0, remainders = 0;
    for (int i = -128; i < 128; i++) {
        signed char x = (signed char)i;
        SMALL(Q) MEDIUM(Q) NEGATIVE_SMALL(Q) NEGATIVE_MEDIUM(Q)
        SMALL(R) MEDIUM(R) NEGATIVE_SMALL(R) NEGATIVE_MEDIUM(R)
        cases++;
    }
    report("schar-quotient", 0, quotients);
    report("schar-remainder", 0, remainders);
}

static void unsigned_shorts(void) {
    unsigned long long quotients = 0, remainders = 0;
    for (int i = 0; i < 65536; i++) {
        unsigned short x = (unsigned short)i;
        SMALL(Q) MEDIUM(Q) LARGE(Q) NEGATIVE_SMALL(Q)
        SMALL(R) MEDIUM(R) LARGE(R) NEGATIVE_SMALL(R)
        cases++;
    }
    report("ushort-quotient", 0, quotients);
    report("ushort-remainder", 0, remainders);
}

static void signed_shorts(void) {
    unsigned long long quotients = 0, remainders = 0;
    for (int i = -32768; i < 32768; i++) {
        short x = (short)i;
        SMALL(Q) MEDIUM(Q) LARGE(Q) NEGATIVE_SMALL(Q) NEGATIVE_MEDIUM(Q)
        SMALL(R) MEDIUM(R) LARGE(R) NEGATIVE_SMALL(R) NEGATIVE_MEDIUM(R)
        cases++;
    }
    report("short-quotient", 0, quotients);
    report("short-remainder", 0, remainders);
}

static void unsigned_ints(int round) {
    unsigned long long quotients = 0, remainders = 0;
    for (int at = 0; at < PER_ROUND; at++) {
        unsigned x = value_at(at);
        SMALL(Q) MEDIUM(Q) LARGE(Q) UNSIGNED_LARGE(Q)
        SMALL(R) MEDIUM(R) LARGE(R) UNSIGNED_LARGE(R)
        cases++;
    }
    report("uint-quotient", round, quotients);
    report("uint-remainder", round, remainders);
}

static void signed_ints(int round) {
    unsigned long long quotients = 0, remainders = 0;
    for (int at = 0; at < PER_ROUND; at++) {
        int x = (int)value_at(at);
        SMALL(Q) MEDIUM(Q) LARGE(Q) NEGATIVE_SMALL(Q) NEGATIVE_MEDIUM(Q) NEGATIVE_LARGE(Q)
        SMALL(R) MEDIUM(R) LARGE(R) NEGATIVE_SMALL(R) NEGATIVE_MEDIUM(R) NEGATIVE_LARGE(R)
        cases++;
    }
    report("int-quotient", round, quotients);
    report("int-remainder", round, remainders);
}

/* A thirty two bit value widened to sixty four, which is a division the pass can do with the
 * thirty two bit number although it is at sixty four bits.
 */
static void widened_unsigned(int round) {
    unsigned long long quotients = 0, remainders = 0;
    for (int at = 0; at < PER_ROUND; at++) {
        unsigned long long x = value_at(at);
        SMALL(Q) MEDIUM(Q) LARGE(Q) UNSIGNED_LARGE(Q)
        SMALL(R) MEDIUM(R) LARGE(R) UNSIGNED_LARGE(R)
        cases++;
    }
    report("widened-unsigned-quotient", round, quotients);
    report("widened-unsigned-remainder", round, remainders);
}

static void widened_signed(int round) {
    unsigned long long quotients = 0, remainders = 0;
    for (int at = 0; at < PER_ROUND; at++) {
        long long x = (int)value_at(at);
        SMALL(Q) MEDIUM(Q) LARGE(Q) NEGATIVE_SMALL(Q) NEGATIVE_MEDIUM(Q) NEGATIVE_LARGE(Q)
        SMALL(R) MEDIUM(R) LARGE(R) NEGATIVE_SMALL(R) NEGATIVE_MEDIUM(R) NEGATIVE_LARGE(R)
        cases++;
    }
    report("widened-signed-quotient", round, quotients);
    report("widened-signed-remainder", round, remainders);
}

/* Sixty four bits that could be anything, where only a power of two is rewritten and the rest stay
 * a division until there is a high multiply, and both have to be right.
 */
static void wide_unsigned(int round) {
    unsigned long long quotients = 0, remainders = 0;
    for (int at = 0; at < PER_ROUND; at++) {
        unsigned long long x = next_random() >> (next_random() % 64);
        SMALL(Q) WIDE(Q)
        SMALL(R) WIDE(R)
        cases++;
    }
    report("wide-unsigned-quotient", round, quotients);
    report("wide-unsigned-remainder", round, remainders);
}

static void wide_signed(int round) {
    unsigned long long quotients = 0, remainders = 0;
    for (int at = 0; at < PER_ROUND; at++) {
        long long x = (long long)next_random() >> (next_random() % 64);
        SMALL(Q) NEGATIVE_SMALL(Q) WIDE(Q) NEGATIVE_WIDE(Q)
        SMALL(R) NEGATIVE_SMALL(R) WIDE(R) NEGATIVE_WIDE(R)
        cases++;
    }
    report("wide-signed-quotient", round, quotients);
    report("wide-signed-remainder", round, remainders);
}

/* Structures whose sizes are not powers of two, so a pointer subtraction over an array of one is an
 * exact division by that size.
 */
struct twelve { char bytes[12]; };
struct twenty_four { char bytes[24]; };
struct forty { char bytes[40]; };
struct seven { char bytes[7]; };

#define SLOTS 1000

static struct twelve twelves[SLOTS];
static struct twenty_four twenty_fours[SLOTS];
static struct forty forties[SLOTS];
static struct seven sevens[SLOTS];

static void pointer_differences(int round) {
    unsigned long long digest = 0;
    for (int at = 0; at < PER_ROUND; at++) {
        int i = (int)(next_random() % SLOTS);
        int j = (int)(next_random() % SLOTS);
        digest = mix(digest, (unsigned long long)(&twelves[i] - &twelves[j]));
        digest = mix(digest, (unsigned long long)(&twenty_fours[i] - &twenty_fours[j]));
        digest = mix(digest, (unsigned long long)(&forties[i] - &forties[j]));
        digest = mix(digest, (unsigned long long)(&sevens[i] - &sevens[j]));
        cases++;
    }
    report("pointer-difference", round, digest);
}

int main(void) {
    unsigned_chars();
    signed_chars();
    unsigned_shorts();
    signed_shorts();
    for (int round = 0; round < ROUNDS; round++) {
        unsigned_ints(round);
        signed_ints(round);
        widened_unsigned(round);
        widened_signed(round);
        wide_unsigned(round);
        wide_signed(round);
        pointer_differences(round);
    }
    printf("cases %ld\n", cases);
    return 0;
}
