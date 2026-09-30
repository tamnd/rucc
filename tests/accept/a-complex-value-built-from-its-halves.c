/* accept: all */
/* glibc's complex.h defines C11's CMPLX, CMPLXF and CMPLXL as __builtin_complex for gcc 4.7 and
   later, and C11 says each is usable in a static initializer. The halves are the values written,
   so a negative zero and an infinity survive, which x + y * I would not give. gcc takes the
   builtin in every dialect, so this runs under c89 too, and the checks are array sizes rather
   than `_Static_assert` for that reason. */

#define CMPLX(x, y) __builtin_complex ((double) (x), (double) (y))
#define CMPLXF(x, y) __builtin_complex ((float) (x), (float) (y))
#define CMPLXL(x, y) __builtin_complex ((long double) (x), (long double) (y))
#define INFINITY (__builtin_inff ())

static _Complex double z = CMPLX(0.0, INFINITY);
static _Complex float w = CMPLXF(1.0, -0.0);
static _Complex long double l = CMPLXL(2.0, 3.0);

/* The negative zero is kept, the infinity is kept, and the real half beside it is not a nan. */
typedef char negative_zero[__builtin_signbit(__imag__ CMPLX(1.0, -0.0)) ? 1 : -1];
typedef char infinity[__builtin_isinf(__imag__ CMPLX(0.0, INFINITY)) ? 1 : -1];
typedef char real_half[__builtin_isnan(__real__ CMPLX(0.0, INFINITY)) ? -1 : 1];

_Complex double
from(double x, double y)
{
  return CMPLX(x, y) + z + w + l;
}
