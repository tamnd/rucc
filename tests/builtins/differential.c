/* The runtime support routines, run over the same cases as whatever is linked beside this file.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8, which asks for the library to be held against a
 * reference over randomized inputs rather than against numbers written down somewhere. The program
 * that does that is this one, linked twice: once against the archive rucc wrote out of
 * runtime/builtins, and once against the same routines in Rust from runtime/rucc-builtins. Each
 * run prints a digest per group of cases and the task compares the two outputs line by line, so a
 * difference names the routine and the length it showed up at.
 *
 * Nothing here knows which side it was linked against, which is the property that makes the
 * comparison worth anything. It is compiled by the system compiler rather than by rucc, with
 * -fno-builtin so the calls are calls, because a harness compiled by the compiler under test can
 * agree with it about the wrong answer.
 *
 * The inputs come from a seed and the same seed is used twice, so the two runs see identical bytes
 * in identical buffers. The seed is argv[1] when there is one, which is what lets a failure be run
 * again and a sweep be run later without changing this file.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef unsigned __int128 uwide;
typedef __int128 wide;

/* The two entry points ordinary C does not reach. A / or a % on a 128-bit value becomes a call to
 * one of the other four, and these two hand back the quotient and the remainder together, which is
 * what a compiler emits when it wants both and what libgcc's own __udivti3 is written over. Nothing
 * in this file would call them unless it says their names, so it says their names.
 */
unsigned __int128 __udivmodti4(unsigned __int128 top, unsigned __int128 bottom,
                               unsigned __int128 *rest);
__int128 __divmodti4(__int128 top, __int128 bottom, __int128 *rest);

/* And all six at sixty four bits, every one of them by name.
 *
 * A / on a long long is a call only on a target whose registers are narrower than that, and this
 * harness runs on the host. So none of these six can be reached by writing arithmetic here, and the
 * machine's own instruction is not a stand-in for them: what is under test is the routine a 32-bit
 * target will call, and a program that divided with the instruction would be a test of the host.
 */
unsigned long long __udivdi3(unsigned long long top, unsigned long long bottom);
unsigned long long __umoddi3(unsigned long long top, unsigned long long bottom);
unsigned long long __udivmoddi4(unsigned long long top, unsigned long long bottom,
                                unsigned long long *rest);
long long __divdi3(long long top, long long bottom);
long long __moddi3(long long top, long long bottom);
long long __divmoddi4(long long top, long long bottom, long long *rest);

/* And the single precision soft float routines, by name for the same reason.
 *
 * Writing a + b here would compile to the machine's own instruction, which is not what is under
 * test: what is under test is what a target with no floating point unit calls instead. The host has
 * a floating point unit, so the only way to reach these is to say their names.
 */
float __addsf3(float left, float right);
float __subsf3(float left, float right);
float __mulsf3(float left, float right);
float __divsf3(float left, float right);
float __negsf2(float value);

/* And the eight comparisons, which are calls on such a target for the same reason `a < b` is not an
 * instruction there. Only the sign of what they hand back is specified, so only the sign is
 * digested, the way the sign of `memcmp` is and for the same reason: pinning the magnitude would
 * make this a test of an accident rather than of the contract.
 */
int __cmpsf2(float left, float right);
int __eqsf2(float left, float right);
int __nesf2(float left, float right);
int __gesf2(float left, float right);
int __gtsf2(float left, float right);
int __lesf2(float left, float right);
int __ltsf2(float left, float right);
int __unordsf2(float left, float right);

/* Every offset in a word and one past it, so the head, the word and the tail of an implementation
 * that works a word at a time each get to be the only part that runs and each get to run beside
 * the others. This one is byte at a time today and the cases outlive that.
 */
#define PHASES 9

/* How much canary sits either side of a destination, which is what catches a write that went one
 * byte too far in either direction.
 */
#define PAD 16

/* The longest case, and the buffers are this plus room for the phase and the padding. */
#define LONGEST 1000
#define ROOM (LONGEST + PHASES + 2 * PAD + 8)

/* The lengths each routine is run at: every one up to 80, because that is where an implementation
 * switches between its loops, and then the handful of larger ones where a length near a power of
 * two is the interesting thing.
 */
