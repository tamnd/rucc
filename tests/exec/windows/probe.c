/* msvc flags: -Wl,/stack:4194304 */
/* gcc flags: -Wl,--stack,4194304 */
/* A frame of one megabyte, touched at its far end first.
 *
 * Windows commits a thread's stack one page at a time behind a guard page, so a function whose frame
 * is bigger than a page has to touch each page in order on the way down, which is what __chkstk is
 * for. Without it the first write below lands past the guard page and the program dies with an
 * access violation instead of printing. Document 09.2.
 *
 * A megabyte is the whole stack lld reserves by default, for lld-link and for llvm-mingw on AArch64,
 * where GNU ld reserves two, so the msvc row and the reference compilers ask for four. */
#include <stdio.h>

__attribute__((noinline)) static unsigned big(unsigned seed) {
    volatile unsigned char a[1 << 20];
    a[0] = (unsigned char)seed;
    for (unsigned i = 1; i < sizeof a; i++)
        a[i] = (unsigned char)(a[i - 1] * 31 + 7);
    unsigned sum = 0;
    for (unsigned i = 0; i < sizeof a; i += 4096)
        sum += a[i];
    return sum;
}

int main(void) {
    printf("probe %u\n", big(3));
    printf("probe %u\n", big(200));
    return 0;
}
