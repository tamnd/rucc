/* x86_64 only: the five word buffer is the one Postgres declares for MinGW64 on x86-64 */
/* __builtin_setjmp and __builtin_longjmp the way Postgres uses them on MinGW64, where c.h makes
 * sigjmp_buf five intptr_t and sigsetjmp and siglongjmp the builtin pair. PG_TRY saves a place,
 * points PG_exception_stack at it, and elog(ERROR) jumps back through that pointer from deep in a
 * call stack; PG_CATCH can rethrow to the try around it. Nothing here unwinds with RtlUnwindEx the
 * way msvcrt's longjmp does: the pair puts back the stack and frame pointers, and every other
 * register the function cared about, including RSI, RDI and XMM6 to XMM15, which Windows x64 asks
 * a function to keep, has to come back out of its frame. It runs a thousand rounds so that a stack
 * pointer put back a word off shows up as a crash or a wrong sum rather than as luck. tamnd/rucc#1993. */
#include <stdint.h>
#include <stdio.h>

typedef intptr_t sigjmp_buf[5];
#define sigsetjmp(x, y) __builtin_setjmp(x)
#define siglongjmp __builtin_longjmp

static sigjmp_buf *exception_stack;
static volatile double sink;
static long inner_total;

__attribute__((noinline, noreturn)) static void rethrow(void) { siglongjmp(*exception_stack, 1); }

__attribute__((noinline)) static double down(int n, double x) {
    double a = x * 1.25, b = x * 2.5, c = x + 3.0, d = x - 0.5;
    if (n == 0)
        rethrow();
    double r = down(n - 1, x + 1.0);
    sink = a + b + c + d;
    return r + a + b + c + d;
}

/* PG_TRY around a call that throws, and a PG_CATCH that looks at what it kept and rethrows. */
__attribute__((noinline)) static int inner(int n, double base) {
    sigjmp_buf *save = exception_stack;
    sigjmp_buf local;
    double p = base * 2.0, q = base + 3.0, r = base - 1.0;
    long a = n * 3, b = n + 7, c = n * n, d = n - 2, e = n * 11, f = n + 100;
    if (sigsetjmp(local, 0) == 0) {
        exception_stack = &local;
        sink = down(n, base);
        exception_stack = save;
        return -1;
    }
    exception_stack = save;
    inner_total += a + b + c + d + e + f + (long)(p * 100.0 + q * 100.0 + r * 100.0);
    rethrow();
}

/* The outer PG_TRY, which catches what the inner one rethrew. */
__attribute__((noinline)) static int outer(int round, int *first) {
    sigjmp_buf *save = exception_stack;
    sigjmp_buf local;
    int got = sigsetjmp(local, 0);
    if (got == 0) {
        *first = got;
        exception_stack = &local;
        inner(4, 1.5);
        exception_stack = save;
        return -1;
    }
    exception_stack = save;
    return got == 1 ? round + 1 : -2;
}

int main(void) {
    long rounds = 0;
    int caught = 0, first = -1;
    for (int round = 0; round < 1000; round++) {
        int got = outer(round, &first);
        if (got > 0 && first == 0)
            caught++;
        rounds += got;
    }
    printf("caught %d rounds, rounds sum %ld\n", caught, rounds);
    printf("inner sum %ld\n", inner_total);
    printf("exception stack cleared %d\n", exception_stack == NULL);
    return caught == 1000 ? 0 : 1;
}
