/* tgmath.h, the type generic maths macros.
 *
 * Apple's copy is written for clang, with every function declared again under
 * `__attribute__((overloadable))`, which rucc does not have, and mingw-w64 ships none at all,
 * since gcc brings its own. Meson's C99 check includes it, so a meson build of Postgres on macOS
 * or Windows stopped before it started. On those targets the macros are written here with
 * `_Generic` instead. The type is that of the arguments added together, so the usual arithmetic
 * conversions pick it, an integer argument counts as `double`, and a complex one picks the
 * complex function.
 *
 * Anywhere else the library's own header is used, which already does what its compiler needs.
 */

#if !defined(__APPLE__) && !defined(_WIN32)

#include_next <tgmath.h>

#elif !defined(__RUCC_TGMATH_H)
#define __RUCC_TGMATH_H

#include <math.h>
#include <complex.h>

/* By the type of an expression, a real function or the complex one. */
#define __rucc_tg_rc(e, f) _Generic((e), \
	float: f##f, \
	long double: f##l, \
	_Complex float: c##f##f, \
	_Complex double: c##f, \
	_Complex long double: c##f##l, \
	default: f)

/* By the type of an expression, a real function only. */
#define __rucc_tg_r(e, f) _Generic((e), \
	float: f##f, \
	long double: f##l, \
	default: f)

/* By the type of an expression, a complex function only. */
#define __rucc_tg_c(e, f) _Generic((e), \
	float: f##f, \
	long double: f##l, \
	_Complex float: f##f, \
	_Complex long double: f##l, \
	default: f)

#undef acos
#undef asin
#undef atan
#undef acosh
#undef asinh
#undef atanh
#undef cos
#undef sin
#undef tan
#undef cosh
#undef sinh
#undef tanh
#undef exp
#undef log
#undef pow
#undef sqrt
#undef fabs

#define acos(x) __rucc_tg_rc((x), acos)(x)
#define asin(x) __rucc_tg_rc((x), asin)(x)
#define atan(x) __rucc_tg_rc((x), atan)(x)
#define acosh(x) __rucc_tg_rc((x), acosh)(x)
#define asinh(x) __rucc_tg_rc((x), asinh)(x)
#define atanh(x) __rucc_tg_rc((x), atanh)(x)
#define cos(x) __rucc_tg_rc((x), cos)(x)
#define sin(x) __rucc_tg_rc((x), sin)(x)
#define tan(x) __rucc_tg_rc((x), tan)(x)
#define cosh(x) __rucc_tg_rc((x), cosh)(x)
#define sinh(x) __rucc_tg_rc((x), sinh)(x)
#define tanh(x) __rucc_tg_rc((x), tanh)(x)
#define exp(x) __rucc_tg_rc((x), exp)(x)
#define log(x) __rucc_tg_rc((x), log)(x)
#define pow(x, y) __rucc_tg_rc((x) + (y), pow)((x), (y))
#define sqrt(x) __rucc_tg_rc((x), sqrt)(x)

/* The complex absolute value is cabs, not cfabs. */
#define fabs(x) _Generic((x), \
	float: fabsf, \
	long double: fabsl, \
	_Complex float: cabsf, \
	_Complex double: cabs, \
	_Complex long double: cabsl, \
	default: fabs)(x)

#undef atan2
#undef cbrt
#undef ceil
#undef copysign
#undef erf
#undef erfc
#undef exp2
#undef expm1
#undef fdim
#undef floor
#undef fma
#undef fmax
#undef fmin
#undef fmod
#undef frexp
#undef hypot
#undef ilogb
#undef ldexp
#undef lgamma
#undef llrint
#undef llround
#undef log10
#undef log1p
#undef log2
#undef logb
#undef lrint
#undef lround
#undef nearbyint
#undef nextafter
#undef nexttoward
#undef remainder
#undef remquo
#undef rint
#undef round
#undef scalbn
#undef scalbln
#undef tgamma
#undef trunc

#define atan2(y, x) __rucc_tg_r((y) + (x), atan2)((y), (x))
#define cbrt(x) __rucc_tg_r((x), cbrt)(x)
#define ceil(x) __rucc_tg_r((x), ceil)(x)
#define copysign(x, y) __rucc_tg_r((x) + (y), copysign)((x), (y))
#define erf(x) __rucc_tg_r((x), erf)(x)
#define erfc(x) __rucc_tg_r((x), erfc)(x)
#define exp2(x) __rucc_tg_r((x), exp2)(x)
#define expm1(x) __rucc_tg_r((x), expm1)(x)
#define fdim(x, y) __rucc_tg_r((x) + (y), fdim)((x), (y))
#define floor(x) __rucc_tg_r((x), floor)(x)
#define fma(x, y, z) __rucc_tg_r((x) + (y) + (z), fma)((x), (y), (z))
#define fmax(x, y) __rucc_tg_r((x) + (y), fmax)((x), (y))
#define fmin(x, y) __rucc_tg_r((x) + (y), fmin)((x), (y))
#define fmod(x, y) __rucc_tg_r((x) + (y), fmod)((x), (y))
#define frexp(x, e) __rucc_tg_r((x), frexp)((x), (e))
#define hypot(x, y) __rucc_tg_r((x) + (y), hypot)((x), (y))
#define ilogb(x) __rucc_tg_r((x), ilogb)(x)
#define ldexp(x, e) __rucc_tg_r((x), ldexp)((x), (e))
#define lgamma(x) __rucc_tg_r((x), lgamma)(x)
#define llrint(x) __rucc_tg_r((x), llrint)(x)
#define llround(x) __rucc_tg_r((x), llround)(x)
#define log10(x) __rucc_tg_r((x), log10)(x)
#define log1p(x) __rucc_tg_r((x), log1p)(x)
#define log2(x) __rucc_tg_r((x), log2)(x)
#define logb(x) __rucc_tg_r((x), logb)(x)
#define lrint(x) __rucc_tg_r((x), lrint)(x)
#define lround(x) __rucc_tg_r((x), lround)(x)
#define nearbyint(x) __rucc_tg_r((x), nearbyint)(x)
#define nextafter(x, y) __rucc_tg_r((x) + (y), nextafter)((x), (y))
#define nexttoward(x, y) __rucc_tg_r((x), nexttoward)((x), (y))
#define remainder(x, y) __rucc_tg_r((x) + (y), remainder)((x), (y))
#define remquo(x, y, q) __rucc_tg_r((x) + (y), remquo)((x), (y), (q))
#define rint(x) __rucc_tg_r((x), rint)(x)
#define round(x) __rucc_tg_r((x), round)(x)
#define scalbn(x, n) __rucc_tg_r((x), scalbn)((x), (n))
#define scalbln(x, n) __rucc_tg_r((x), scalbln)((x), (n))
#define tgamma(x) __rucc_tg_r((x), tgamma)(x)
#define trunc(x) __rucc_tg_r((x), trunc)(x)

#undef carg
#undef cimag
#undef conj
#undef cproj
#undef creal

#define carg(x) __rucc_tg_c((x), carg)(x)
#define cimag(x) __rucc_tg_c((x), cimag)(x)
#define conj(x) __rucc_tg_c((x), conj)(x)
#define cproj(x) __rucc_tg_c((x), cproj)(x)
#define creal(x) __rucc_tg_c((x), creal)(x)

#endif
