/* Calls in tail position, run rather than read.
 *
 * Design: spec/optimizer/25-tail-calls.md section 25.2. The pass this is about is
 * crates/rucc-codegen/src/tail.rs, which turns `return f(x)` into the epilogue and a jump to `f` at
 * -O2 and -Os. Its own tests say which calls it takes. This file is compiled by this compiler and by
 * the system compiler, run, and the digests held against each other, so what is checked is the code
 * that came out.
 *
 * Every function here is noinline, so that the call is still a call when the back end sees it. The
 * shapes are the ones that can go wrong: arguments that trade registers on the way to the callee,
 * values in registers the epilogue puts back, floating point, a pair that comes back in two
 * registers, narrow returns, a variadic callee, arguments that arrived on the stack, and the cases
 * that must stay calls: a local whose address the callee is handed, more arguments than there are
 * registers, and a call through a pointer.
 *
 * Run with an argument, it walks a chain of ten million calls between two functions instead, which
 * only fits in a small stack when each of them is a jump. The script runs that at -O2 under a one
 * megabyte stack.
 */

int printf(const char *format, ...);

#define NOINLINE __attribute__((noinline))

/* How many rounds each group runs, and how many cases in one. */
#define ROUNDS 4
#define PER_ROUND 4096

/* How many cases ran, printed at the end so that a run that stopped early is a different thing from
 * a run that disagreed.
 */
static long cases = 0;

/* xorshift64, seeded away from the one state it cannot leave. */
static unsigned long long state = 0x9E3779B97F4A7C15ull;

static unsigned long long next_random(void) {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    return state;
}

static unsigned long long mix(unsigned long long digest, unsigned long long value) {
    return (digest ^ value) * 1099511628211ull;
}

static void report(const char *what, int round, unsigned long long digest) {
    printf("%s %d %llu\n", what, round, digest);
}

/* Something a callee can write, so that a function returning nothing has an effect to check. */
static volatile unsigned long long sink;

/* One argument in, one answer out. */
NOINLINE int twice_plus(int x) { return 2 * x + 1; }
NOINLINE int one(int x) { return twice_plus(x ^ 0x5a5a); }

/* Six arguments that come out in the other order, which is every register trading with another. */
NOINLINE long six(long a, long b, long c, long d, long e, long f) {
    return a * 3 + b * 5 + c * 7 + d * 11 + e * 13 + f * 17;
}
NOINLINE long reversed(long a, long b, long c, long d, long e, long f) {
    return six(f, e, d, c, b, a);
}

/* Values that live across a call of their own, so they are in registers the epilogue puts back,
 * and then all of them go into the tail call.
 */
NOINLINE long spread(long x) { return x * 2654435761u; }
NOINLINE long held(long a, long b, long c) {
    long p = spread(a), q = spread(b), r = spread(c);
    long s = spread(p ^ q), t = spread(q ^ r);
    return six(p, q, r, s, t, a + b + c);
}

/* Floating point in and out, with the two arguments swapped. */
NOINLINE double weigh(double x, double y) { return x * 0.75 - y * 0.25; }
NOINLINE double swapped(double x, double y) { return weigh(y, x); }

/* Integers and floating point together. */
NOINLINE double blend(int n, double x, long m, double y) { return n * x + m * y; }
NOINLINE double mixed(double x, int n, double y, long m) { return blend(n + 1, y, m - 1, x); }

/* A pair that comes back in two registers. */
struct pair {
    long low;
    long high;
};
NOINLINE struct pair make(long low, long high) {
    struct pair p = {low * 7, high * 9};
    return p;
}
NOINLINE struct pair flipped(long low, long high) { return make(high, low); }

/* Narrow answers, which have to reach the caller's caller at the width it expects. */
NOINLINE signed char narrow(int x) { return (signed char)(x * 37); }
NOINLINE signed char narrow_again(int x) { return narrow(x + 3); }
NOINLINE unsigned short half(unsigned x) { return (unsigned short)(x * 40503u); }
NOINLINE unsigned short half_again(unsigned x) { return half(x ^ 0xffff); }

/* Nothing comes back, and the callee writes something the caller can check. */
NOINLINE void store(unsigned long long x) { sink = mix(sink, x); }
NOINLINE void store_twice(unsigned long long x) {
    sink = mix(sink, x + 1);
    store(x * 3);
}

/* Two tail calls in one function, one down each arm. */
NOINLINE long odd_arm(long x) { return x * 5 + 1; }
NOINLINE long even_arm(long x) { return x / 2; }
NOINLINE long arms(long x) {
    if (x & 1)
        return odd_arm(x);
    return even_arm(x);
}

/* Recursion that calls itself in tail position. */
NOINLINE unsigned long gcd(unsigned long a, unsigned long b) {
    if (b == 0)
        return a;
    return gcd(b, a % b);
}

/* Two functions that call each other in tail position, a thousand deep at most, which fits in any
 * stack whether or not the calls are jumps.
 */
