/* longjmp out of five frames that each hold doubles across a call.
 *
 * mingw-w64's setjmp passes the frame address, and msvcrt's longjmp then unwinds to it with
 * RtlUnwindEx, which reads the .pdata and .xdata of every frame on the way. A frame whose unwind
 * information does not say where it saved XMM6 to XMM15, or saves them where it says it does not,
 * either crashes the unwind or hands main back the wrong values. Documents 09.6 and 05.1. */
#include <setjmp.h>
#include <stdio.h>

static jmp_buf env;
static volatile double sink;

__attribute__((noinline)) static double level(int n, double x) {
    double a = x * 1.25, b = x * 2.5, c = x + 3.0, d = x - 0.5;
    double e = a * b, f = c * d, g = a + d, h = b - c;
    if (n == 5)
        longjmp(env, 42);
    double r = level(n + 1, x + 1.0);
    sink = a + b + c + d + e + f + g + h;
    return r + a + b + c + d + e + f + g + h;
}

__attribute__((noinline)) static int run(double base) {
    double keep[6];
    for (int i = 0; i < 6; i++)
        keep[i] = base * (i + 1);
    double p = keep[0] + keep[1], q = keep[2] * keep[3], r = keep[4] - keep[5];
    int got = setjmp(env);
    if (got == 0) {
        level(1, base);
        return -1;
    }
    printf("longjmp returned %d\n", got);
    printf("kept %.2f %.2f %.2f\n", p, q, r);
    return got;
}

int main(void) {
    int got = run(1.5);
    printf("run %d\n", got);
    return got == 42 ? 0 : 1;
}
