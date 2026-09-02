/* float.h, the shape of the floating point formats.
 *
 * The compiler already knows all of this, because it has to fold a constant of each format,
 * and it says so in the `__FLT_`, `__DBL_` and `__LDBL_` macros. This header is those macros
 * under the names the standard gives them, and nothing else. There is no chain to a system
 * header: the format is the target's and the library has no say in it.
 *
 * The `_TRUE_MIN` names are C11's and are the smallest subnormal, which is not `_MIN`. The
 * compiler spells that one `DENORM_MIN`, which was the C99 name for it. */

#ifndef __RUCC_FLOAT_H
#define __RUCC_FLOAT_H

#undef FLT_RADIX
#define FLT_RADIX __FLT_RADIX__

#undef FLT_MANT_DIG
#undef FLT_DIG
#undef FLT_MIN_EXP
#undef FLT_MIN_10_EXP
#undef FLT_MAX_EXP
#undef FLT_MAX_10_EXP
#undef FLT_MAX
#undef FLT_MIN
#undef FLT_EPSILON
#define FLT_MANT_DIG __FLT_MANT_DIG__
#define FLT_DIG __FLT_DIG__
#define FLT_MIN_EXP __FLT_MIN_EXP__
#define FLT_MIN_10_EXP __FLT_MIN_10_EXP__
#define FLT_MAX_EXP __FLT_MAX_EXP__
#define FLT_MAX_10_EXP __FLT_MAX_10_EXP__
#define FLT_MAX __FLT_MAX__
#define FLT_MIN __FLT_MIN__
#define FLT_EPSILON __FLT_EPSILON__

#undef DBL_MANT_DIG
#undef DBL_DIG
#undef DBL_MIN_EXP
#undef DBL_MIN_10_EXP
#undef DBL_MAX_EXP
#undef DBL_MAX_10_EXP
#undef DBL_MAX
#undef DBL_MIN
#undef DBL_EPSILON
#define DBL_MANT_DIG __DBL_MANT_DIG__
#define DBL_DIG __DBL_DIG__
#define DBL_MIN_EXP __DBL_MIN_EXP__
#define DBL_MIN_10_EXP __DBL_MIN_10_EXP__
#define DBL_MAX_EXP __DBL_MAX_EXP__
#define DBL_MAX_10_EXP __DBL_MAX_10_EXP__
#define DBL_MAX __DBL_MAX__
#define DBL_MIN __DBL_MIN__
#define DBL_EPSILON __DBL_EPSILON__

#undef LDBL_MANT_DIG
#undef LDBL_DIG
#undef LDBL_MIN_EXP
#undef LDBL_MIN_10_EXP
#undef LDBL_MAX_EXP
#undef LDBL_MAX_10_EXP
#undef LDBL_MAX
#undef LDBL_MIN
#undef LDBL_EPSILON
#define LDBL_MANT_DIG __LDBL_MANT_DIG__
#define LDBL_DIG __LDBL_DIG__
#define LDBL_MIN_EXP __LDBL_MIN_EXP__
#define LDBL_MIN_10_EXP __LDBL_MIN_10_EXP__
#define LDBL_MAX_EXP __LDBL_MAX_EXP__
#define LDBL_MAX_10_EXP __LDBL_MAX_10_EXP__
#define LDBL_MAX __LDBL_MAX__
#define LDBL_MIN __LDBL_MIN__
#define LDBL_EPSILON __LDBL_EPSILON__

/* One, meaning round to nearest. This compiler does not change the rounding mode and does
 * not fold as if it had been changed. A program that changes it at run time with
 * `<fenv.h>` is reading the wrong constant here, which is what every implementation that
 * defines this as a constant also does. */
#undef FLT_ROUNDS
#define FLT_ROUNDS 1

/* Whether the arithmetic is done wider than the operands. Zero on every target here, since
 * none of them evaluates `float` in `double`. */
#undef FLT_EVAL_METHOD
#define FLT_EVAL_METHOD __FLT_EVAL_METHOD__

#if !defined(__STDC_VERSION__) || __STDC_VERSION__ >= 199901L
#undef DECIMAL_DIG
#define DECIMAL_DIG __DECIMAL_DIG__

#undef FLT_DECIMAL_DIG
#undef FLT_TRUE_MIN
#undef FLT_HAS_SUBNORM
#undef FLT_NORM_MAX
#undef FLT_IS_IEC_60559
#define FLT_DECIMAL_DIG __FLT_DECIMAL_DIG__
#define FLT_TRUE_MIN __FLT_DENORM_MIN__
#define FLT_HAS_SUBNORM __FLT_HAS_DENORM__
#define FLT_NORM_MAX __FLT_NORM_MAX__
#define FLT_IS_IEC_60559 __FLT_IS_IEC_60559__

#undef DBL_DECIMAL_DIG
#undef DBL_TRUE_MIN
#undef DBL_HAS_SUBNORM
#undef DBL_NORM_MAX
#undef DBL_IS_IEC_60559
#define DBL_DECIMAL_DIG __DBL_DECIMAL_DIG__
#define DBL_TRUE_MIN __DBL_DENORM_MIN__
#define DBL_HAS_SUBNORM __DBL_HAS_DENORM__
#define DBL_NORM_MAX __DBL_NORM_MAX__
#define DBL_IS_IEC_60559 __DBL_IS_IEC_60559__

#undef LDBL_DECIMAL_DIG
#undef LDBL_TRUE_MIN
#undef LDBL_HAS_SUBNORM
#undef LDBL_NORM_MAX
#undef LDBL_IS_IEC_60559
#define LDBL_DECIMAL_DIG __LDBL_DECIMAL_DIG__
#define LDBL_TRUE_MIN __LDBL_DENORM_MIN__
#define LDBL_HAS_SUBNORM __LDBL_HAS_DENORM__
#define LDBL_NORM_MAX __LDBL_NORM_MAX__
#define LDBL_IS_IEC_60559 __LDBL_IS_IEC_60559__
#endif

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
#undef FLT_HAS_INFINITY
#undef DBL_HAS_INFINITY
#undef LDBL_HAS_INFINITY
#undef FLT_HAS_QUIET_NAN
#undef DBL_HAS_QUIET_NAN
#undef LDBL_HAS_QUIET_NAN
#define FLT_HAS_INFINITY __FLT_HAS_INFINITY__
#define DBL_HAS_INFINITY __DBL_HAS_INFINITY__
#define LDBL_HAS_INFINITY __LDBL_HAS_INFINITY__
#define FLT_HAS_QUIET_NAN __FLT_HAS_QUIET_NAN__
#define DBL_HAS_QUIET_NAN __DBL_HAS_QUIET_NAN__
#define LDBL_HAS_QUIET_NAN __LDBL_HAS_QUIET_NAN__
#endif

#endif
