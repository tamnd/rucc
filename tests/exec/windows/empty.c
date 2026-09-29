/* An empty struct passed to a function on Windows x64, fixed and through `...`.
 *
 * gcc passes one as the address of a copy, because its size is not 1, 2, 4 or 8, so it takes up a
 * position and every argument after it moves along one. The zero length array is gcc's extension
 * and the empty struct is another one; both have size zero. va-arg-22 in the gcc torture tests is
 * where this was found. */
#include <stdarg.h>
#include <stdio.h>

typedef struct { char x[0]; } Zero;
typedef struct { char x[3]; } Three;

__attribute__((noinline)) static int fixed(Zero z, int a, int b) { return a * 10 + b; }

__attribute__((noinline)) static void listed(int n, ...) {
    va_list ap;
    va_start(ap, n);
    Zero z = va_arg(ap, Zero);
    (void)z;
    int a = va_arg(ap, int);
    Three t = va_arg(ap, Three);
    int b = va_arg(ap, int);
    va_end(ap);
    printf("listed %d %d %d %d %d %d\n", n, a, t.x[0], t.x[1], t.x[2], b);
}

int main(void) {
    Zero z;
    Three t = {{7, 8, 9}};
    printf("fixed %d\n", fixed(z, 4, 2));
    listed(1, z, 5, t, 6);
    return 0;
}
