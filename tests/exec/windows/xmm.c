/* XMM6 to XMM15 belong to the caller on Windows x64, unlike every other x86-64 convention.
 *
 * The caller here is an asm block that puts known values in all ten, calls a function that has
 * sixteen doubles live at once so that it needs them, and reads them back. A callee that uses one
 * without saving it changes a value the caller did not change. Document 05.1. */
#include <stdio.h>

static double in[10] = {1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5, 9.5, 10.5};
static double out[10];
static double seen;

__attribute__((noinline, used)) void busy(void) {
    double a[16];
    for (int i = 0; i < 16; i++)
        a[i] = i * 0.25 + seen;
    for (int n = 0; n < 100; n++)
        for (int i = 0; i < 16; i++)
            a[i] = a[i] * 0.5 + a[(i + 1) & 15] * 0.25 + 1.0;
    double s = 0;
    for (int i = 0; i < 16; i++)
        s += a[i];
    seen = s;
}

__attribute__((noinline)) static void check(void) {
    __asm__ volatile(
        "movsd 0(%0), %%xmm6\n\t"
        "movsd 8(%0), %%xmm7\n\t"
        "movsd 16(%0), %%xmm8\n\t"
        "movsd 24(%0), %%xmm9\n\t"
        "movsd 32(%0), %%xmm10\n\t"
        "movsd 40(%0), %%xmm11\n\t"
        "movsd 48(%0), %%xmm12\n\t"
        "movsd 56(%0), %%xmm13\n\t"
        "movsd 64(%0), %%xmm14\n\t"
        "movsd 72(%0), %%xmm15\n\t"
        "mov %%rsp, %%rbx\n\t"
        "and $-16, %%rsp\n\t"
        "sub $32, %%rsp\n\t"
        "call busy\n\t"
        "mov %%rbx, %%rsp\n\t"
        "movsd %%xmm6, 0(%1)\n\t"
        "movsd %%xmm7, 8(%1)\n\t"
        "movsd %%xmm8, 16(%1)\n\t"
        "movsd %%xmm9, 24(%1)\n\t"
        "movsd %%xmm10, 32(%1)\n\t"
        "movsd %%xmm11, 40(%1)\n\t"
        "movsd %%xmm12, 48(%1)\n\t"
        "movsd %%xmm13, 56(%1)\n\t"
        "movsd %%xmm14, 64(%1)\n\t"
        "movsd %%xmm15, 72(%1)\n\t"
        :
        : "r"(in), "r"(out)
        : "rax", "rbx", "rcx", "rdx", "r8", "r9", "r10", "r11", "xmm0", "xmm1", "xmm2", "xmm3",
          "xmm4", "xmm5", "xmm6", "xmm7", "xmm8", "xmm9", "xmm10", "xmm11", "xmm12", "xmm13",
          "xmm14", "xmm15", "memory", "cc");
}

int main(void) {
    check();
    int kept = 0;
    for (int i = 0; i < 10; i++)
        kept += out[i] == in[i];
    printf("busy %.4f\n", seen);
    printf("xmm6 to xmm15 kept %d of 10\n", kept);
    return 0;
}
