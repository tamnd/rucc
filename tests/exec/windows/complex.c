/* Multiplying and dividing complex values, which are calls into the runtime's __mulxc3 family.
 *
 * Every width, because the long double one is where Windows differs: an 80-bit long double goes to
 * a function by the address of a copy, and the call the lowering writes for `z / w` has to do that
 * as well as a call the program wrote. The infinite and the huge operands are the cases the plain
 * formula gets wrong and the routines are there for. Printed as doubles, since msvcrt's printf does
 * not read an 80-bit long double. */
#include <stdio.h>

#define SHOW(what, z) printf("%-10s %g %g\n", what, (double)__real__(z), (double)__imag__(z))

__attribute__((noinline)) static float _Complex fmul(float _Complex a, float _Complex b) { return a * b; }
__attribute__((noinline)) static float _Complex fdiv(float _Complex a, float _Complex b) { return a / b; }
__attribute__((noinline)) static double _Complex dmul(double _Complex a, double _Complex b) { return a * b; }
__attribute__((noinline)) static double _Complex ddiv(double _Complex a, double _Complex b) { return a / b; }
__attribute__((noinline)) static long double _Complex lmul(long double _Complex a, long double _Complex b) { return a * b; }
__attribute__((noinline)) static long double _Complex ldiv_(long double _Complex a, long double _Complex b) { return a / b; }

int main(void) {
    float _Complex f = 3.0f + 4.0fi, g = 1.0f - 2.0fi;
    double _Complex d = 3.0 + 4.0i, e = 1.0 - 2.0i;
    long double _Complex l = 3.0L + 4.0Li, m = 1.0L - 2.0Li;
    SHOW("fmul", fmul(f, g));
    SHOW("fdiv", fdiv(f, g));
    SHOW("dmul", dmul(d, e));
    SHOW("ddiv", ddiv(d, e));
    SHOW("lmul", lmul(l, m));
    SHOW("ldiv", ldiv_(l, m));

    double inf = __builtin_inf();
    double _Complex big = inf + 0.0i, unit = 0.0 + 1.0i;
    double _Complex p = dmul(big, unit);
    printf("inf*i      %d %d\n", __builtin_isinf(__real__ p), __builtin_isinf(__imag__ p));
    double _Complex q = ddiv(1.0 + 1.0i, 0.0 + 0.0i);
    printf("1/0        %d %d\n", __builtin_isinf(__real__ q), __builtin_isinf(__imag__ q));
    double _Complex r = ddiv(1.0 + 1.0i, big);
    SHOW("1/inf", r);
    long double _Complex s = lmul(__builtin_infl() + 0.0Li, 0.0L + 1.0Li);
    printf("linf*i     %d %d\n", __builtin_isinf(__real__ s), __builtin_isinf(__imag__ s));

    double huge = 0x1p1020;
    SHOW("huge", ddiv(huge + huge * 1.0i, huge + huge * 1.0i));
    double tiny = 0x1p-1060;
    SHOW("tiny", ddiv(tiny + tiny * 1.0i, tiny - tiny * 1.0i));
    return 0;
}
