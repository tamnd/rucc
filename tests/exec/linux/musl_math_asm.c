/* The inline assembly of musl's src/math/x86_64, one function for each form it writes: "x" on
 * double and float, "=r" from "x", and the x87 "t" and "u" with an operand that is not on the x87
 * stack beside them. */
#include <stdio.h>

static double sqrt_x(double x) {
    __asm__("sqrtsd %1, %0" : "=x"(x) : "x"(x));
    return x;
}

static float sqrtf_x(float x) {
    __asm__("sqrtss %1, %0" : "=x"(x) : "x"(x));
    return x;
}

static double fabs_x(double x) {
    double t;
    __asm__("pcmpeqd %0, %0" : "=x"(t));
    __asm__("psrlq   $1, %0" : "+x"(t));
    __asm__("andps   %1, %0" : "+x"(x) : "x"(t));
    return x;
}

static float fabsf_x(float x) {
    float t;
    __asm__("pcmpeqd %0, %0" : "=x"(t));
    __asm__("psrld   $1, %0" : "+x"(t));
    __asm__("andps   %1, %0" : "+x"(x) : "x"(t));
    return x;
}

static long lrint_x(double x) {
    long r;
    __asm__("cvtsd2si %1, %0" : "=r"(r) : "x"(x));
    return r;
}

static long lrintf_x(float x) {
    long r;
    __asm__("cvtss2si %1, %0" : "=r"(r) : "x"(x));
    return r;
}

static long double fmodl_x(long double x, long double y) {
    unsigned short fpsr;
    do __asm__("fprem; fnstsw %%ax" : "+t"(x), "=a"(fpsr) : "u"(y));
    while (fpsr & 0x400);
    return x;
}

static long double remainderl_x(long double x, long double y) {
    unsigned short fpsr;
    do __asm__("fprem1; fnstsw %%ax" : "+t"(x), "=a"(fpsr) : "u"(y));
    while (fpsr & 0x400);
    return x;
}

static long long llrintl_x(long double x) {
    long long r;
    __asm__("fistpll %0" : "=m"(r) : "t"(x) : "st");
    return r;
}

static long double sqrtl_x(long double x) {
    __asm__("fsqrt" : "+t"(x));
    return x;
}

int main(void) {
    printf("%.17g %.9g\n", sqrt_x(2.0), (double)sqrtf_x(2.0f));
    printf("%g %g %g\n", fabs_x(-3.5), (double)fabsf_x(-0.25f), fabs_x(7.0));
    printf("%ld %ld %ld\n", lrint_x(2.5), lrint_x(-3.7), lrintf_x(9.5f));
    printf("%Lg %Lg\n", fmodl_x(10.5L, 3.0L), fmodl_x(-1e20L, 7.0L));
    printf("%Lg %Lg\n", remainderl_x(10.5L, 3.0L), remainderl_x(11.0L, 4.0L));
    printf("%lld %lld\n", llrintl_x(123456789012.5L), llrintl_x(-2.5L));
    printf("%.20Lg\n", sqrtl_x(2.0L));
    return 0;
}
