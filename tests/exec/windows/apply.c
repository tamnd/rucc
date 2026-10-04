/* x86_64 only: the block __builtin_apply_args answers is laid out for x86_64 */
/* __builtin_apply_args and __builtin_apply, which pass the arguments a function was called with on
 * to another without knowing what they are. On x86_64 Windows the block is gcc's for that
 * convention, with both register files in it, and the bytes the call copies start with the shadow
 * space, so arguments past the fourth land where the callee reads them. The first case is gcc's
 * execute/pr47237.c. tamnd/rucc#2145. */
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>

static void foo(int arg) {
    if (arg != 5)
        abort();
}

__attribute__((noinline)) static void bar(int arg) {
    foo(arg);
    __builtin_apply((void (*)())foo, __builtin_apply_args(), 16);
}

__attribute__((noinline)) static int mixed(int a, double b, int c, double d, int e, double f) {
    printf("%d %g %d %g %d %g\n", a, b, c, d, e, f);
    return a + c + e;
}

__attribute__((noinline)) static int to_mixed(int a, double b, int c, double d, int e, double f) {
    (void)a, (void)b, (void)c, (void)d, (void)e, (void)f;
    void *back = __builtin_apply((void (*)())mixed, __builtin_apply_args(), 64);
    return *(int *)back;
}

__attribute__((noinline)) static double half(double x, float y) {
    return x / 2 + y;
}

__attribute__((noinline)) static double to_half(double x, float y) {
    (void)x, (void)y;
    void *back = __builtin_apply((void (*)())half, __builtin_apply_args(), 32);
    return *(double *)((char *)back + 16);
}

static long sum(int n, ...) {
    va_list ap;
    long total = 0;
    va_start(ap, n);
    for (int i = 0; i < n; i++)
        total += va_arg(ap, long);
    va_end(ap);
    return total;
}

__attribute__((noinline)) static long to_sum(int n, long a, long b, long c, long d, long e) {
    (void)n, (void)a, (void)b, (void)c, (void)d, (void)e;
    void *back = __builtin_apply((void (*)())sum, __builtin_apply_args(), 48);
    return *(long *)back;
}

int main(void) {
    bar(5);
    printf("%d\n", to_mixed(1, 2.5, 3, 4.5, 5, 6.5));
    printf("%g\n", to_half(9.0, 0.25f));
    printf("%ld\n", to_sum(5, 10, 20, 30, 40, 50));
    return 0;
}