static const int LENGTHS[] = {
    0,  1,  2,  3,  4,  5,  6,  7,  8,  9,  10, 11, 12, 13,  14,  15,  16,  17,  18,  19,
    20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33,  34,  35,  36,  37,  38,  39,
    40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53,  54,  55,  56,  57,  58,  59,
    60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73,  74,  75,  76,  77,  78,  79,
    80, 96, 127, 128, 129, 255, 256, 257, 511, 512, 513, 999, 1000,
};

#define LENGTHS_COUNT ((int)(sizeof LENGTHS / sizeof LENGTHS[0]))

/* The byte the padding is filled with, which is not a byte any pattern produces. */
#define CANARY 0xAA

static unsigned char source[ROOM];
static unsigned char dest[ROOM];
static unsigned char other[ROOM];

/* How many cases ran, which is printed at the end so that a run that stopped early is a different
 * thing from a run that disagreed.
 */
static long cases = 0;

/* xorshift64, because the two runs have to see the same bytes and a C library's rand differs
 * between one library and the next. Seeded away from zero, which is the one state this cannot
 * leave.
 */
static unsigned long long state = 0x9E3779B97F4A7C15ull;

static unsigned long long next_random(void) {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    return state;
}

/* FNV-1a, over whatever a case wants to say about itself. A digest rather than the bytes, because
 * the output of a run is read by a task that is comparing it against another run and not by a
 * person reading buffers.
 */
static unsigned long long mix(unsigned long long digest, const unsigned char *at, size_t n) {
    for (size_t i = 0; i < n; i++) {
        digest ^= at[i];
        digest *= 1099511628211ull;
    }
    return digest;
}

static unsigned long long mix_number(unsigned long long digest, long long value) {
    unsigned char bytes[8];
    for (int i = 0; i < 8; i++) {
        bytes[i] = (unsigned char)((unsigned long long)value >> (8 * i));
    }
    return mix(digest, bytes, sizeof bytes);
}

static unsigned long long mix_wide(unsigned long long digest, uwide value) {
    unsigned char bytes[16];
    for (int i = 0; i < 16; i++) {
        bytes[i] = (unsigned char)(value >> (8 * i));
    }
    return mix(digest, bytes, sizeof bytes);
}

/* Bytes that differ at every position, so a copy that repeats or drops one is visible rather than
 * lucky, and randomized on top of that so a run is not only the pattern this file happened to
 * pick.
 */
static void fill_random(unsigned char *at, size_t n) {
    for (size_t i = 0; i < n; i++) {
        at[i] = (unsigned char)(next_random() >> 24);
    }
}

static void fill_canary(unsigned char *at, size_t n) {
    for (size_t i = 0; i < n; i++) {
        at[i] = CANARY;
    }
}

/* One group of cases, named and digested. */
static void say(const char *what, int length, unsigned long long digest) {
    printf("%s %d %016llx\n", what, length, digest);
}

/* memcpy at every pair of alignments, with the whole destination buffer in the digest rather than
 * only the bytes that were asked for, which is what makes a write outside the range a difference
 * rather than something nobody looked at.
 */
static void copies(int length) {
    unsigned long long digest = 14695981039346656037ull;
    for (int from = 0; from < PHASES; from++) {
        for (int to = 0; to < PHASES; to++) {
            fill_random(source, (size_t)ROOM);
            fill_canary(dest, (size_t)ROOM);
            void *back = memcpy(dest + PAD + to, source + from, (size_t)length);
            digest = mix(digest, dest, (size_t)ROOM);
            digest = mix_number(digest, back == dest + PAD + to);
            cases++;
        }
    }
    say("memcpy", length, digest);
}

/* memmove, both where the ranges do not touch and where they overlap in each direction. The
 * overlapping cases are the whole reason this routine is not memcpy, and the direction a correct
 * one picks is decided by which of the two pointers is the greater.
 */
