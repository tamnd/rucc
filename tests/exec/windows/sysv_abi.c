/* Functions written in the System V calling convention on 64-bit Windows, called directly, through
 * a pointer whose type says so, and handed across as callbacks in both directions. Every value is
 * printed, so an argument that went to the wrong register shows up as a wrong number. */
#include <stdio.h>

#define SYSV __attribute__((sysv_abi))

struct pair {
    int a;
    int b;
};

struct wide {
    long long a;
    long long b;
    long long c;
};

/* Four integers, which is fewer than the six registers this convention has for them. */
SYSV int four(int a, int b, int c, int d) {
    return a * 1000 + b * 100 + c * 10 + d;
}

/* Seven, so one of them is on the stack, with no shadow space below it. */
SYSV long long seven(long long a, long long b, long long c, long long d, long long e, long long f, long long g) {
    return a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g;
}

/* Integers and floating point counted separately, which is what sets this convention apart. */
SYSV double mixed(int a, double b, float c, long long d, double e, int f) {
    return a + b * 2 + c * 3 + d * 4 + e * 5 + f * 6;
}

SYSV float halve(float x) {
    return x / 2;
}

/* A struct of eight bytes travels in a register, and one of twenty-four on the stack. */
SYSV int small(struct pair p) {
    return p.a - p.b;
}

SYSV long long big(struct wide w, int scale) {
    return (w.a + w.b + w.c) * scale;
}

SYSV struct pair swap(struct pair p) {
    struct pair q = {p.b, p.a};
    return q;
}

SYSV struct wide spread(long long x) {
    struct wide w = {x, x * 2, x * 3};
    return w;
}

/* A pointer type that carries the convention, the way a UEFI typedef does. */
typedef long long(SYSV *combine_fn)(long long, long long, long long, long long, long long);

SYSV long long combine(long long a, long long b, long long c, long long d, long long e) {
    return a - b + c - d + e;
}

/* A plain function that is handed a System V callback and calls it. */
static long long drive(combine_fn f, long long base) {
    return f(base, 1, 2, 3, 4) + f(4, 3, 2, 1, base);
}

/* The other way round: a System V function handed a plain callback. The plain callee keeps
 * rsi, rdi and xmm6 to xmm15, which the System V function does not owe its own caller. */
typedef double (*plain_fn)(double, int);

static double scale(double x, int by) {
    double junk[8];
    for (int i = 0; i < 8; i++) {
        junk[i] = x * i + by;
    }
    return junk[by & 7] + junk[(by + 3) & 7];
}

SYSV double gather(plain_fn f, double a, double b, int n) {
    double first = f(a, n);
    double second = f(b, n + 1);
    return first + second + a * b;
}

/* A System V function calling another through a pointer of the same type. */
SYSV long long twice(combine_fn f, long long x) {
    return f(x, x, x, x, x) * 2;
}

/* Pointers to System V functions kept in a struct. */
struct protocol {
    int (SYSV *four)(int, int, int, int);
    double (SYSV *mixed)(int, double, float, long long, double, int);
};

int main(void) {
    printf("four %d\n", four(1, 2, 3, 4));
    printf("seven %lld\n", seven(1, 2, 3, 4, 5, 6, 7));
    printf("mixed %.3f\n", mixed(1, 2.5, 3.25f, 4, 5.5, 6));
    printf("halve %.3f\n", halve(7.0f));
    struct pair p = {10, 3};
    printf("small %d\n", small(p));
    struct wide w = {1, 20, 300};
    printf("big %lld\n", big(w, 3));
    struct pair q = swap(p);
    printf("swap %d %d\n", q.a, q.b);
    struct wide s = spread(7);
    printf("spread %lld %lld %lld\n", s.a, s.b, s.c);
    combine_fn f = combine;
    printf("combine %lld\n", f(10, 20, 30, 40, 50));
    printf("drive %lld\n", drive(combine, 100));
    printf("gather %.3f\n", gather(scale, 1.5, 2.5, 5));
    printf("twice %lld\n", twice(combine, 9));
    struct protocol proto = {four, mixed};
    printf("proto %d %.3f\n", proto.four(4, 3, 2, 1), proto.mixed(6, 5.0, 4.0f, 3, 2.0, 1));
    /* Values live across calls into System V functions, which keep none of the vector registers, in a
     * plain caller that has to keep them somewhere. */
    double kept[6] = {1.25, 2.5, 3.75, 5.0, 6.25, 7.5};
    long long sum = 0;
    double total = 0;
    for (int i = 0; i < 6; i++) {
        sum += seven(i, i, i, i, i, i, i);
        total += kept[i] * mixed(i, kept[i], (float)i, i, kept[5 - i], i);
    }
    printf("loop %lld %.3f\n", sum, total);
    return 0;
}
