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

/* The decimal types, which are there only where the compiler says so with __DEC32_MANT_DIG__. The
   conditions are gcc's, so a program that asks for these the way it would ask gcc gets them. */
#ifdef __DEC32_MANT_DIG__
#if defined(__STDC_WANT_DEC_FP__) || defined(__STDC_WANT_IEC_60559_DFP_EXT__) || \
    (defined(__STDC_VERSION__) && __STDC_VERSION__ > 201710L)
#undef DEC32_MANT_DIG
#undef DEC64_MANT_DIG
#undef DEC128_MANT_DIG
#undef DEC32_MIN_EXP
#undef DEC64_MIN_EXP
#undef DEC128_MIN_EXP
#undef DEC32_MAX_EXP
#undef DEC64_MAX_EXP
#undef DEC128_MAX_EXP
#undef DEC32_MAX
#undef DEC64_MAX
#undef DEC128_MAX
#undef DEC32_EPSILON
#undef DEC64_EPSILON
#undef DEC128_EPSILON
#undef DEC32_MIN
#undef DEC64_MIN
#undef DEC128_MIN
#undef DEC_EVAL_METHOD
#define DEC32_MANT_DIG __DEC32_MANT_DIG__
#define DEC64_MANT_DIG __DEC64_MANT_DIG__
#define DEC128_MANT_DIG __DEC128_MANT_DIG__
#define DEC32_MIN_EXP __DEC32_MIN_EXP__
#define DEC64_MIN_EXP __DEC64_MIN_EXP__
#define DEC128_MIN_EXP __DEC128_MIN_EXP__
#define DEC32_MAX_EXP __DEC32_MAX_EXP__
#define DEC64_MAX_EXP __DEC64_MAX_EXP__
#define DEC128_MAX_EXP __DEC128_MAX_EXP__
#define DEC32_MAX __DEC32_MAX__
#define DEC64_MAX __DEC64_MAX__
#define DEC128_MAX __DEC128_MAX__
#define DEC32_EPSILON __DEC32_EPSILON__
#define DEC64_EPSILON __DEC64_EPSILON__
#define DEC128_EPSILON __DEC128_EPSILON__
#define DEC32_MIN __DEC32_MIN__
#define DEC64_MIN __DEC64_MIN__
#define DEC128_MIN __DEC128_MIN__
#define DEC_EVAL_METHOD __DEC_EVAL_METHOD__
#endif

#ifdef __STDC_WANT_DEC_FP__
#undef DEC32_SUBNORMAL_MIN
#undef DEC64_SUBNORMAL_MIN
#undef DEC128_SUBNORMAL_MIN
#define DEC32_SUBNORMAL_MIN __DEC32_SUBNORMAL_MIN__
#define DEC64_SUBNORMAL_MIN __DEC64_SUBNORMAL_MIN__
#define DEC128_SUBNORMAL_MIN __DEC128_SUBNORMAL_MIN__
#endif

#if defined(__STDC_WANT_IEC_60559_DFP_EXT__) || \
    (defined(__STDC_VERSION__) && __STDC_VERSION__ > 201710L)
#undef DEC32_TRUE_MIN
#undef DEC64_TRUE_MIN
#undef DEC128_TRUE_MIN
#define DEC32_TRUE_MIN __DEC32_SUBNORMAL_MIN__
#define DEC64_TRUE_MIN __DEC64_SUBNORMAL_MIN__
#define DEC128_TRUE_MIN __DEC128_SUBNORMAL_MIN__
#endif

#if defined(__STDC_VERSION__) && __STDC_VERSION__ > 201710L
#undef DEC_INFINITY
#undef DEC_NAN
#undef DEC32_SNAN
#undef DEC64_SNAN
#undef DEC128_SNAN
#define DEC_INFINITY (__builtin_infd32())
#define DEC_NAN (__builtin_nand32(""))
#define DEC32_SNAN (__builtin_nansd32(""))
#define DEC64_SNAN (__builtin_nansd64(""))
#define DEC128_SNAN (__builtin_nansd128(""))
#endif
#endif

#endif