static void moves(int length) {
    unsigned long long digest = 14695981039346656037ull;
    for (int from = 0; from < PHASES; from++) {
        for (int to = 0; to < PHASES; to++) {
            fill_random(source, (size_t)ROOM);
            fill_canary(dest, (size_t)ROOM);
            void *back = memmove(dest + PAD + to, source + from, (size_t)length);
            digest = mix(digest, dest, (size_t)ROOM);
            digest = mix_number(digest, back == dest + PAD + to);
            cases++;
        }
    }
    for (int shift = -PHASES + 1; shift < PHASES; shift++) {
        fill_random(other, (size_t)ROOM);
        unsigned char *at = other + PAD + PHASES;
        void *back = memmove(at + shift, at, (size_t)length);
        digest = mix(digest, other, (size_t)ROOM);
        digest = mix_number(digest, back == at + shift);
        cases++;
    }
    say("memmove", length, digest);
}

/* memset, with the value taken from the whole range of an int rather than from a byte, because the
 * standard says the value is an int and only its low byte is stored and a parameter written as a
 * char is how that gets got wrong.
 */
static void fills(int length) {
    unsigned long long digest = 14695981039346656037ull;
    for (int to = 0; to < PHASES; to++) {
        for (int which = 0; which < 4; which++) {
            int value = (int)(next_random() >> 32);
            /* The two ends of the byte and one value that is nothing but high bits, so a narrowing
             * that went wrong shows up rather than being hidden by a random low byte.
             */
            if (which == 1) {
                value = 0;
            } else if (which == 2) {
                value = 0xFF;
            } else if (which == 3) {
                value = -256;
            }
            fill_canary(dest, (size_t)ROOM);
            void *back = memset(dest + PAD + to, value, (size_t)length);
            digest = mix(digest, dest, (size_t)ROOM);
            digest = mix_number(digest, back == dest + PAD + to);
            cases++;
        }
    }
    say("memset", length, digest);
}

/* memcmp, over equal runs and over runs that differ at one place. Only the sign of the answer is
 * digested, because that is all the standard specifies: a routine that returns the difference of
 * the two bytes and one that returns minus one are both right, and pinning the magnitude would
 * make this a test of an accident rather than of the contract.
 */
static void comparisons(int length) {
    unsigned long long digest = 14695981039346656037ull;
    for (int phase = 0; phase < PHASES; phase++) {
        fill_random(source, (size_t)ROOM);
        for (size_t i = 0; i < (size_t)ROOM; i++) {
            other[i] = source[i];
        }
        unsigned char *left = source + PAD + phase;
        unsigned char *right = other + PAD + phase;
        int answer = memcmp(left, right, (size_t)length);
        digest = mix_number(digest, answer == 0);
        cases++;
        if (length == 0) {
            continue;
        }
        for (int which = 0; which < 4; which++) {
            int at = (int)(next_random() % (unsigned long long)length);
            unsigned char was = right[at];
            /* A difference in the top bit as well as a difference in the low bits, since the
             * comparison is on unsigned bytes and a signed one gets that pair the wrong way round.
             */
            right[at] = (unsigned char)(which < 2 ? was ^ 0x80 : was ^ 0x01);
            int one = memcmp(left, right, (size_t)length);
            int two = memcmp(right, left, (size_t)length);
            digest = mix_number(digest, (one > 0) - (one < 0));
            digest = mix_number(digest, (two > 0) - (two < 0));
            right[at] = was;
            cases += 2;
        }
    }
    say("memcmp", length, digest);
}

/* A value whose width is itself random, because the implementations branch on how many digits an
 * operand has: a pair that both fit in sixty four bits goes one way, a pair that does not goes
 * another, and a dividend shorter than its divisor goes a third. A stream of full width values
 * would only ever ask about one of those.
 */
static uwide random_wide(void) {
    int width = (int)(next_random() % 129);
    if (width == 0) {
        return 0;
    }
    uwide whole = ((uwide)next_random() << 64) | (uwide)next_random();
    return whole >> (128 - width);
}

/* The places a 128-bit value is worth asking about by name rather than by luck: the two ends of the
 * type, the digit boundaries whatever base an implementation works in, and the values either side
 * of each.
 */
static const int CORNER_SHIFTS[] = {0, 1, 31, 32, 33, 63, 64, 65, 95, 96, 97, 126, 127};

