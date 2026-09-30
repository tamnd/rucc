/* gcc flags: -fno-omit-frame-pointer */
/* Frames that force their own alignment, which on Windows are laid out the way clang lays them out:
 * the whole prologue the unwind record describes first, and the stack pointer rounded down after it.
 *
 * Three realigned frames on the stack at once. The deepest one walks the stack with
 * RtlCaptureStackBackTrace, which needs a record for each of them, and then either returns normally
 * or longjmps out through all of them, which unwinds them the same way. The fifth and sixth arguments
 * of deep are read from the caller's stack through the frame pointer, and middle and outer keep
 * doubles live across the call, which puts vector registers in the saves. tamnd/rucc#1422.
 *
 * On AArch64, RtlCaptureStackBackTrace follows the chain of frame records that x29 heads, which
 * clang for mingw leaves out of a frame unless it is asked for one, so the reference build asks. */
#include <windows.h>
#include <setjmp.h>
#include <stdint.h>
#include <stdio.h>

static jmp_buf back;
static int jump;
static void *want[3];

__attribute__((noinline)) static int walk(void) {
    void *frames[32];
    USHORT n = RtlCaptureStackBackTrace(0, 32, frames, NULL);
    int found = 0;
    for (USHORT i = 0; i < n; i++) {
        DWORD64 base = 0;
        PRUNTIME_FUNCTION f = RtlLookupFunctionEntry((DWORD64)frames[i] - 1, &base, NULL);
        if (!f)
            continue;
        for (int k = 0; k < 3; k++)
            if (base + f->BeginAddress == (DWORD64)want[k])
                found |= 1 << k;
    }
    return found;
}

__attribute__((noinline)) static double deep(int a, int b, int c, int d, int e, int f, double x) {
    _Alignas(64) char buf[64];
    snprintf(buf, sizeof buf, "%d", a + b + c + d + e + f);
    printf("deep aligned %d sum %s x %.1f\n", ((uintptr_t)buf & 63) == 0, buf, x);
    printf("walk %d\n", walk());
    if (jump)
        longjmp(back, 7);
    return x + a;
}

__attribute__((noinline)) static double middle(int n, double x) {
    _Alignas(32) double v[4] = {x, x * 2, x * 3, x * 4};
    printf("middle aligned %d\n", ((uintptr_t)v & 31) == 0);
    double r = deep(n, n + 1, n + 2, n + 3, n + 4, n + 5, v[3]);
    return r + v[0] + v[1] + v[2];
}

__attribute__((noinline)) static double outer(double y) {
    _Alignas(32) volatile char pad[32];
    pad[0] = 1;
    double r = middle(7, y);
    return r * y + pad[0];
}

int main(void) {
    want[0] = (void *)deep;
    want[1] = (void *)middle;
    want[2] = (void *)outer;
    jump = 1;
    int got = setjmp(back);
    if (got == 0) {
        outer(3.0);
        printf("not reached\n");
    } else {
        printf("back %d\n", got);
    }
    jump = 0;
    printf("outer %.1f\n", outer(3.0));
    return 0;
}
