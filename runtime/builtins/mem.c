/* The four block routines, which are what the backend calls when a copy or a fill is too big to
 * open up into moves.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8, which settled in tamnd/rucc#912 that what
 * ships is C compiled by rucc itself rather than the Rust crate next door. The Rust crate stays
 * as the reference these are tested against.
 *
 * The names are libc's rather than names of our own, because an object we produced gets linked
 * against objects GCC produced and there is one memcpy in a program. That is also the hazard in
 * the file: a loop that copies bytes is a loop a compiler is allowed to recognize and replace
 * with a call to memcpy, and in this file that call would be the function calling itself. rucc
 * does not recognize the pattern today and -fno-builtin is on the command line that builds this
 * anyway, which is the same thing #![no_builtins] does for the Rust crate.
 *
 * A word at a time, when the two ends of the call are the same distance from a word boundary. The
 * loop runs after a prologue that copies bytes up to that boundary, and it runs at all only under
 * that condition, because on a target that faults on an unaligned load there is no other legal
 * way to do it and on the targets that do not fault the misaligned case is a shift and merge loop
 * whose byte order is one more thing to get wrong. So a call whose two ends disagree still runs a
 * byte at a time, and `cargo xtask builtins-bench` has a row for that case next to the row for the
 * aligned one so that nobody has to guess which of the two a change moved. The misaligned pair is
 * the next measurement rather than the next opinion.
 *
 * Written with no helper functions in it and no divide in it, and both of those are about the
 * compiler that compiles it rather than about taste. rucc has no inliner, so a two line predicate
 * factored out of these loops is a call on the hot path and is worth about a third of the time of
 * a short memcpy. It does not turn a divide by a constant power of two into a shift either, so
 * `count / sizeof(word)` is a `div` instruction, which is why the loops below count down against
 * the word size instead of working out how many words there are first. Both of those are gaps in
 * the optimizer and the day either closes this file gets simpler; until then it is written for the
 * compiler it actually has, which is the whole point of the runtime being compiled by rucc.
 *
 * What holds this to its answers is `cargo xtask builtins-diff`, which runs both implementations
 * over every length up to eighty and every pair of alignments, so the prologue and the tail are
 * covered at every offset a word loop can leave a byte at.
 */

#include <stddef.h>
#include <stdint.h>

/* The unit the loops move. A pointer's width, which is the widest thing a target is sure to load
 * and store in one go, and which is what makes this eight bytes at a time on a 64-bit machine and
 * four on a 32-bit one without the file saying either number.
 */
typedef size_t word;

#define STEP (sizeof(word))

/* Below this there is no word loop to reach. The prologue can eat up to STEP - 1 bytes getting to
 * a boundary, so a call shorter than two words might have nothing left for the loop and would
 * have paid for the test anyway.
 */
#define WORTH_IT (STEP * 2)

/* Whether two addresses are the same distance from a word boundary, which is the condition the
 * word loop needs: align one of them and the other comes with it.
 */
#define SAME_SKEW(one, two) (((((uintptr_t)(one)) ^ ((uintptr_t)(two))) & (STEP - 1)) == 0)

/* How many bytes from here up to the next word boundary, and zero when this is one. */
#define TO_BOUNDARY(at) ((size_t)((STEP - ((uintptr_t)(at) & (STEP - 1))) & (STEP - 1)))

/* How many bytes from here down to the boundary below, which is the address itself modulo the
 * word and is what the backward copy has to shed before its own loop can start.
 */
#define PAST_BOUNDARY(at) ((size_t)((uintptr_t)(at) & (STEP - 1)))

void *memcpy(void *to, const void *from, size_t count) {
    unsigned char *out = to;
    const unsigned char *in = from;
    if (count >= WORTH_IT && SAME_SKEW(out, in)) {
        size_t lead = TO_BOUNDARY(out);
        count -= lead;
        while (lead != 0) {
            *out = *in;
            out += 1;
            in += 1;
            lead -= 1;
        }
        while (count >= STEP) {
            *(word *)out = *(const word *)in;
            out += STEP;
            in += STEP;
            count -= STEP;
        }
    }
    while (count != 0) {
        *out = *in;
        out += 1;
        in += 1;
        count -= 1;
    }
    return to;
}