#define CORNER_SHIFT_COUNT ((int)(sizeof CORNER_SHIFTS / sizeof CORNER_SHIFTS[0]))
#define CORNERS (3 * CORNER_SHIFT_COUNT + 2)

/* Volatile, which is the only part of this file that is about the compiler reading it rather than
 * about the routines under test. These are the operands a compiler can work out for itself, and a
 * division it works out for itself is a division it does at compile time with its own arithmetic.
 * That would be a test of gcc.
 */
static volatile uwide corners[CORNERS];

static void make_corners(void) {
    int at = 0;
    for (int i = 0; i < CORNER_SHIFT_COUNT; i++) {
        uwide one = (uwide)1 << CORNER_SHIFTS[i];
        corners[at++] = one - 1;
        corners[at++] = one;
        corners[at++] = one + 1;
    }
    corners[at++] = ~(uwide)0;
    corners[at++] = ~(uwide)0 - 1;
}

/* One pair through all six entry points: unsigned, then the same bits read as signed, which is
 * where truncation towards zero and the sign of a remainder get decided.
 */
static unsigned long long pair(unsigned long long digest, uwide top, uwide bottom) {
    if (bottom == 0) {
        /* Dividing by zero is undefined and the machine traps on it, so there is no answer here for
         * two sides to agree about. One instead, which keeps the dividend rather than dropping the
         * case.
         */
        bottom = 1;
    }
    uwide rest;
    digest = mix_wide(digest, top / bottom);
    digest = mix_wide(digest, top % bottom);
    digest = mix_wide(digest, __udivmodti4(top, bottom, &rest));
    digest = mix_wide(digest, rest);
    cases += 4;

    wide signed_top = (wide)top;
    wide signed_bottom = (wide)bottom;
    /* Every pair but the most negative value over minus one, whose quotient is one past the type.
     * That is an overflow, and a program that is the reference for what the library does is not the
     * place to find out what this one's compiler makes of one.
     */
    if (!(signed_top == (wide)((uwide)1 << 127) && signed_bottom == -1)) {
        wide signed_rest;
        digest = mix_wide(digest, (uwide)(signed_top / signed_bottom));
        digest = mix_wide(digest, (uwide)(signed_top % signed_bottom));
        digest = mix_wide(digest, (uwide)__divmodti4(signed_top, signed_bottom, &signed_rest));
        digest = mix_wide(digest, (uwide)signed_rest);
        cases += 4;
    }
    return digest;
}

/* How many rounds of random pairs, and how many pairs in each. A round is a group, so the number of
 * rounds is how finely a disagreement gets located and the number of cases is how likely it is to
 * be found at all.
 */
#define DIVISION_ROUNDS 8
#define DIVISION_CASES 2048

static void divisions(int round) {
    unsigned long long digest = 14695981039346656037ull;
    for (int i = 0; i < DIVISION_CASES; i++) {
        digest = pair(digest, random_wide(), random_wide());
    }
    say("divide", round, digest);
}

/* One less than the divisor, and one more, which is the pair that makes a long division in any base
 * estimate a quotient digit one too big and then have to take it back. The estimate looks at the top
 * digits only, and here they are equal while the whole values are not. It is rare enough among random
 * pairs not to come up at all, which was measured rather than assumed: with the add back in the
 * reference taken out by hand, every group of random pairs still agreed and what differed was these
 * and the corner groups, where one less than a power of two sits next to it in the table.
 */
static void near_misses(int round) {
    unsigned long long digest = 14695981039346656037ull;
    for (int i = 0; i < DIVISION_CASES; i++) {
        uwide bottom = random_wide() | 1;
        digest = pair(digest, bottom - 1, bottom);
        digest = pair(digest, bottom, bottom - 1);
        digest = pair(digest, bottom + 1, bottom);
    }
    say("divnear", round, digest);
}

/* Every corner against every corner, both ways round, grouped by which corner one side was. */
static void corner_divisions(int which) {
    unsigned long long digest = 14695981039346656037ull;
    for (int i = 0; i < CORNERS; i++) {
        digest = pair(digest, corners[which], corners[i]);
        digest = pair(digest, corners[i], corners[which]);
    }
    say("divcorner", which, digest);
}

