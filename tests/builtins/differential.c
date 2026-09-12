/* The block routines, run over the same cases as whatever is linked beside this file.
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
    printf("cases %ld\n", cases);
    return 0;
}
