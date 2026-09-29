/* Functions written in the Windows calling convention on x86-64 Linux, called directly, through a
 * pointer whose type says so, and handed across as callbacks in both directions. Every value is
 * printed, so an argument that went to the wrong register shows up as a wrong number. */
#include <stdio.h>

#define EFIAPI __attribute__((ms_abi))

struct pair {
    int a;
    int b;
};

struct wide {
    long a;
    long b;
    long c;
};

/* Four integers, which is every register the Windows convention has for them. */
EFIAPI int four(int a, int b, int c, int d) {
    return a * 1000 + b * 100 + c * 10 + d;
}

/* Seven, so three of them are on the stack above the 32 bytes of shadow space. */
EFIAPI long seven(long a, long b, long c, long d, long e, long f, long g) {
    return a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g;
}

/* Integers and floating point sharing positions, which is what sets this convention apart. */
EFIAPI double mixed(int a, double b, float c, long d, double e, int f) {
    return a + b * 2 + c * 3 + d * 4 + e * 5 + f * 6;
}

EFIAPI float halve(float x) {
    return x / 2;
}

/* A struct of eight bytes travels in a register, and one of twenty-four by reference. */
EFIAPI int small(struct pair p) {
    return p.a - p.b;
}

EFIAPI long big(struct wide w, int scale) {
    return (w.a + w.b + w.c) * scale;
}

EFIAPI struct pair swap(struct pair p) {
    struct pair q = {p.b, p.a};
    return q;
}

EFIAPI struct wide spread(long x) {
    struct wide w = {x, x * 2, x * 3};
    return w;
}

/* The shape UEFI writes its protocol members in. */
typedef long(EFIAPI *combine_fn)(long, long, long, long, long);

EFIAPI long combine(long a, long b, long c, long d, long e) {
    return a - b + c - d + e;
}

/* A plain function that is handed a Windows convention callback and calls it. */
static long drive(combine_fn f, long base) {
    return f(base, 1, 2, 3, 4) + f(4, 3, 2, 1, base);
}

/* The other way round: a Windows convention function handed a plain callback. The doubles stay
 * alive across the calls, and the plain callee is free to write every vector register, so they
 * have to be somewhere it does not reach. */
typedef double (*plain_fn)(double, int);

static double scale(double x, int by) {
    double junk[8];
    for (int i = 0; i < 8; i++) {
        junk[i] = x * i + by;
    }
    return junk[by & 7] + junk[(by + 3) & 7];
}

EFIAPI double gather(plain_fn f, double a, double b, int n) {
    double first = f(a, n);
    double second = f(b, n + 1);
    return first + second + a * b;
}

/* A Windows convention function calling another through a pointer of the same type. */
EFIAPI long twice(combine_fn f, long x) {
    return f(x, x, x, x, x) * 2;
}

/* A pointer to a Windows convention function kept in a struct, which is where UEFI keeps them. */
struct protocol {
    int (EFIAPI *four)(int, int, int, int);
    double (EFIAPI *mixed)(int, double, float, long, double, int);
};

int main(void) {
    printf("four %d\n", four(1, 2, 3, 4));
    printf("seven %ld\n", seven(1, 2, 3, 4, 5, 6, 7));
    printf("mixed %.3f\n", mixed(1, 2.5, 3.25f, 4, 5.5, 6));
    printf("halve %.3f\n", halve(7.0f));
    struct pair p = {10, 3};
    printf("small %d\n", small(p));
    struct wide w = {1, 20, 300};
    printf("big %ld\n", big(w, 3));
    struct pair q = swap(p);
    printf("swap %d %d\n", q.a, q.b);
    struct wide s = spread(7);
    printf("spread %ld %ld %ld\n", s.a, s.b, s.c);
    combine_fn f = combine;
    printf("combine %ld\n", f(10, 20, 30, 40, 50));
    printf("drive %ld\n", drive(combine, 100));
    printf("gather %.3f\n", gather(scale, 1.5, 2.5, 5));
    printf("twice %ld\n", twice(combine, 9));
    struct protocol proto = {four, mixed};
    printf("proto %d %.3f\n", proto.four(4, 3, 2, 1), proto.mixed(6, 5.0, 4.0f, 3, 2.0, 1));
    /* Values live across calls into the Windows convention, in a plain caller. */
    double kept[6] = {1.25, 2.5, 3.75, 5.0, 6.25, 7.5};
    long sum = 0;
    double total = 0;
    for (int i = 0; i < 6; i++) {
        sum += seven(i, i, i, i, i, i, i);
        total += kept[i] * mixed(i, kept[i], (float)i, i, kept[5 - i], i);
    }
    printf("loop %ld %.3f\n", sum, total);
    return 0;
}