/* The same three families one width down, for the six routines a 32-bit target calls.
 *
 * Random widths for the same reason: the C branches on whether both operands fit in thirty two bits
 * and the reference branches on how many base 2^32 digits the divisor has, and a stream of full
 * width values would only ever ask about one side of each.
 */
static unsigned long long random_long(void) {
    int width = (int)(next_random() % 65);
    if (width == 0) {
        return 0;
    }
    return next_random() >> (64 - width);
}

/* The ends of the narrower type, the boundary between its halves, and the digit boundaries of the
 * reference, with the values either side of each.
 */
static const int CORNER_SHIFTS_LONG[] = {0, 1, 15, 16, 17, 31, 32, 33, 62, 63};

#define CORNER_SHIFT_LONG_COUNT ((int)(sizeof CORNER_SHIFTS_LONG / sizeof CORNER_SHIFTS_LONG[0]))
#define CORNERS_LONG (3 * CORNER_SHIFT_LONG_COUNT + 2)

/* Volatile for the reason the wider table is, and it costs nothing to keep the same rule here. The
 * divisions below are opaque calls rather than arithmetic, so there is nothing for a compiler to
 * work out in advance, but the table is also read as signed and that is arithmetic.
 */
static volatile unsigned long long corners_long[CORNERS_LONG];

static void make_corners_long(void) {
    int at = 0;
    for (int i = 0; i < CORNER_SHIFT_LONG_COUNT; i++) {
        unsigned long long one = 1ull << CORNER_SHIFTS_LONG[i];
        corners_long[at++] = one - 1;
        corners_long[at++] = one;
        corners_long[at++] = one + 1;
    }
    corners_long[at++] = ~0ull;
    corners_long[at++] = ~0ull - 1;
}

/* One pair through all six, unsigned and then the same bits read as signed. */
static unsigned long long pair_long(unsigned long long digest, unsigned long long top,
                                    unsigned long long bottom) {
    if (bottom == 0) {
        bottom = 1;
    }
    unsigned long long rest;
    digest = mix_number(digest, (long long)__udivdi3(top, bottom));
    digest = mix_number(digest, (long long)__umoddi3(top, bottom));
    digest = mix_number(digest, (long long)__udivmoddi4(top, bottom, &rest));
    digest = mix_number(digest, (long long)rest);
    cases += 4;

    long long signed_top = (long long)top;
    long long signed_bottom = (long long)bottom;
    /* Every pair but the most negative value over minus one, whose quotient is one past the type,
     * which is the same case the wider width leaves out and for the same reason.
     */
    if (!(signed_top == (long long)(1ull << 63) && signed_bottom == -1)) {
        long long signed_rest;
        digest = mix_number(digest, __divdi3(signed_top, signed_bottom));
        digest = mix_number(digest, __moddi3(signed_top, signed_bottom));
        digest = mix_number(digest, __divmoddi4(signed_top, signed_bottom, &signed_rest));
        digest = mix_number(digest, signed_rest);
        cases += 4;
    }
    return digest;
}

static void divisions_long(int round) {
    unsigned long long digest = 14695981039346656037ull;
    for (int i = 0; i < DIVISION_CASES; i++) {
        digest = pair_long(digest, random_long(), random_long());
    }
    say("divide64", round, digest);
}

static void near_misses_long(int round) {
    unsigned long long digest = 14695981039346656037ull;
    for (int i = 0; i < DIVISION_CASES; i++) {
        unsigned long long bottom = random_long() | 1;
        digest = pair_long(digest, bottom - 1, bottom);
        digest = pair_long(digest, bottom, bottom - 1);
        digest = pair_long(digest, bottom + 1, bottom);
    }
    say("divnear64", round, digest);
}

static void corner_divisions_long(int which) {
    unsigned long long digest = 14695981039346656037ull;
    for (int i = 0; i < CORNERS_LONG; i++) {
        digest = pair_long(digest, corners_long[which], corners_long[i]);
        digest = pair_long(digest, corners_long[i], corners_long[which]);
    }
    say("divcorner64", which, digest);
}

