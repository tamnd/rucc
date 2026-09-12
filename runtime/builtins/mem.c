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
 * No word at a time path yet. These are the simple loops, they are correct, and a faster memcpy
 * is a measurement rather than an opinion: the word loop wants a benchmark to hold it to and the
 * unaligned prologue it needs is where this kind of routine is usually got wrong. What exists
 * first is the archive a cross link can read.
 */

#include <stddef.h>

void *memcpy(void *to, const void *from, size_t count) {
    unsigned char *out = to;
    const unsigned char *in = from;
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
 */
void *memmove(void *to, const void *from, size_t count) {
    unsigned char *out = to;
    const unsigned char *in = from;
    if (out < in) {
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
 */
int memcmp(const void *one, const void *two, size_t count) {
    const unsigned char *left = one;
    const unsigned char *right = two;
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
