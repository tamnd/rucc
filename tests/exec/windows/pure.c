/* A `pure` or `const` function that gives back a value too big for a register.
 *
 * On Windows x64 a complex double, a complex long double and any struct over eight bytes come back
 * through a pointer the caller hands over, so the function writes memory to return at all. A caller
 * that believed the attribute's promise to write nothing lost the store at -O2 and read whatever
 * was in the slot before the call. execute/20050121-1.c and 20070614-1.c in the gcc torture tests
 * are where this was found. */
#include <stdio.h>

struct three { int a, b, c; };

__attribute__((pure, noinline)) static double _Complex dpair(int x) {
    double _Complex r;
    __real__ r = x + 1;
    __imag__ r = x - 1;
    return r;
}

__attribute__((pure, noinline)) static long double _Complex lpair(int x) {
    long double _Complex r;
    __real__ r = x + 2;
    __imag__ r = x - 2;
    return r;
}

__attribute__((const, noinline)) static struct three counted(int x) {
    struct three t = { x, x * 2, x * 3 };
    return t;
}

int main(void) {
    printf("%g %g\n", __real__ dpair(5), __imag__ dpair(5));
    printf("%g %g\n", (double)__real__ lpair(5), (double)__imag__ lpair(5));
    printf("%d %d\n", counted(4).b, counted(4).c);
    return 0;
}