/* The eight conversions between a 128-bit integer and a float.
 *
 * Nothing here says their names, because a cast is what a program writes and a call is what the
 * compiler makes of it, which is the same arrangement the divisions are tested under. The names are
 * checked against what each archive defines rather than read off this file.
 *
 * The float that comes back is digested as its bits rather than as a number, so that a difference in
 * the last place is a difference rather than something printf rounded away. memcpy for that, since a
 * union or a cast through a pointer would be this file taking a position on effective types while
 * testing something else.
 */
static unsigned long long mix_double(unsigned long long digest, double value) {
    unsigned char bytes[sizeof value];
    memcpy(bytes, &value, sizeof value);
    return mix(digest, bytes, sizeof bytes);
}

static unsigned long long mix_float(unsigned long long digest, float value) {
    unsigned char bytes[sizeof value];
    memcpy(bytes, &value, sizeof value);
    return mix(digest, bytes, sizeof bytes);
}

/* One integer through all four conversions up. The signed pair reads the same bits as a signed
 * value, which is where the sign and the magnitude of the most negative value get decided.
 */
static unsigned long long up(unsigned long long digest, uwide value) {
    digest = mix_double(digest, (double)value);
    digest = mix_float(digest, (float)value);
    digest = mix_double(digest, (double)(wide)value);
    digest = mix_float(digest, (float)(wide)value);
    cases += 4;
    return digest;
}

/* One float through all four conversions down, at both widths it arrives in.
 *
 * The float has to be inside the integer it is going to, because a value that is not is undefined in
 * C and there is no answer for two implementations to agree about. The caller is what keeps it
 * inside: every value here is built with an exponent below the top of the signed type, which is also
 * below the top of the unsigned one.
 */
static unsigned long long down(unsigned long long digest, double value) {
    digest = mix_wide(digest, (uwide)value);
    digest = mix_wide(digest, (uwide)(float)value);
    digest = mix_wide(digest, (uwide)(wide)value);
    digest = mix_wide(digest, (uwide)(wide)(float)value);
    cases += 4;
    return digest;
}

/* A float whose exponent is random over the range the integer holds and whose mantissa is random
 * bits, built out of its bits rather than out of arithmetic so that the exponent is the thing being
 * swept. The exponent stops at 2^125 so that the value is inside the signed type as well as the
 * unsigned one, with room for the narrowing to a float to round it up, and goes down to 2^-8 so that
 * the cases that truncate to zero and to one are in here too.
 */
static double random_float(void) {
    unsigned long long bits = next_random();
    unsigned long long exponent = 1023 - 8 + (next_random() % 134);
    double value;
    bits = (bits & 0x800FFFFFFFFFFFFFull) | (exponent << 52);
    memcpy(&value, &bits, sizeof value);
    return value;
}

#define CONVERSION_ROUNDS 8
#define CONVERSION_CASES 2048

static void conversions(int round) {
    unsigned long long digest = 14695981039346656037ull;
    for (int i = 0; i < CONVERSION_CASES; i++) {
        digest = up(digest, random_wide());
        digest = down(digest, random_float());
    }
    say("convert", round, digest);
}

/* Every corner value up, and the float of every corner value back down.
 *
 * Going up, a power of two and the values either side of it are where the rounding is decided by
 * something other than the bits: one of the three is exact, one rounds down and one rounds up, and
 * at this width all three are past the precision of both formats. Coming back down from the float of
 * one is how a value that came out rounded gets asked about in the other direction.
 */
static void corner_conversions(int which) {
    unsigned long long digest = 14695981039346656037ull;
    uwide value = corners[which];
    digest = up(digest, value);
    /* Only the ones the signed type holds, since the float of a larger one is outside it. */
    if ((value >> 126) == 0) {
        digest = down(digest, (double)value);
        digest = down(digest, -(double)value);
    }
    for (int i = 0; i < CORNERS; i++) {
        uwide other = corners[i];
        digest = up(digest, value ^ other);
        digest = up(digest, value + other);
    }
    say("convcorner", which, digest);
}

/* The sign bit of a single, which the groups below set and clear on operands of their own. */
#define SINGLE_SIGN 0x80000000u

/* A bit pattern as the float it is, by memcpy for the reason the digests use it: a union or a cast
 * through a pointer would be this file taking a position on effective types while testing something
 * else.
 */
