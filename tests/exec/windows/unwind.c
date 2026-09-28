/* An access violation eight rucc frames down, and a walk of the stack from inside the handler.
 *
 * RtlCaptureStackBackTrace unwinds through the exception dispatcher into the faulting frame and on
 * up, one RtlVirtualUnwind per frame, so every frame on the way has to have .pdata that covers it and
 * .xdata that says what its prologue did. Each return address is looked up with
 * RtlLookupFunctionEntry and matched to the function it is in. Document 06.4. */
#include <windows.h>
#include <stdio.h>

typedef int (*step)(volatile int *, int);
static step chain[8];
static const char *names[8] = {"d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7"};
static volatile int *nowhere;

#define LEVEL(n, next)                                                                                 \
    __attribute__((noinline)) static int d##n(volatile int *p, int x) {                               \
        volatile char pad[64 * (n + 1)];                                                               \
        pad[0] = (char)x;                                                                              \
        return next(p, x + 1) + pad[0];                                                                \
    }

__attribute__((noinline)) static void touch(volatile char *pad) { pad[1] = pad[0]; }

/* Not a leaf, because a leaf that allocates nothing has no .pdata to find it by in any compiler's
 * output, and then the walk steps over it by reading the return address at the stack pointer. */
__attribute__((noinline)) static int d7(volatile int *p, int x) {
    volatile char pad[32];
    pad[0] = (char)x;
    touch(pad);
    return *p + x + pad[1];
}
LEVEL(6, d7)
LEVEL(5, d6)
LEVEL(4, d5)
LEVEL(3, d4)
LEVEL(2, d3)
LEVEL(1, d2)
LEVEL(0, d1)

static LONG CALLBACK handler(EXCEPTION_POINTERS *info) {
    if (info->ExceptionRecord->ExceptionCode != EXCEPTION_ACCESS_VIOLATION)
        return EXCEPTION_CONTINUE_SEARCH;
    void *frames[64];
    USHORT count = RtlCaptureStackBackTrace(0, 64, frames, NULL);
    int found[8] = {0};
    for (USHORT i = 0; i < count; i++) {
        DWORD64 base = 0;
        DWORD64 pc = (DWORD64)frames[i] - 1;
        PRUNTIME_FUNCTION f = RtlLookupFunctionEntry(pc, &base, NULL);
        if (!f)
            continue;
        for (int k = 0; k < 8; k++)
            if (base + f->BeginAddress == (DWORD64)chain[k])
                found[k] = 1;
    }
    for (int k = 0; k < 8; k++)
        printf("%s %s\n", names[k], found[k] ? "found" : "missing");
    fflush(stdout);
    ExitProcess(0);
    return EXCEPTION_CONTINUE_SEARCH;
}

int main(void) {
    step all[8] = {d0, d1, d2, d3, d4, d5, d6, d7};
    for (int k = 0; k < 8; k++)
        chain[k] = all[k];
    AddVectoredExceptionHandler(1, handler);
    printf("%d\n", d0(nowhere, 0));
    return 1;
}