/* The one routine where the direction matters. The ranges may overlap, and a copy that runs
 * forwards over a destination above the source reads bytes it has already written, so that case
 * runs backwards instead. Equal pointers are either direction and copying nothing is what it
 * does in the end.
 *
 * The forward half is memcpy's loop written again rather than a call to memcpy, for the reason in
 * the header: a call here is a call, and this is the direction most memmoves take.
 */
void *memmove(void *to, const void *from, size_t count) {
    unsigned char *out = to;
    const unsigned char *in = from;
    if (out < in) {
        if (count >= WORTH_IT && SAME_SKEW(out, in)) {
            size_t lead = TO_BOUNDARY(out);
            count -= lead;
            while (lead != 0) {
                *out = *in;
                out += 1;
                in += 1;
                lead -= 1;
            }
            while (count >= STEP) {
                *(word *)out = *(const word *)in;
                out += STEP;
                in += STEP;
                count -= STEP;
            }
        }
        while (count != 0) {
            *out = *in;
            out += 1;
            in += 1;
            count -= 1;
        }
        return to;
    }
    out += count;
    in += count;
    if (count >= WORTH_IT && SAME_SKEW(out, in)) {
        size_t lead = PAST_BOUNDARY(out);
        count -= lead;
        while (lead != 0) {
            out -= 1;
            in -= 1;
            *out = *in;
            lead -= 1;
        }
        while (count >= STEP) {
            out -= STEP;
            in -= STEP;
            *(word *)out = *(const word *)in;
            count -= STEP;
        }
    }
    while (count != 0) {
        out -= 1;
        in -= 1;
        *out = *in;
        count -= 1;
    }
    return to;
}

/* The value is an int and what gets stored is its low byte, which is what the standard says and
 * is the part people get wrong when they write the parameter as a char.
 */
void *memset(void *at, int value, size_t count) {
    unsigned char *out = at;
    unsigned char byte = (unsigned char)value;
    if (count >= WORTH_IT) {
        /* The byte spread across a whole word, doubled up rather than written as one shift per
         * width, because a shift by the width of the type is undefined and a file that has to
         * work at both widths would have one of those in it either way.
         */
        word fill = byte;
        size_t spread = 8;
        size_t lead = TO_BOUNDARY(out);
        while (spread < STEP * 8) {
            fill |= fill << spread;
            spread *= 2;
        }
        count -= lead;
        while (lead != 0) {
            *out = byte;
            out += 1;
            lead -= 1;
        }
        while (count >= STEP) {
            *(word *)out = fill;
            out += STEP;
            count -= STEP;
        }
    }
    while (count != 0) {
        *out = byte;
        out += 1;
        count -= 1;
    }
    return at;
}

/* The comparison is between unsigned chars, so a byte with its top bit set is greater than one
 * without it whatever plain char happens to be on this target. The result only has to have the
 * right sign, and the difference of the two bytes is the answer every libc returns.
 *
 * The word loop here only says that a word is equal, never which of two unequal words is the
 * smaller, because that answer depends on the byte order and the sign of the result is specified.
 * So a word that differs stops the loop with the pointers still on it and the byte loop below
 * walks into it and finds the first byte that differs, which is the same answer at any endianness
 * and costs at most one word of bytes.
 */
int memcmp(const void *one, const void *two, size_t count) {
    const unsigned char *left = one;
    const unsigned char *right = two;
    if (count >= WORTH_IT && SAME_SKEW(left, right)) {
        size_t lead = TO_BOUNDARY(left);
        count -= lead;
        while (lead != 0) {
            if (*left != *right) {
                return (int)*left - (int)*right;
            }
            left += 1;
            right += 1;
            lead -= 1;
        }
        while (count >= STEP && *(const word *)left == *(const word *)right) {
            left += STEP;
            right += STEP;
            count -= STEP;
        }
    }
    while (count != 0) {
        if (*left != *right) {
            return (int)*left - (int)*right;
        }
        left += 1;
        right += 1;
        count -= 1;
    }
    return 0;
}