static float single_of(unsigned int pattern) {
    float value;
    memcpy(&value, &pattern, sizeof value);
    return value;
}

/* The sign of an answer and nothing else, which is all eight comparisons promise. */
static unsigned long long mix_sign(unsigned long long digest, int answer) {
    return mix_number(digest, (answer > 0) - (answer < 0));
}

/* One pair through all thirteen routines. The negation takes one operand, so it is the left one here
 * and every pair below is also run the other way round, which is what gets the right one through it.
 */
static unsigned long long thirteen(unsigned long long digest, unsigned int left,
                                   unsigned int right) {
    float one = single_of(left);
    float two = single_of(right);
    digest = mix_float(digest, __addsf3(one, two));
    digest = mix_float(digest, __subsf3(one, two));
    digest = mix_float(digest, __mulsf3(one, two));
    digest = mix_float(digest, __divsf3(one, two));
    digest = mix_float(digest, __negsf2(one));
    digest = mix_sign(digest, __cmpsf2(one, two));
    digest = mix_sign(digest, __eqsf2(one, two));
    digest = mix_sign(digest, __nesf2(one, two));
    digest = mix_sign(digest, __gesf2(one, two));
    digest = mix_sign(digest, __gtsf2(one, two));
    digest = mix_sign(digest, __lesf2(one, two));
    digest = mix_sign(digest, __ltsf2(one, two));
    digest = mix_sign(digest, __unordsf2(one, two));
    cases += 13;
    return digest;
}

/* Random bits read as a float, which is how the infinities, the not a numbers and the subnormals get
 * in without a generator for each: every one of those is a slice of the patterns, and one pattern in
 * every two hundred and fifty six lands in the slice that is not an ordinary number.
 */
static unsigned int random_single(void) {
    return (unsigned int)(next_random() >> 32);
}

/* A pattern whose exponent is within four of another's, which is where the additions that say
 * something are. Two values far apart add to the larger one and ask nothing about the alignment, and
 * random pairs are far apart nearly always.
 */
static unsigned int near_single(unsigned int other) {
    long long moved = (long long)((other >> 23) & 0xFF) + (long long)(next_random() % 9) - 4;
    if (moved < 0) {
        moved = 0;
    }
    if (moved > 254) {
        moved = 254;
    }
    return (random_single() & 0x807FFFFFu) | ((unsigned int)moved << 23);
}

#define SINGLE_ROUNDS 8
#define SINGLE_CASES 2048

static void singles(int round) {
    unsigned long long digest = 14695981039346656037ull;
    for (int i = 0; i < SINGLE_CASES; i++) {
        unsigned int left = random_single();
        unsigned int right = random_single();
        unsigned int close = near_single(left);
        digest = thirteen(digest, left, right);
        digest = thirteen(digest, right, left);
        digest = thirteen(digest, left, close);
        digest = thirteen(digest, close, left);
    }
    say("single", round, digest);
}

/* A value and exactly half the spacing of its own exponent, which is the pair that puts a sum
 * exactly between two floats. There the rounding cannot be decided by the bits that were dropped,
 * because they are the same either way, and what decides it is the lowest bit that was kept. Both
 * values of that bit are here by name rather than by luck, since an implementation that rounds a tie
 * up agrees with a correct one on one of the two.
 */
static void single_ties(int round) {
    unsigned long long digest = 14695981039346656037ull;
    for (int i = 0; i < SINGLE_CASES; i++) {
        /* An exponent with room for half the spacing to be an ordinary number below it and room for
         * the sum to stay inside the type above it.
         */
        unsigned int stored = 30 + (unsigned int)(next_random() % 200);
        unsigned int value = (random_single() & 0x007FFFFFu) | (stored << 23);
        unsigned int half = (stored - 24) << 23;
        digest = thirteen(digest, value & ~1u, half);
        digest = thirteen(digest, value | 1u, half);
        digest = thirteen(digest, (value & ~1u) | SINGLE_SIGN, half);
        digest = thirteen(digest, half, value | 1u);
    }
    say("singletie", round, digest);
}