NOINLINE long ping(long n, long acc);
NOINLINE long pong(long n, long acc) {
    if (n <= 0)
        return acc;
    return ping(n - 1, acc * 3 + n);
}
NOINLINE long ping(long n, long acc) {
    if (n <= 0)
        return acc;
    return pong(n - 1, acc ^ (n << 7));
}

/* A variadic callee, which also reads how many vector registers the call used. */
typedef __builtin_va_list va_list;
NOINLINE double total(int count, ...) {
    va_list args;
    __builtin_va_start(args, count);
    double sum = 0;
    for (int i = 0; i < count; i++)
        sum += __builtin_va_arg(args, double);
    __builtin_va_end(args);
    return sum;
}
NOINLINE double three(double a, double b, double c) { return total(3, a, b * 2, c * 3); }

/* Eight arguments, two of which arrived on the stack, and a tail call with only two. */
NOINLINE long pair_sum(long a, long b) { return a * 31 + b; }
NOINLINE long eight(long a, long b, long c, long d, long e, long f, long g, long h) {
    return pair_sum(a + b + c + d, e + f + g + h);
}

/* These have to stay calls. The first hands the callee the address of a local, the second passes
 * more arguments than there are registers, and the third calls through a pointer.
 */
NOINLINE long read_back(const long *p, int n) {
    long sum = 0;
    for (int i = 0; i < n; i++)
        sum = sum * 7 + p[i];
    return sum;
}
NOINLINE long local(long x) {
    long a[4] = {x, x + 1, x * 2, x ^ 99};
    return read_back(a, 4);
}
NOINLINE long seven(long a, long b, long c, long d, long e, long f, long g) {
    return a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g;
}
NOINLINE long wide(long x) { return seven(x, x + 1, x + 2, x + 3, x + 4, x + 5, x + 6); }
static long (*const through)(long) = odd_arm;
NOINLINE long pointer(long x) { return through(x + 7); }

/* Ten million calls between two functions, in tail position. */
NOINLINE long deep_odd(long n, long acc);
NOINLINE long deep_even(long n, long acc) {
    if (n == 0)
        return acc;
    return deep_odd(n - 1, acc + 1);
}
NOINLINE long deep_odd(long n, long acc) {
    if (n == 0)
        return acc;
    return deep_even(n - 1, acc ^ n);
}

static void run(void) {
    for (int round = 0; round < ROUNDS; round++) {
        unsigned long long d[20] = {0};
        for (int i = 0; i < PER_ROUND; i++) {
            unsigned long long r = next_random();
            long a = (long)(r & 0xffffff), b = (long)((r >> 24) & 0xffff), c = (long)(r >> 44);
            double x = (double)(long)(r >> 40) / 3.0, y = (double)(long)(r & 0xfffff) / 7.0;
            d[0] = mix(d[0], (unsigned long long)one((int)r));
            d[1] = mix(d[1], (unsigned long long)reversed(a, b, c, a ^ b, b ^ c, c ^ a));
            d[2] = mix(d[2], (unsigned long long)held(a, b, c));
            d[3] = mix(d[3], (unsigned long long)(long)(swapped(x, y) * 1000));
            d[4] = mix(d[4], (unsigned long long)(long)(mixed(x, (int)b, y, c) * 1000));
            struct pair p = flipped(a, c);
            d[5] = mix(mix(d[5], (unsigned long long)p.low), (unsigned long long)p.high);
            d[6] = mix(d[6], (unsigned long long)(long)narrow_again((int)r));
            d[7] = mix(d[7], (unsigned long long)half_again((unsigned)r));
            store_twice(r);
            d[8] = mix(d[8], sink);
            d[9] = mix(d[9], (unsigned long long)arms((long)(r >> 3)));
            d[10] = mix(d[10], (unsigned long long)gcd(r >> 20, (r & 0xfffff) + 1));
            d[11] = mix(d[11], (unsigned long long)ping((long)(r & 1023), (long)b));
            d[12] = mix(d[12], (unsigned long long)(long)(three(x, y, x - y) * 1000));
            d[13] = mix(d[13], (unsigned long long)eight(a, b, c, a, b, c, a ^ c, b ^ c));
            d[14] = mix(d[14], (unsigned long long)local(a));
            d[15] = mix(d[15], (unsigned long long)wide(b));
            d[16] = mix(d[16], (unsigned long long)pointer(c));
            cases += 17;
        }
        static const char *const names[] = {
            "one",    "reversed", "held", "swapped", "mixed", "pair",  "narrow", "half",  "store",
            "arms",   "gcd",      "ping", "variadic", "eight", "local", "wide",   "pointer",
        };
        for (int g = 0; g < 17; g++)
            report(names[g], round, d[g]);
    }
}

int main(int argc, char **argv) {
    (void)argv;
    if (argc > 1) {
        printf("deep %ld\n", deep_even(10000000, 1));
        return 0;
    }
    run();
    printf("cases %ld\n", cases);
    return 0;
}
