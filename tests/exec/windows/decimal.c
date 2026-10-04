/* x86_64 only: gcc has decimal floating point on x86-64 and not on the other Windows targets */
/* not on msvc: Microsoft's compiler has no decimal types and its CRT no routines for them */
/* _Decimal32, _Decimal64 and _Decimal128 passed, returned and added up on Windows x64.
 *
 * gcc passes the two narrow ones in the integer registers by position and returns them in rax,
 * and the wide one by reference with the result through a hidden pointer, which is not where the
 * System V ABI puts any of the three. The arithmetic is libgcc's __bid_* routines. A float or a
 * double in among them takes the next position the way it would anywhere on Windows x64. */
#include <stdio.h>

static _Decimal64 add64(_Decimal64 a, _Decimal64 b) { return a + b; }
static _Decimal32 mul32(_Decimal32 a, _Decimal32 b) { return a * b; }
static _Decimal128 sub128(_Decimal128 a, _Decimal128 b) { return a - b; }

__attribute__((noinline)) static _Decimal64 mixed(int i, _Decimal64 d, double f, _Decimal32 s)
{
    return d + (_Decimal64)i + (_Decimal64)f + (_Decimal64)s;
}

__attribute__((noinline)) static _Decimal64 many(_Decimal64 a, _Decimal64 b, _Decimal64 c,
                                                 _Decimal64 d, _Decimal64 e, _Decimal64 f)
{
    return a + b * 10 + c * 100 + d * 1000 + e * 10000 + f * 100000;
}

static long long whole(_Decimal64 d) { return (long long)d; }

int main(void)
{
    _Decimal64 x = 1.1DD, y = 2.2DD;
    /* Exact in decimal, which is the point: 1.1 + 2.2 is 3.3 here and not in binary. */
    printf("%d\n", add64(x, y) == 3.3DD);
    printf("%d\n", mul32(1.5DF, 4.0DF) == 6.0DF);
    printf("%d\n", sub128(10.25DL, 0.05DL) == 10.2DL);
    printf("%lld\n", whole(mixed(3, 4.5DD, 2.5, 0.0DF)));
    printf("%lld\n", whole(many(1, 2, 3, 4, 5, 6)));
    _Decimal64 sum = 0;
    for (int i = 0; i < 10; i++)
        sum += 0.1DD;
    printf("%d\n", sum == 1.0DD);
    printf("%d\n", (int)(sizeof(_Decimal32) + sizeof(_Decimal64) + sizeof(_Decimal128)));
    return 0;
}