/* The bottom of the range and the top of it, which random patterns reach now and then and these two
 * families are nothing but.
 *
 * At the bottom a result has fewer bits than the format usually keeps, and an implementation that
 * rounded before it shifted into place gets a different answer there. At the top a rounding carries
 * past the largest finite value and the answer is an infinity rather than a wrapped exponent.
 */
static void single_edges(int round) {
    unsigned long long digest = 14695981039346656037ull;
    for (int i = 0; i < SINGLE_CASES; i++) {
        unsigned int small = (random_single() & 0x807FFFFFu)
                             | ((unsigned int)(next_random() % 3) << 23);
        unsigned int other = (random_single() & 0x807FFFFFu)
                             | ((unsigned int)(next_random() % 3) << 23);
        unsigned int large = (random_single() & 0x807FFFFFu)
                             | ((unsigned int)(248 + next_random() % 7) << 23);
        digest = thirteen(digest, small, other);
        digest = thirteen(digest, small, large);
        digest = thirteen(digest, large, small);
        digest = thirteen(digest, large, large);
    }
    say("singleedge", round, digest);
}

/* The patterns worth asking about by name: both zeros, the ends of the subnormal range, the ends of
 * the normal one, the two powers of two where the spacing of the floats reaches and passes one, half
 * the spacing at one, an infinity, and a not a number of each kind.
 */
static const unsigned int SINGLE_CORNERS[] = {
    0x00000000u, 0x00000001u, 0x00000002u, 0x007FFFFEu, 0x007FFFFFu, 0x00800000u, 0x00800001u,
    0x33800000u, 0x34000000u, 0x3F7FFFFFu, 0x3F800000u, 0x3F800001u, 0x40000000u, 0x4B000000u,
    0x4B800000u, 0x7F7FFFFFu, 0x7F800000u, 0x7F800001u, 0x7FC00000u, 0x7FFFFFFFu,
};

#define SINGLE_CORNER_COUNT ((int)(sizeof SINGLE_CORNERS / sizeof SINGLE_CORNERS[0]))

/* Every corner against every corner, both ways round and at all four pairs of signs, grouped by
 * which corner one side was. The signs are swept rather than sampled because the sign of a zero and
 * the sign of an infinity are where a subtraction and a division decide what to hand back out of the
 * two signs they were given.
 */
static void single_corner_cases(int which) {
    unsigned long long digest = 14695981039346656037ull;
    for (int i = 0; i < SINGLE_CORNER_COUNT; i++) {
        for (int signs = 0; signs < 4; signs++) {
            unsigned int left = SINGLE_CORNERS[which] | ((signs & 1) ? SINGLE_SIGN : 0u);
            unsigned int right = SINGLE_CORNERS[i] | ((signs & 2) ? SINGLE_SIGN : 0u);
            digest = thirteen(digest, left, right);
            digest = thirteen(digest, right, left);
        }
    }
    say("singlecorner", which, digest);
}

int main(int argc, char **argv) {
    if (argc > 1) {
        unsigned long long seed = strtoull(argv[1], NULL, 0);
        if (seed != 0) {
            state = seed;
        }
    }
    for (int i = 0; i < LENGTHS_COUNT; i++) {
        int length = LENGTHS[i];
        copies(length);
        moves(length);
        fills(length);
        comparisons(length);
    }
    make_corners();
    make_corners_long();
    for (int i = 0; i < CORNERS; i++) {
        corner_divisions(i);
    }
    for (int i = 0; i < CORNERS_LONG; i++) {
        corner_divisions_long(i);
    }
    for (int i = 0; i < CORNERS; i++) {
        corner_conversions(i);
    }
    for (int i = 0; i < DIVISION_ROUNDS; i++) {
        divisions(i);
        near_misses(i);
        divisions_long(i);
        near_misses_long(i);
    }
    for (int i = 0; i < CONVERSION_ROUNDS; i++) {
        conversions(i);
    }
    for (int i = 0; i < SINGLE_CORNER_COUNT; i++) {
        single_corner_cases(i);
    }
    for (int i = 0; i < SINGLE_ROUNDS; i++) {
        singles(i);
        single_ties(i);
        single_edges(i);
    }
    printf("cases %ld\n", cases);
    return 0;
}
