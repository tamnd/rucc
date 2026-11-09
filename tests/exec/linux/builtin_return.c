/* __builtin_return, which leaves a function with what a __builtin_apply came back with as its
 * answer. Each forwarder passes its arguments on to a function of its own type and gives back what
 * that one answered, the way libobjc's sendmsg.c does: a word, a double, a float, a structure in
 * one register of each file, one in a vector register, one that goes back in memory, and nothing. */
#include <stdio.h>

struct pair {
    long a;
    double b;
};

struct two {
    float x, y;
};

struct big {
    long w[5];
};

static int seen;

__attribute__((noinline)) static long add(long a, long b) { return a + b; }

__attribute__((noinline)) static double half(double x, int n) { return x / 2 + n; }

__attribute__((noinline)) static float triple(float x) { return x * 3; }

__attribute__((noinline)) static struct pair both(long a, double b) {
    struct pair p = {a * 2, b * 3};
    return p;
}

__attribute__((noinline)) static struct two moved(float x, float y) {
    struct two t = {x + 1, y + 2};
    return t;
}

__attribute__((noinline)) static struct big many(long x) {
    struct big b;
    for (int i = 0; i < 5; i++)
        b.w[i] = x + i;
    return b;
}

__attribute__((noinline)) static void note(int x) { seen = x; }

#define FORWARD(to) __builtin_return(__builtin_apply((void (*)())(to), __builtin_apply_args(), 0))

__attribute__((noinline)) static long to_add(long a, long b) {
    (void)a, (void)b;
    FORWARD(add);
}

__attribute__((noinline)) static double to_half(double x, int n) {
    (void)x, (void)n;
    FORWARD(half);
}

__attribute__((noinline)) static float to_triple(float x) {
    (void)x;
    FORWARD(triple);
}

__attribute__((noinline)) static struct pair to_both(long a, double b) {
    (void)a, (void)b;
    FORWARD(both);
}

__attribute__((noinline)) static struct two to_moved(float x, float y) {
    (void)x, (void)y;
    FORWARD(moved);
}

__attribute__((noinline)) static struct big to_many(long x) {
    (void)x;
    FORWARD(many);
}

__attribute__((noinline)) static void to_note(int x) {
    (void)x;
    FORWARD(note);
}

int main(void) {
    printf("%ld\n", to_add(3, 4));
    printf("%g\n", to_half(5.0, 2));
    printf("%g\n", to_triple(2.5f));
    struct pair p = to_both(4, 1.5);
    printf("%ld %g\n", p.a, p.b);
    struct two t = to_moved(0.5f, 0.25f);
    printf("%g %g\n", t.x, t.y);
    struct big b = to_many(10);
    printf("%ld %ld %ld\n", b.w[0], b.w[2], b.w[4]);
    to_note(9);
    printf("%d\n", seen);
    return 0;
}
