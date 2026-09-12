/* emmintrin.h, the SSE2 intrinsics.
 *
 * The last and largest of the three vector headers, and the one real programs actually reach for.
 * `<mmintrin.h>` is the eight byte vector, `<xmmintrin.h>` is the sixteen byte vector of four
 * floats, and this is the same sixteen bytes read two other ways: as two doubles, spelled
 * `__m128d`, and as integers of any of the four widths, spelled `__m128i`. It includes
 * `<xmmintrin.h>`, which includes the other two, so a program that includes this one alone gets
 * the whole family.
 *
 * Two hundred and thirty five of the two hundred and thirty seven names gcc 16.2.0 has are here.
 * The two that are not are `_mm_sqrt_pd` and `_mm_sqrt_sd`, for the reason `<xmmintrin.h>` gives
 * about the four square roots it leaves out: a correctly rounded square root is an instruction,
 * software can approximate one but cannot cheaply be exactly right for every input, and a result
 * that is off by one unit in the last place for some inputs is a wrong answer nobody sees where
 * an absent name is a diagnostic. `tamnd/rucc#1157` is the square root instruction and the two
 * arrive with it.
 *
 * # These are C, not instructions
 *
 * Every function below is vector arithmetic, a subscript that picks a lane, or a brace
 * initializer that builds one. What a program computes is what gcc computes, checked lane by
 * lane. What it compiles to is not the instruction the name is short for, because vectors come
 * apart into their lanes before the back end sees them, so `_mm_add_epi32` is four additions
 * rather than one `paddd`. `tamnd/rucc#200` is vectors that stay in registers, and when it lands
 * these stop being the slow way round without a line of this file changing.
 *
 * # Why `static __inline__` and not what GCC writes
 *
 * gcc writes `extern __inline` with `__gnu_inline__`, which asks for a definition inlined
 * everywhere and emitted nowhere. This compiler honours the second half and not yet the first, so
 * a call would be left behind as a reference to a symbol no object file defines. `tamnd/rucc#1149`
 * is the work that makes gcc's own spelling behave. */

#ifndef __RUCC_EMMINTRIN_H
#define __RUCC_EMMINTRIN_H

/* The rung below, which brings `<mmintrin.h>` and `<mm_malloc.h>` with it. */
#include <xmmintrin.h>

/* The lane types this header adds. The four float spellings are in `<xmmintrin.h>` and are not
 * repeated here, since a typedef written twice is not something every mode of C accepts.
 *
 * Which spelling a function uses is not decoration. A comparison of bytes has to say whether the
 * bytes are signed, a shift right has to say whether it brings in zeros or the sign, and a sum of
 * two lanes near the top of their range has to be worked out somewhere it cannot overflow. Each
 * one below picks the spelling that makes the operation mean what the instruction means. */
typedef double __v2df __attribute__((__vector_size__(16)));
typedef long long __v2di __attribute__((__vector_size__(16)));
typedef unsigned long long __v2du __attribute__((__vector_size__(16)));
typedef short __v8hi __attribute__((__vector_size__(16)));
typedef unsigned short __v8hu __attribute__((__vector_size__(16)));
typedef char __v16qi __attribute__((__vector_size__(16)));
typedef unsigned char __v16qu __attribute__((__vector_size__(16)));

/* The two types a program names. `__may_alias__` because a program is allowed to point one of
 * these at bytes it also reads as something else, which is what every load below does. */
typedef long long __m128i __attribute__((__vector_size__(16), __may_alias__));
typedef double __m128d __attribute__((__vector_size__(16), __may_alias__));

/* The same two with no alignment, which is how the unaligned loads and stores are written. */
typedef long long __m128i_u __attribute__((__vector_size__(16), __may_alias__, __aligned__(1)));
typedef double __m128d_u __attribute__((__vector_size__(16), __may_alias__, __aligned__(1)));

/* # Rounding
 *
 * The double precision twin of the helper in `<xmmintrin.h>`. Adding two to the fifty second and
 * taking it away again forces every bit below the point off the end of the significand, and the
 * rounding that happens when it goes off the end is the hardware's under whatever mode the
 * program set, which is how this follows a rounding mode it cannot read. Values already that
 * large are integers and come back untouched, which is also what leaves infinities and nans
 * alone. The sign comes off first and goes back after so that a negative value rounding to zero
 * gives a negative zero, which is what the instruction gives. */
static __inline__ double __attribute__((__always_inline__)) __rucc_round_to_integral_d(double __x)
{
  double __magic = 4503599627370496.0;
  double __size = __x < 0.0 ? -__x : __x;
  double __answer;
  if (!(__size < __magic))
    return __x;
  __answer = (__size + __magic) - __magic;
  return __x < 0.0 ? -__answer : __answer;
}

/* A rounded double as an int, with the answer the machine gives when it does not fit.
 *
 * `cvtsd2si` produces the integer indefinite value, which is the most negative int, for a nan, an
 * infinity or anything out of range, where a cast in C is undefined behaviour. The range is
 * tested first so the machine's answer is what comes out. The comparisons are written with `!` in
 * front so that a nan, which compares false against everything, takes the out of range path. */
static __inline__ int __attribute__((__always_inline__)) __rucc_double_to_int(double __x)
{
  double __rounded = __rucc_round_to_integral_d(__x);
  if (!(__rounded >= -2147483648.0) || !(__rounded < 2147483648.0))
    return (-2147483647 - 1);
  return (int)__rounded;
}

static __inline__ long long __attribute__((__always_inline__)) __rucc_double_to_long(double __x)
{
  double __rounded = __rucc_round_to_integral_d(__x);
  if (!(__rounded >= -9223372036854775808.0) || !(__rounded < 9223372036854775808.0))
    return (-9223372036854775807LL - 1);
  return (long long)__rounded;
}

/* The truncating pair, where the rounding is the cast's own and only the range needs looking
 * after. The lower bound is the most negative value of the type as a double, which is exact for
 * both widths, and anything below it is a double whose truncation does not fit. */
static __inline__ int __attribute__((__always_inline__)) __rucc_double_to_int_trunc(double __x)
{
  if (!(__x > -2147483649.0) || !(__x < 2147483648.0))
    return (-2147483647 - 1);
  return (int)__x;
}

static __inline__ long long __attribute__((__always_inline__))
__rucc_double_to_long_trunc(double __x)
{
  if (!(__x >= -9223372036854775808.0) || !(__x < 9223372036854775808.0))
    return (-9223372036854775807LL - 1);
  return (long long)__x;
}

/* The four saturating clamps the packing and the saturating arithmetic need are in
 * `<mmintrin.h>`, take an `int` and are the same at both vector widths, so they are used from
 * here rather than written again.
 *
 * # The double precision vector
 *
 * Two doubles. Each operation has a `pd` form that works on both lanes and an `sd` form that
 * works on lane zero and takes lane one from the left operand unchanged. */

static __inline__ __m128d __attribute__((__always_inline__)) _mm_setzero_pd(void)
{
  return (__m128d)(__v2df){ 0.0, 0.0 };
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_undefined_pd(void)
{
  __m128d __answer;
  return __answer;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_add_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2df)__a + (__v2df)__b);
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_add_sd(__m128d __a, __m128d __b)
{
  __v2df __answer = (__v2df)__a;
  __answer[0] = __answer[0] + ((__v2df)__b)[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_sub_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2df)__a - (__v2df)__b);
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_sub_sd(__m128d __a, __m128d __b)
{
  __v2df __answer = (__v2df)__a;
  __answer[0] = __answer[0] - ((__v2df)__b)[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_mul_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2df)__a * (__v2df)__b);
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_mul_sd(__m128d __a, __m128d __b)
{
  __v2df __answer = (__v2df)__a;
  __answer[0] = __answer[0] * ((__v2df)__b)[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_div_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2df)__a / (__v2df)__b);
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_div_sd(__m128d __a, __m128d __b)
{
  __v2df __answer = (__v2df)__a;
  __answer[0] = __answer[0] / ((__v2df)__b)[0];
  return (__m128d)__answer;
}

/* The smaller and the larger of each lane, written as a conditional for the reason `_mm_min_ps`
 * gives: `minpd` answers its second operand whenever the comparison is false, which covers a nan
 * on either side and two zeros of opposite sign, and `fmin` has the opposite rule for a nan. */
static __inline__ __m128d __attribute__((__always_inline__)) _mm_min_pd(__m128d __a, __m128d __b)
{
  __v2df __x = (__v2df)__a, __y = (__v2df)__b;
  return (__m128d)(__v2df){ __x[0] < __y[0] ? __x[0] : __y[0],
                            __x[1] < __y[1] ? __x[1] : __y[1] };
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_min_sd(__m128d __a, __m128d __b)
{
  __v2df __answer = (__v2df)__a;
  double __y = ((__v2df)__b)[0];
  __answer[0] = __answer[0] < __y ? __answer[0] : __y;
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_max_pd(__m128d __a, __m128d __b)
{
  __v2df __x = (__v2df)__a, __y = (__v2df)__b;
  return (__m128d)(__v2df){ __x[0] > __y[0] ? __x[0] : __y[0],
                            __x[1] > __y[1] ? __x[1] : __y[1] };
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_max_sd(__m128d __a, __m128d __b)
{
  __v2df __answer = (__v2df)__a;
  double __y = ((__v2df)__b)[0];
  __answer[0] = __answer[0] > __y ? __answer[0] : __y;
  return (__m128d)__answer;
}

/* The bitwise operations on doubles, done on the same bytes read as integers, which is what the
 * instructions do as well. */

static __inline__ __m128d __attribute__((__always_inline__)) _mm_and_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2du)__a & (__v2du)__b);
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_andnot_pd(__m128d __a, __m128d __b)
{
  return (__m128d)(~(__v2du)__a & (__v2du)__b);
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_or_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2du)__a | (__v2du)__b);
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_xor_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2du)__a ^ (__v2du)__b);
}

/* The comparisons, which answer a mask rather than a truth value. The six spelled with an `n` are
 * the complement of the six without it, which is not the same as the opposite comparison, since
 * `cmpnlt` holds for every pair with a nan in it and `cmpge` does not. `cmpord` holds where
 * neither operand is a nan, written as each being equal to itself. */

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cmpeq_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2di)((__v2df)__a == (__v2df)__b));
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cmplt_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2di)((__v2df)__a < (__v2df)__b));
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cmple_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2di)((__v2df)__a <= (__v2df)__b));
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cmpgt_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2di)((__v2df)__a > (__v2df)__b));
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cmpge_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2di)((__v2df)__a >= (__v2df)__b));
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpneq_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2di)((__v2df)__a != (__v2df)__b));
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpnlt_pd(__m128d __a, __m128d __b)
{
  return (__m128d)(~(__v2di)((__v2df)__a < (__v2df)__b));
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpnle_pd(__m128d __a, __m128d __b)
{
  return (__m128d)(~(__v2di)((__v2df)__a <= (__v2df)__b));
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpngt_pd(__m128d __a, __m128d __b)
{
  return (__m128d)(~(__v2di)((__v2df)__a > (__v2df)__b));
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpnge_pd(__m128d __a, __m128d __b)
{
  return (__m128d)(~(__v2di)((__v2df)__a >= (__v2df)__b));
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpord_pd(__m128d __a, __m128d __b)
{
  return (__m128d)((__v2di)((__v2df)__a == (__v2df)__a) & (__v2di)((__v2df)__b == (__v2df)__b));
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpunord_pd(__m128d __a, __m128d __b)
{
  return (__m128d)(~((__v2di)((__v2df)__a == (__v2df)__a) & (__v2di)((__v2df)__b == (__v2df)__b)));
}

/* The `sd` comparisons, which put the mask in lane zero and leave lane one as it was in the left
 * operand. Lane one is a double and lane zero is now a mask, so the value is assembled through
 * the unsigned integer spelling and cast back once at the end. */

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cmpeq_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  __answer[0] = (unsigned long long)((__v2di)((__v2df)__a == (__v2df)__b))[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cmplt_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  __answer[0] = (unsigned long long)((__v2di)((__v2df)__a < (__v2df)__b))[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cmple_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  __answer[0] = (unsigned long long)((__v2di)((__v2df)__a <= (__v2df)__b))[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cmpgt_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  __answer[0] = (unsigned long long)((__v2di)((__v2df)__a > (__v2df)__b))[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cmpge_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  __answer[0] = (unsigned long long)((__v2di)((__v2df)__a >= (__v2df)__b))[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpneq_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  __answer[0] = (unsigned long long)((__v2di)((__v2df)__a != (__v2df)__b))[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpnlt_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  __answer[0] = (unsigned long long)(~(__v2di)((__v2df)__a < (__v2df)__b))[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpnle_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  __answer[0] = (unsigned long long)(~(__v2di)((__v2df)__a <= (__v2df)__b))[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpngt_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  __answer[0] = (unsigned long long)(~(__v2di)((__v2df)__a > (__v2df)__b))[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpnge_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  __answer[0] = (unsigned long long)(~(__v2di)((__v2df)__a >= (__v2df)__b))[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpord_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  double __x = ((__v2df)__a)[0], __y = ((__v2df)__b)[0];
  __answer[0] = __x == __x && __y == __y ? ~0ULL : 0ULL;
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmpunord_sd(__m128d __a, __m128d __b)
{
  __v2du __answer = (__v2du)__a;
  double __x = ((__v2df)__a)[0], __y = ((__v2df)__b)[0];
  __answer[0] = __x == __x && __y == __y ? 0ULL : ~0ULL;
  return (__m128d)__answer;
}

/* The comparisons that answer an int. The `comi` forms raise the invalid exception for a quiet
 * nan where the `ucomi` forms do not, and the value they produce is the same for every input, so
 * each pair has the same body. Nothing here reads the exception flags, since `_mm_getcsr` is not
 * in `<xmmintrin.h>` for want of an instruction. */

static __inline__ int __attribute__((__always_inline__)) _mm_comieq_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] == ((__v2df)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_comilt_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] < ((__v2df)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_comile_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] <= ((__v2df)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_comigt_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] > ((__v2df)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_comige_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] >= ((__v2df)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_comineq_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] != ((__v2df)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomieq_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] == ((__v2df)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomilt_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] < ((__v2df)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomile_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] <= ((__v2df)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomigt_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] > ((__v2df)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomige_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] >= ((__v2df)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomineq_sd(__m128d __a, __m128d __b)
{
  return ((__v2df)__a)[0] != ((__v2df)__b)[0];
}

/* The comparison that carries its predicate as a number. The same eight SSE has, and the same two
 * differences from gcc `<xmmintrin.h>` describes: this takes a predicate worked out at run time
 * where gcc wants a constant, and answers a mask of zeros for the twenty four AVX predicates
 * where gcc refuses them outright. Both accept more than gcc, so a program that builds under gcc
 * gets the same answers here. */
static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmp_pd(__m128d __a, __m128d __b, const int __predicate)
{
  switch (__predicate & 31)
  {
    case 0:
      return _mm_cmpeq_pd(__a, __b);
    case 1:
      return _mm_cmplt_pd(__a, __b);
    case 2:
      return _mm_cmple_pd(__a, __b);
    case 3:
      return _mm_cmpunord_pd(__a, __b);
    case 4:
      return _mm_cmpneq_pd(__a, __b);
    case 5:
      return _mm_cmpnlt_pd(__a, __b);
    case 6:
      return _mm_cmpnle_pd(__a, __b);
    case 7:
      return _mm_cmpord_pd(__a, __b);
    default:
      return _mm_setzero_pd();
  }
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cmp_sd(__m128d __a, __m128d __b, const int __predicate)
{
  switch (__predicate & 31)
  {
    case 0:
      return _mm_cmpeq_sd(__a, __b);
    case 1:
      return _mm_cmplt_sd(__a, __b);
    case 2:
      return _mm_cmple_sd(__a, __b);
    case 3:
      return _mm_cmpunord_sd(__a, __b);
    case 4:
      return _mm_cmpneq_sd(__a, __b);
    case 5:
      return _mm_cmpnlt_sd(__a, __b);
    case 6:
      return _mm_cmpnle_sd(__a, __b);
    case 7:
      return _mm_cmpord_sd(__a, __b);
    default:
    {
      __v2du __answer = (__v2du)__a;
      __answer[0] = 0ULL;
      return (__m128d)__answer;
    }
  }
}

/* # Building and taking apart a double vector */

/* The arguments highest lane first, which is the order the name is written in and the opposite of
 * the order the lanes are in. `_mm_setr_pd` is the same thing the other way round. */
static __inline__ __m128d __attribute__((__always_inline__)) _mm_set_pd(double __x, double __w)
{
  return (__m128d)(__v2df){ __w, __x };
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_setr_pd(double __w, double __x)
{
  return (__m128d)(__v2df){ __w, __x };
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_set1_pd(double __w)
{
  return (__m128d)(__v2df){ __w, __w };
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_set_pd1(double __w)
{
  return _mm_set1_pd(__w);
}

/* One double in lane zero and a zero above it. */
static __inline__ __m128d __attribute__((__always_inline__)) _mm_set_sd(double __w)
{
  return (__m128d)(__v2df){ __w, 0.0 };
}

/* Lane zero as an ordinary double, which is a read and not a conversion. */
static __inline__ double __attribute__((__always_inline__)) _mm_cvtsd_f64(__m128d __a)
{
  return ((__v2df)__a)[0];
}

/* Lane zero from the second operand and lane one from the first. */
static __inline__ __m128d __attribute__((__always_inline__)) _mm_move_sd(__m128d __a, __m128d __b)
{
  __v2df __answer = (__v2df)__a;
  __answer[0] = ((__v2df)__b)[0];
  return (__m128d)__answer;
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_unpackhi_pd(__m128d __a, __m128d __b)
{
  return (__m128d)(__v2df){ ((__v2df)__a)[1], ((__v2df)__b)[1] };
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_unpacklo_pd(__m128d __a, __m128d __b)
{
  return (__m128d)(__v2df){ ((__v2df)__a)[0], ((__v2df)__b)[0] };
}

/* Two lanes chosen by a selector, one bit each: the low bit picks lane zero out of the first
 * operand and the next bit picks lane one out of the second. */
static __inline__ __m128d __attribute__((__always_inline__))
_mm_shuffle_pd(__m128d __a, __m128d __b, const int __mask)
{
  return (__m128d)(__v2df){ ((__v2df)__a)[__mask & 1], ((__v2df)__b)[(__mask >> 1) & 1] };
}

/* The sign bit of each lane, lane zero in the lowest bit. */
static __inline__ int __attribute__((__always_inline__)) _mm_movemask_pd(__m128d __a)
{
  __v2du __from = (__v2du)__a;
  return (int)((__from[0] >> 63) | ((__from[1] >> 63) << 1));
}

/* # Loads and stores of doubles
 *
 * The aligned forms go through a pointer to `__m128d`, whose alignment is sixteen, and the
 * unaligned forms through `__m128d_u`, whose alignment is one. The type is what says which is
 * which, and handing an unaligned address to an aligned form is what the machine faults on. */

static __inline__ __m128d __attribute__((__always_inline__)) _mm_load_pd(double const *__p)
{
  return *(const __m128d *)__p;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_loadu_pd(double const *__p)
{
  return *(const __m128d_u *)__p;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_load_sd(double const *__p)
{
  return _mm_set_sd(*__p);
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_load1_pd(double const *__p)
{
  return _mm_set1_pd(*__p);
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_load_pd1(double const *__p)
{
  return _mm_load1_pd(__p);
}

/* Two doubles in the opposite order to the one they are in memory. */
static __inline__ __m128d __attribute__((__always_inline__)) _mm_loadr_pd(double const *__p)
{
  __v2df __from = (__v2df)*(const __m128d *)__p;
  return (__m128d)(__v2df){ __from[1], __from[0] };
}

/* One double into lane one, lane zero kept. */
static __inline__ __m128d __attribute__((__always_inline__))
_mm_loadh_pd(__m128d __a, double const *__p)
{
  __v2df __answer = (__v2df)__a;
  __answer[1] = *__p;
  return (__m128d)__answer;
}

/* One double into lane zero, lane one kept. */
static __inline__ __m128d __attribute__((__always_inline__))
_mm_loadl_pd(__m128d __a, double const *__p)
{
  __v2df __answer = (__v2df)__a;
  __answer[0] = *__p;
  return (__m128d)__answer;
}

static __inline__ void __attribute__((__always_inline__)) _mm_store_pd(double *__p, __m128d __a)
{
  *(__m128d *)__p = __a;
}

static __inline__ void __attribute__((__always_inline__)) _mm_storeu_pd(double *__p, __m128d __a)
{
  *(__m128d_u *)__p = __a;
}

static __inline__ void __attribute__((__always_inline__)) _mm_store_sd(double *__p, __m128d __a)
{
  *__p = ((__v2df)__a)[0];
}

static __inline__ void __attribute__((__always_inline__)) _mm_store1_pd(double *__p, __m128d __a)
{
  *(__m128d *)__p = _mm_set1_pd(((__v2df)__a)[0]);
}

static __inline__ void __attribute__((__always_inline__)) _mm_store_pd1(double *__p, __m128d __a)
{
  _mm_store1_pd(__p, __a);
}

static __inline__ void __attribute__((__always_inline__)) _mm_storer_pd(double *__p, __m128d __a)
{
  __v2df __from = (__v2df)__a;
  *(__m128d *)__p = (__m128d)(__v2df){ __from[1], __from[0] };
}

static __inline__ void __attribute__((__always_inline__)) _mm_storeh_pd(double *__p, __m128d __a)
{
  *__p = ((__v2df)__a)[1];
}

static __inline__ void __attribute__((__always_inline__)) _mm_storel_pd(double *__p, __m128d __a)
{
  *__p = ((__v2df)__a)[0];
}

/* # The integer vector
 *
 * The same sixteen bytes read as integers. Which width is a matter of what the operation says
 * rather than of the type, which is why `__m128i` is one type and the operations name their lane
 * width in their own names. */

static __inline__ __m128i __attribute__((__always_inline__)) _mm_setzero_si128(void)
{
  return (__m128i)(__v2di){ 0LL, 0LL };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_undefined_si128(void)
{
  __m128i __answer;
  return __answer;
}

/* The casts, every one of which is a reinterpretation of the same bytes and no work at all. They
 * exist because C will not let one vector type stand in for another without being told. */

static __inline__ __m128 __attribute__((__always_inline__)) _mm_castpd_ps(__m128d __a)
{
  return (__m128)__a;
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_castpd_si128(__m128d __a)
{
  return (__m128i)__a;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_castps_pd(__m128 __a)
{
  return (__m128d)__a;
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_castps_si128(__m128 __a)
{
  return (__m128i)__a;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_castsi128_ps(__m128i __a)
{
  return (__m128)__a;
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_castsi128_pd(__m128i __a)
{
  return (__m128d)__a;
}

/* # Building an integer vector
 *
 * The `set` forms take their arguments highest lane first and the `setr` forms take them lowest
 * lane first, which is the one difference between them. */

static __inline__ __m128i __attribute__((__always_inline__))
_mm_set_epi64x(long long __q1, long long __q0)
{
  return (__m128i)(__v2di){ __q0, __q1 };
}

/* The two halves given as eight byte vectors rather than as integers. `__m64` is itself a vector
 * of one `long long`, so its value comes out through a subscript: C has no cast between a vector
 * and a scalar in either direction, which is the same reason `_mm_movepi64_pi64` below builds a
 * one lane vector by hand instead of casting to one. */
static __inline__ __m128i __attribute__((__always_inline__)) _mm_set_epi64(__m64 __q1, __m64 __q0)
{
  return (__m128i)(__v2di){ ((__v1di)__q0)[0], ((__v1di)__q1)[0] };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_set_epi32(int __q3, int __q2, int __q1, int __q0)
{
  return (__m128i)(__v4si){ __q0, __q1, __q2, __q3 };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_set_epi16(short __q7, short __q6, short __q5, short __q4, short __q3, short __q2, short __q1,
              short __q0)
{
  return (__m128i)(__v8hi){ __q0, __q1, __q2, __q3, __q4, __q5, __q6, __q7 };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_set_epi8(char __q15, char __q14, char __q13, char __q12, char __q11, char __q10, char __q09,
             char __q08, char __q07, char __q06, char __q05, char __q04, char __q03, char __q02,
             char __q01, char __q00)
{
  return (__m128i)(__v16qi){ __q00, __q01, __q02, __q03, __q04, __q05, __q06, __q07,
                             __q08, __q09, __q10, __q11, __q12, __q13, __q14, __q15 };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_setr_epi64(__m64 __q0, __m64 __q1)
{
  return (__m128i)(__v2di){ ((__v1di)__q0)[0], ((__v1di)__q1)[0] };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_setr_epi32(int __q0, int __q1, int __q2, int __q3)
{
  return (__m128i)(__v4si){ __q0, __q1, __q2, __q3 };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_setr_epi16(short __q0, short __q1, short __q2, short __q3, short __q4, short __q5, short __q6,
               short __q7)
{
  return (__m128i)(__v8hi){ __q0, __q1, __q2, __q3, __q4, __q5, __q6, __q7 };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_setr_epi8(char __q00, char __q01, char __q02, char __q03, char __q04, char __q05, char __q06,
              char __q07, char __q08, char __q09, char __q10, char __q11, char __q12, char __q13,
              char __q14, char __q15)
{
  return (__m128i)(__v16qi){ __q00, __q01, __q02, __q03, __q04, __q05, __q06, __q07,
                             __q08, __q09, __q10, __q11, __q12, __q13, __q14, __q15 };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_set1_epi64x(long long __a)
{
  return (__m128i)(__v2di){ __a, __a };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_set1_epi64(__m64 __a)
{
  return _mm_set1_epi64x(((__v1di)__a)[0]);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_set1_epi32(int __a)
{
  return (__m128i)(__v4si){ __a, __a, __a, __a };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_set1_epi16(short __a)
{
  return (__m128i)(__v8hi){ __a, __a, __a, __a, __a, __a, __a, __a };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_set1_epi8(char __a)
{
  return (__m128i)(__v16qi){ __a, __a, __a, __a, __a, __a, __a, __a,
                             __a, __a, __a, __a, __a, __a, __a, __a };
}

/* # Moving between an integer vector and an ordinary integer */

static __inline__ __m128i __attribute__((__always_inline__)) _mm_cvtsi32_si128(int __a)
{
  return (__m128i)(__v4si){ __a, 0, 0, 0 };
}

static __inline__ int __attribute__((__always_inline__)) _mm_cvtsi128_si32(__m128i __a)
{
  return ((__v4si)__a)[0];
}

#ifdef __x86_64__
static __inline__ __m128i __attribute__((__always_inline__)) _mm_cvtsi64_si128(long long __a)
{
  return (__m128i)(__v2di){ __a, 0LL };
}

static __inline__ long long __attribute__((__always_inline__)) _mm_cvtsi128_si64(__m128i __a)
{
  return ((__v2di)__a)[0];
}

/* The `x` spellings are the same two under the names an older compiler used. */
static __inline__ __m128i __attribute__((__always_inline__)) _mm_cvtsi64x_si128(long long __a)
{
  return (__m128i)(__v2di){ __a, 0LL };
}

static __inline__ long long __attribute__((__always_inline__)) _mm_cvtsi128_si64x(__m128i __a)
{
  return ((__v2di)__a)[0];
}
#endif

/* Between the eight byte vector and the sixteen byte one, which is a move of eight bytes in each
 * direction with the upper eight either dropped or zeroed. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_movepi64_pi64(__m128i __a)
{
  return (__m64)(__v1di){ ((__v2di)__a)[0] };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_movpi64_epi64(__m64 __a)
{
  return (__m128i)(__v2di){ ((__v1di)__a)[0], 0LL };
}

/* Lane zero kept and lane one zeroed, which is the same shape without changing type. */
static __inline__ __m128i __attribute__((__always_inline__)) _mm_move_epi64(__m128i __a)
{
  return (__m128i)(__v2di){ ((__v2di)__a)[0], 0LL };
}

/* # Integer arithmetic
 *
 * The plain adds and subtracts are done through the unsigned spelling of their width, so that a
 * sum that does not fit wraps rather than being undefined. Wrapping is what the instruction does
 * and what the intrinsic promises; signed overflow in C is neither. */

static __inline__ __m128i __attribute__((__always_inline__)) _mm_add_epi8(__m128i __a, __m128i __b)
{
  return (__m128i)((__v16qu)__a + (__v16qu)__b);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_add_epi16(__m128i __a, __m128i __b)
{
  return (__m128i)((__v8hu)__a + (__v8hu)__b);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_add_epi32(__m128i __a, __m128i __b)
{
  return (__m128i)((__v4su)__a + (__v4su)__b);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_add_epi64(__m128i __a, __m128i __b)
{
  return (__m128i)((__v2du)__a + (__v2du)__b);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_sub_epi8(__m128i __a, __m128i __b)
{
  return (__m128i)((__v16qu)__a - (__v16qu)__b);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_sub_epi16(__m128i __a, __m128i __b)
{
  return (__m128i)((__v8hu)__a - (__v8hu)__b);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_sub_epi32(__m128i __a, __m128i __b)
{
  return (__m128i)((__v4su)__a - (__v4su)__b);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_sub_epi64(__m128i __a, __m128i __b)
{
  return (__m128i)((__v2du)__a - (__v2du)__b);
}

/* The saturating adds and subtracts, which clamp rather than wrap. Each sum is worked out in an
 * `int`, where it cannot overflow, and then clamped to the range of its own lane. */

static __inline__ __m128i __attribute__((__always_inline__))
_mm_adds_epi8(__m128i __a, __m128i __b)
{
  __v16qi __x = (__v16qi)__a, __y = (__v16qi)__b, __answer;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __answer[__i] = __rucc_sat_qi((int)__x[__i] + (int)__y[__i]);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_adds_epi16(__m128i __a, __m128i __b)
{
  __v8hi __x = (__v8hi)__a, __y = (__v8hi)__b, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = __rucc_sat_hi((int)__x[__i] + (int)__y[__i]);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_adds_epu8(__m128i __a, __m128i __b)
{
  __v16qu __x = (__v16qu)__a, __y = (__v16qu)__b, __answer;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __answer[__i] = __rucc_sat_qu((int)__x[__i] + (int)__y[__i]);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_adds_epu16(__m128i __a, __m128i __b)
{
  __v8hu __x = (__v8hu)__a, __y = (__v8hu)__b, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = __rucc_sat_hu((int)__x[__i] + (int)__y[__i]);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_subs_epi8(__m128i __a, __m128i __b)
{
  __v16qi __x = (__v16qi)__a, __y = (__v16qi)__b, __answer;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __answer[__i] = __rucc_sat_qi((int)__x[__i] - (int)__y[__i]);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_subs_epi16(__m128i __a, __m128i __b)
{
  __v8hi __x = (__v8hi)__a, __y = (__v8hi)__b, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = __rucc_sat_hi((int)__x[__i] - (int)__y[__i]);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_subs_epu8(__m128i __a, __m128i __b)
{
  __v16qu __x = (__v16qu)__a, __y = (__v16qu)__b, __answer;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __answer[__i] = __rucc_sat_qu((int)__x[__i] - (int)__y[__i]);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_subs_epu16(__m128i __a, __m128i __b)
{
  __v8hu __x = (__v8hu)__a, __y = (__v8hu)__b, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = __rucc_sat_hu((int)__x[__i] - (int)__y[__i]);
  return (__m128i)__answer;
}

/* The three multiplies over shorts. The low halves wrap and are done unsigned for that reason,
 * and the two high halves differ only in how the operands are read, which is the whole of the
 * difference between `pmulhw` and `pmulhuw`. */

static __inline__ __m128i __attribute__((__always_inline__))
_mm_mullo_epi16(__m128i __a, __m128i __b)
{
  return (__m128i)((__v8hu)__a * (__v8hu)__b);
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_mulhi_epi16(__m128i __a, __m128i __b)
{
  __v8hi __x = (__v8hi)__a, __y = (__v8hi)__b, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = (short)(((int)__x[__i] * (int)__y[__i]) >> 16);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_mulhi_epu16(__m128i __a, __m128i __b)
{
  __v8hu __x = (__v8hu)__a, __y = (__v8hu)__b, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = (unsigned short)(((unsigned int)__x[__i] * (unsigned int)__y[__i]) >> 16);
  return (__m128i)__answer;
}

/* Eight shorts multiplied in pairs and each pair added, giving four ints. The sum is worked out
 * in `unsigned` because the one input that overflows an `int` is two lanes both holding the most
 * negative short, whose products are each two to the thirtieth and whose sum is two to the
 * thirty first. That wraps to the most negative int, which is what the instruction answers. */
static __inline__ __m128i __attribute__((__always_inline__))
_mm_madd_epi16(__m128i __a, __m128i __b)
{
  __v8hi __x = (__v8hi)__a, __y = (__v8hi)__b;
  __v4si __answer;
  int __i;
  for (__i = 0; __i < 4; __i++)
    __answer[__i] = (int)((unsigned int)((int)__x[__i * 2] * (int)__y[__i * 2])
                          + (unsigned int)((int)__x[__i * 2 + 1] * (int)__y[__i * 2 + 1]));
  return (__m128i)__answer;
}

/* The even numbered thirty two bit lanes multiplied as unsigned values into sixty four bit
 * answers. Lanes one and three take no part, which is what makes this the odd one out among the
 * multiplies and why a program reaching for it usually shuffles first. */
static __inline__ __m128i __attribute__((__always_inline__))
_mm_mul_epu32(__m128i __a, __m128i __b)
{
  __v4su __x = (__v4su)__a, __y = (__v4su)__b;
  return (__m128i)(__v2du){ (unsigned long long)__x[0] * (unsigned long long)__y[0],
                            (unsigned long long)__x[2] * (unsigned long long)__y[2] };
}

/* The same operation on the eight byte vector, where there is one pair rather than two. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_mul_su32(__m64 __a, __m64 __b)
{
  __v2su __x = (__v2su)__a, __y = (__v2su)__b;
  return (__m64)(__v1du){ (unsigned long long)__x[0] * (unsigned long long)__y[0] };
}

/* The sum of the absolute differences of sixteen bytes, taken eight at a time, each sum landing
 * in the lowest short of its own half. The differences go through `int` so that subtracting two
 * unsigned bytes cannot wrap round to a large positive number. */
static __inline__ __m128i __attribute__((__always_inline__))
_mm_sad_epu8(__m128i __a, __m128i __b)
{
  __v16qu __x = (__v16qu)__a, __y = (__v16qu)__b;
  int __half, __i;
  __v2di __answer = (__v2di){ 0LL, 0LL };
  for (__half = 0; __half < 2; __half++)
  {
    int __total = 0;
    for (__i = 0; __i < 8; __i++)
    {
      int __difference = (int)__x[__half * 8 + __i] - (int)__y[__half * 8 + __i];
      __total += __difference < 0 ? -__difference : __difference;
    }
    __answer[__half] = __total;
  }
  return (__m128i)__answer;
}

/* The larger and the smaller of each lane, at the two widths SSE2 has instructions for: signed
 * shorts and unsigned bytes. */

static __inline__ __m128i __attribute__((__always_inline__))
_mm_max_epi16(__m128i __a, __m128i __b)
{
  __v8hi __x = (__v8hi)__a, __y = (__v8hi)__b, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = __x[__i] > __y[__i] ? __x[__i] : __y[__i];
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_min_epi16(__m128i __a, __m128i __b)
{
  __v8hi __x = (__v8hi)__a, __y = (__v8hi)__b, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = __x[__i] < __y[__i] ? __x[__i] : __y[__i];
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_max_epu8(__m128i __a, __m128i __b)
{
  __v16qu __x = (__v16qu)__a, __y = (__v16qu)__b, __answer;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __answer[__i] = __x[__i] > __y[__i] ? __x[__i] : __y[__i];
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_min_epu8(__m128i __a, __m128i __b)
{
  __v16qu __x = (__v16qu)__a, __y = (__v16qu)__b, __answer;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __answer[__i] = __x[__i] < __y[__i] ? __x[__i] : __y[__i];
  return (__m128i)__answer;
}

/* The rounded average of each lane, which is the sum plus one halved, summed where it cannot
 * wrap. */

static __inline__ __m128i __attribute__((__always_inline__))
_mm_avg_epu8(__m128i __a, __m128i __b)
{
  __v16qu __x = (__v16qu)__a, __y = (__v16qu)__b, __answer;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __answer[__i] = (unsigned char)(((unsigned int)__x[__i] + (unsigned int)__y[__i] + 1u) >> 1);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_avg_epu16(__m128i __a, __m128i __b)
{
  __v8hu __x = (__v8hu)__a, __y = (__v8hu)__b, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = (unsigned short)(((unsigned int)__x[__i] + (unsigned int)__y[__i] + 1u) >> 1);
  return (__m128i)__answer;
}

/* # The bitwise operations
 *
 * One set for the whole vector, since a bitwise operation does not care what the lanes are. */

static __inline__ __m128i __attribute__((__always_inline__)) _mm_and_si128(__m128i __a, __m128i __b)
{
  return (__m128i)((__v2du)__a & (__v2du)__b);
}

/* The complement of the first and the second, in that order, which is the opposite of how the
 * name reads. */
static __inline__ __m128i __attribute__((__always_inline__))
_mm_andnot_si128(__m128i __a, __m128i __b)
{
  return (__m128i)(~(__v2du)__a & (__v2du)__b);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_or_si128(__m128i __a, __m128i __b)
{
  return (__m128i)((__v2du)__a | (__v2du)__b);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_xor_si128(__m128i __a, __m128i __b)
{
  return (__m128i)((__v2du)__a ^ (__v2du)__b);
}

/* # Integer comparisons
 *
 * Equality and greater than at three widths, each answering a mask. There is no unsigned
 * comparison and no sixty four bit one, which is SSE2 and not an omission here: the first arrived
 * with SSE4.1 and the second with SSE4.2. */

static __inline__ __m128i __attribute__((__always_inline__))
_mm_cmpeq_epi8(__m128i __a, __m128i __b)
{
  return (__m128i)((__v16qi)((__v16qi)__a == (__v16qi)__b));
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_cmpeq_epi16(__m128i __a, __m128i __b)
{
  return (__m128i)((__v8hi)((__v8hi)__a == (__v8hi)__b));
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_cmpeq_epi32(__m128i __a, __m128i __b)
{
  return (__m128i)((__v4si)((__v4si)__a == (__v4si)__b));
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_cmpgt_epi8(__m128i __a, __m128i __b)
{
  return (__m128i)((__v16qi)((__v16qi)__a > (__v16qi)__b));
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_cmpgt_epi16(__m128i __a, __m128i __b)
{
  return (__m128i)((__v8hi)((__v8hi)__a > (__v8hi)__b));
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_cmpgt_epi32(__m128i __a, __m128i __b)
{
  return (__m128i)((__v4si)((__v4si)__a > (__v4si)__b));
}

/* The less than forms, which are not instructions. The machine has only the greater than ones and
 * a compiler gets these by swapping the operands, and so does this. */

static __inline__ __m128i __attribute__((__always_inline__))
_mm_cmplt_epi8(__m128i __a, __m128i __b)
{
  return _mm_cmpgt_epi8(__b, __a);
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_cmplt_epi16(__m128i __a, __m128i __b)
{
  return _mm_cmpgt_epi16(__b, __a);
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_cmplt_epi32(__m128i __a, __m128i __b)
{
  return _mm_cmpgt_epi32(__b, __a);
}

/* # Shifts
 *
 * Each width has three forms: a count held in a vector, a count given as an immediate, and for
 * the right shifts a choice between bringing in zeros and bringing in the sign. A count wider
 * than the lane gives zero, or the sign repeated for an arithmetic right shift, where C leaves a
 * shift that wide open. The count is read as an unsigned sixty four bit value, so a negative one
 * is enormous and therefore wide, which is what the instruction does with it. */

static __inline__ unsigned long long __attribute__((__always_inline__))
__rucc_count128(__m128i __n)
{
  return ((__v2du)__n)[0];
}

static __inline__ __m128i __attribute__((__always_inline__))
__rucc_sllw128(__m128i __a, unsigned long long __n)
{
  __v8hu __x = (__v8hu)__a, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = __n > 15 ? 0 : (unsigned short)(__x[__i] << __n);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
__rucc_srlw128(__m128i __a, unsigned long long __n)
{
  __v8hu __x = (__v8hu)__a, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = __n > 15 ? 0 : (unsigned short)(__x[__i] >> __n);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
__rucc_sraw128(__m128i __a, unsigned long long __n)
{
  __v8hi __x = (__v8hi)__a, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = (short)((int)__x[__i] >> (__n > 15 ? 15 : (int)__n));
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
__rucc_slld128(__m128i __a, unsigned long long __n)
{
  __v4su __x = (__v4su)__a, __answer;
  int __i;
  for (__i = 0; __i < 4; __i++)
    __answer[__i] = __n > 31 ? 0u : __x[__i] << __n;
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
__rucc_srld128(__m128i __a, unsigned long long __n)
{
  __v4su __x = (__v4su)__a, __answer;
  int __i;
  for (__i = 0; __i < 4; __i++)
    __answer[__i] = __n > 31 ? 0u : __x[__i] >> __n;
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
__rucc_srad128(__m128i __a, unsigned long long __n)
{
  __v4si __x = (__v4si)__a, __answer;
  int __i;
  for (__i = 0; __i < 4; __i++)
    __answer[__i] = __x[__i] >> (__n > 31 ? 31 : (int)__n);
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
__rucc_sllq128(__m128i __a, unsigned long long __n)
{
  __v2du __x = (__v2du)__a;
  return (__m128i)(__v2du){ __n > 63 ? 0ULL : __x[0] << __n, __n > 63 ? 0ULL : __x[1] << __n };
}

static __inline__ __m128i __attribute__((__always_inline__))
__rucc_srlq128(__m128i __a, unsigned long long __n)
{
  __v2du __x = (__v2du)__a;
  return (__m128i)(__v2du){ __n > 63 ? 0ULL : __x[0] >> __n, __n > 63 ? 0ULL : __x[1] >> __n };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_sll_epi16(__m128i __a, __m128i __b)
{
  return __rucc_sllw128(__a, __rucc_count128(__b));
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_slli_epi16(__m128i __a, int __n)
{
  return __rucc_sllw128(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_srl_epi16(__m128i __a, __m128i __b)
{
  return __rucc_srlw128(__a, __rucc_count128(__b));
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_srli_epi16(__m128i __a, int __n)
{
  return __rucc_srlw128(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_sra_epi16(__m128i __a, __m128i __b)
{
  return __rucc_sraw128(__a, __rucc_count128(__b));
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_srai_epi16(__m128i __a, int __n)
{
  return __rucc_sraw128(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_sll_epi32(__m128i __a, __m128i __b)
{
  return __rucc_slld128(__a, __rucc_count128(__b));
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_slli_epi32(__m128i __a, int __n)
{
  return __rucc_slld128(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_srl_epi32(__m128i __a, __m128i __b)
{
  return __rucc_srld128(__a, __rucc_count128(__b));
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_srli_epi32(__m128i __a, int __n)
{
  return __rucc_srld128(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_sra_epi32(__m128i __a, __m128i __b)
{
  return __rucc_srad128(__a, __rucc_count128(__b));
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_srai_epi32(__m128i __a, int __n)
{
  return __rucc_srad128(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_sll_epi64(__m128i __a, __m128i __b)
{
  return __rucc_sllq128(__a, __rucc_count128(__b));
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_slli_epi64(__m128i __a, int __n)
{
  return __rucc_sllq128(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_srl_epi64(__m128i __a, __m128i __b)
{
  return __rucc_srlq128(__a, __rucc_count128(__b));
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_srli_epi64(__m128i __a, int __n)
{
  return __rucc_srlq128(__a, (unsigned long long)(unsigned int)__n);
}

/* The shifts of the whole vector by a number of bytes rather than of bits. These are not lane
 * operations at all: the sixteen bytes move as one run and zeros come in at the end they leave.
 * A count of sixteen or more gives all zeros, which is the instruction's rule and not C's. */

static __inline__ __m128i __attribute__((__always_inline__)) _mm_slli_si128(__m128i __a, int __n)
{
  __v16qu __from = (__v16qu)__a, __answer;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __answer[__i] = __n > 15 || __n < 0 || __i < __n ? 0 : __from[__i - __n];
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_srli_si128(__m128i __a, int __n)
{
  __v16qu __from = (__v16qu)__a, __answer;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __answer[__i] = __n > 15 || __n < 0 || __i + __n > 15 ? 0 : __from[__i + __n];
  return (__m128i)__answer;
}

/* The same two under the names that say in full what they shift, which is what gcc calls them. */
static __inline__ __m128i __attribute__((__always_inline__)) _mm_bslli_si128(__m128i __a, int __n)
{
  return _mm_slli_si128(__a, __n);
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_bsrli_si128(__m128i __a, int __n)
{
  return _mm_srli_si128(__a, __n);
}

/* # Packing and unpacking
 *
 * A pack halves the width of every lane and puts two vectors' worth into one, saturating rather
 * than truncating. An unpack does the opposite, interleaving the halves of two vectors. */

static __inline__ __m128i __attribute__((__always_inline__))
_mm_packs_epi16(__m128i __a, __m128i __b)
{
  __v8hi __x = (__v8hi)__a, __y = (__v8hi)__b;
  __v16qi __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
  {
    __answer[__i] = __rucc_sat_qi((int)__x[__i]);
    __answer[__i + 8] = __rucc_sat_qi((int)__y[__i]);
  }
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_packs_epi32(__m128i __a, __m128i __b)
{
  __v4si __x = (__v4si)__a, __y = (__v4si)__b;
  __v8hi __answer;
  int __i;
  for (__i = 0; __i < 4; __i++)
  {
    __answer[__i] = __rucc_sat_hi(__x[__i]);
    __answer[__i + 4] = __rucc_sat_hi(__y[__i]);
  }
  return (__m128i)__answer;
}

/* The unsigned pack, which clamps at zero below rather than at the most negative byte. */
static __inline__ __m128i __attribute__((__always_inline__))
_mm_packus_epi16(__m128i __a, __m128i __b)
{
  __v8hi __x = (__v8hi)__a, __y = (__v8hi)__b;
  __v16qu __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
  {
    __answer[__i] = __rucc_sat_qu((int)__x[__i]);
    __answer[__i + 8] = __rucc_sat_qu((int)__y[__i]);
  }
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_unpackhi_epi8(__m128i __a, __m128i __b)
{
  __v16qi __x = (__v16qi)__a, __y = (__v16qi)__b, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
  {
    __answer[__i * 2] = __x[__i + 8];
    __answer[__i * 2 + 1] = __y[__i + 8];
  }
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_unpacklo_epi8(__m128i __a, __m128i __b)
{
  __v16qi __x = (__v16qi)__a, __y = (__v16qi)__b, __answer;
  int __i;
  for (__i = 0; __i < 8; __i++)
  {
    __answer[__i * 2] = __x[__i];
    __answer[__i * 2 + 1] = __y[__i];
  }
  return (__m128i)__answer;
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_unpackhi_epi16(__m128i __a, __m128i __b)
{
  __v8hi __x = (__v8hi)__a, __y = (__v8hi)__b;
  return (__m128i)(__v8hi){ __x[4], __y[4], __x[5], __y[5], __x[6], __y[6], __x[7], __y[7] };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_unpacklo_epi16(__m128i __a, __m128i __b)
{
  __v8hi __x = (__v8hi)__a, __y = (__v8hi)__b;
  return (__m128i)(__v8hi){ __x[0], __y[0], __x[1], __y[1], __x[2], __y[2], __x[3], __y[3] };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_unpackhi_epi32(__m128i __a, __m128i __b)
{
  __v4si __x = (__v4si)__a, __y = (__v4si)__b;
  return (__m128i)(__v4si){ __x[2], __y[2], __x[3], __y[3] };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_unpacklo_epi32(__m128i __a, __m128i __b)
{
  __v4si __x = (__v4si)__a, __y = (__v4si)__b;
  return (__m128i)(__v4si){ __x[0], __y[0], __x[1], __y[1] };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_unpackhi_epi64(__m128i __a, __m128i __b)
{
  return (__m128i)(__v2di){ ((__v2di)__a)[1], ((__v2di)__b)[1] };
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_unpacklo_epi64(__m128i __a, __m128i __b)
{
  return (__m128i)(__v2di){ ((__v2di)__a)[0], ((__v2di)__b)[0] };
}

/* # Shuffles and lane access */

/* Four thirty two bit lanes chosen by a selector, two bits each, all four out of the one
 * operand. */
static __inline__ __m128i __attribute__((__always_inline__))
_mm_shuffle_epi32(__m128i __a, const int __mask)
{
  __v4si __from = (__v4si)__a;
  return (__m128i)(__v4si){ __from[__mask & 3], __from[(__mask >> 2) & 3],
                            __from[(__mask >> 4) & 3], __from[(__mask >> 6) & 3] };
}

/* The four shorts of the upper half chosen by a selector, the lower half untouched. */
static __inline__ __m128i __attribute__((__always_inline__))
_mm_shufflehi_epi16(__m128i __a, const int __mask)
{
  __v8hi __from = (__v8hi)__a, __answer = __from;
  __answer[4] = __from[4 + (__mask & 3)];
  __answer[5] = __from[4 + ((__mask >> 2) & 3)];
  __answer[6] = __from[4 + ((__mask >> 4) & 3)];
  __answer[7] = __from[4 + ((__mask >> 6) & 3)];
  return (__m128i)__answer;
}

/* The four shorts of the lower half chosen by a selector, the upper half untouched. */
static __inline__ __m128i __attribute__((__always_inline__))
_mm_shufflelo_epi16(__m128i __a, const int __mask)
{
  __v8hi __from = (__v8hi)__a, __answer = __from;
  __answer[0] = __from[__mask & 3];
  __answer[1] = __from[(__mask >> 2) & 3];
  __answer[2] = __from[(__mask >> 4) & 3];
  __answer[3] = __from[(__mask >> 6) & 3];
  return (__m128i)__answer;
}

/* One short out of the eight, read unsigned so that a lane with its top bit set does not come out
 * negative, which is the instruction's zero extension. */
static __inline__ int __attribute__((__always_inline__))
_mm_extract_epi16(__m128i __a, const int __n)
{
  return (int)((__v8hu)__a)[__n & 7];
}

static __inline__ __m128i __attribute__((__always_inline__))
_mm_insert_epi16(__m128i __a, const int __d, const int __n)
{
  __v8hi __answer = (__v8hi)__a;
  __answer[__n & 7] = (short)__d;
  return (__m128i)__answer;
}

/* The sign bit of each of sixteen bytes, lowest byte in the lowest bit. */
static __inline__ int __attribute__((__always_inline__)) _mm_movemask_epi8(__m128i __a)
{
  __v16qu __from = (__v16qu)__a;
  int __i, __answer = 0;
  for (__i = 0; __i < 16; __i++)
    __answer |= (int)(__from[__i] >> 7) << __i;
  return __answer;
}

/* # Loads and stores of integer vectors */

static __inline__ __m128i __attribute__((__always_inline__)) _mm_load_si128(__m128i const *__p)
{
  return *__p;
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_loadu_si128(__m128i_u const *__p)
{
  return *(const __m128i_u *)__p;
}

/* Eight bytes into the lower half, the upper half zeroed. */
static __inline__ __m128i __attribute__((__always_inline__)) _mm_loadl_epi64(__m128i_u const *__p)
{
  long long __from;
  __builtin_memcpy(&__from, __p, sizeof __from);
  return (__m128i)(__v2di){ __from, 0LL };
}

/* Two, four and eight bytes into the bottom of the vector with everything above them zeroed.
 * `memcpy` rather than a load through a pointer to the width, because the address need not be
 * aligned for any of the three and this is how that is said without a type for each. */
static __inline__ __m128i __attribute__((__always_inline__)) _mm_loadu_si16(void const *__p)
{
  short __from;
  __builtin_memcpy(&__from, __p, sizeof __from);
  return (__m128i)(__v8hi){ __from, 0, 0, 0, 0, 0, 0, 0 };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_loadu_si32(void const *__p)
{
  int __from;
  __builtin_memcpy(&__from, __p, sizeof __from);
  return (__m128i)(__v4si){ __from, 0, 0, 0 };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_loadu_si64(void const *__p)
{
  long long __from;
  __builtin_memcpy(&__from, __p, sizeof __from);
  return (__m128i)(__v2di){ __from, 0LL };
}

static __inline__ void __attribute__((__always_inline__)) _mm_store_si128(__m128i *__p, __m128i __b)
{
  *__p = __b;
}

static __inline__ void __attribute__((__always_inline__))
_mm_storeu_si128(__m128i_u *__p, __m128i __b)
{
  *(__m128i_u *)__p = __b;
}

static __inline__ void __attribute__((__always_inline__))
_mm_storel_epi64(__m128i_u *__p, __m128i __b)
{
  long long __from = ((__v2di)__b)[0];
  __builtin_memcpy(__p, &__from, sizeof __from);
}

static __inline__ void __attribute__((__always_inline__)) _mm_storeu_si16(void *__p, __m128i __b)
{
  short __from = ((__v8hi)__b)[0];
  __builtin_memcpy(__p, &__from, sizeof __from);
}

static __inline__ void __attribute__((__always_inline__)) _mm_storeu_si32(void *__p, __m128i __b)
{
  int __from = ((__v4si)__b)[0];
  __builtin_memcpy(__p, &__from, sizeof __from);
}

static __inline__ void __attribute__((__always_inline__)) _mm_storeu_si64(void *__p, __m128i __b)
{
  long long __from = ((__v2di)__b)[0];
  __builtin_memcpy(__p, &__from, sizeof __from);
}

/* The bytes of the first operand whose matching byte in the second has its sign bit set, written
 * to the address given. The rest are not written at all, which is the point of the operation and
 * is why this is a loop over sixteen bytes rather than one store. */
static __inline__ void __attribute__((__always_inline__))
_mm_maskmoveu_si128(__m128i __a, __m128i __mask, char *__p)
{
  __v16qu __from = (__v16qu)__a, __which = (__v16qu)__mask;
  int __i;
  for (__i = 0; __i < 16; __i++)
    if ((__which[__i] & 0x80u) != 0u)
      __p[__i] = (char)__from[__i];
}

/* # Conversions between the three vector types
 *
 * The ones that produce integers round under whatever mode the program is running in, which the
 * helpers at the top of this file follow without being able to read it. The truncating forms are
 * the ones spelled with two `t`s and are an ordinary cast. Every one of them answers the most
 * negative value for an input that does not fit, which is what the instruction answers and what a
 * cast in C leaves undefined. */

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cvtepi32_pd(__m128i __a)
{
  __v4si __from = (__v4si)__a;
  return (__m128d)(__v2df){ (double)__from[0], (double)__from[1] };
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cvtepi32_ps(__m128i __a)
{
  __v4si __from = (__v4si)__a;
  return (__m128)(__v4sf){ (float)__from[0], (float)__from[1], (float)__from[2],
                           (float)__from[3] };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_cvtpd_epi32(__m128d __a)
{
  __v2df __from = (__v2df)__a;
  return (__m128i)(__v4si){ __rucc_double_to_int(__from[0]), __rucc_double_to_int(__from[1]), 0,
                            0 };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_cvttpd_epi32(__m128d __a)
{
  __v2df __from = (__v2df)__a;
  return (__m128i)(__v4si){ __rucc_double_to_int_trunc(__from[0]),
                            __rucc_double_to_int_trunc(__from[1]), 0, 0 };
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cvtpd_ps(__m128d __a)
{
  __v2df __from = (__v2df)__a;
  return (__m128)(__v4sf){ (float)__from[0], (float)__from[1], 0.0f, 0.0f };
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cvtps_pd(__m128 __a)
{
  __v4sf __from = (__v4sf)__a;
  return (__m128d)(__v2df){ (double)__from[0], (double)__from[1] };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_cvtps_epi32(__m128 __a)
{
  __v4sf __from = (__v4sf)__a;
  return (__m128i)(__v4si){ __rucc_float_to_int(__from[0]), __rucc_float_to_int(__from[1]),
                            __rucc_float_to_int(__from[2]), __rucc_float_to_int(__from[3]) };
}

static __inline__ __m128i __attribute__((__always_inline__)) _mm_cvttps_epi32(__m128 __a)
{
  __v4sf __from = (__v4sf)__a;
  return (__m128i)(__v4si){
    __rucc_float_to_int_trunc(__from[0]), __rucc_float_to_int_trunc(__from[1]),
    __rucc_float_to_int_trunc(__from[2]), __rucc_float_to_int_trunc(__from[3])
  };
}

/* The ones that cross to the eight byte vector. */

static __inline__ __m64 __attribute__((__always_inline__)) _mm_cvtpd_pi32(__m128d __a)
{
  __v2df __from = (__v2df)__a;
  return (__m64)(__v2si){ __rucc_double_to_int(__from[0]), __rucc_double_to_int(__from[1]) };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_cvttpd_pi32(__m128d __a)
{
  __v2df __from = (__v2df)__a;
  return (__m64)(__v2si){ __rucc_double_to_int_trunc(__from[0]),
                          __rucc_double_to_int_trunc(__from[1]) };
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cvtpi32_pd(__m64 __a)
{
  __v2si __from = (__v2si)__a;
  return (__m128d)(__v2df){ (double)__from[0], (double)__from[1] };
}

/* The scalar conversions, each of which writes lane zero and leaves lane one as it was. */

static __inline__ int __attribute__((__always_inline__)) _mm_cvtsd_si32(__m128d __a)
{
  return __rucc_double_to_int(((__v2df)__a)[0]);
}

static __inline__ int __attribute__((__always_inline__)) _mm_cvttsd_si32(__m128d __a)
{
  return __rucc_double_to_int_trunc(((__v2df)__a)[0]);
}

static __inline__ __m128d __attribute__((__always_inline__)) _mm_cvtsi32_sd(__m128d __a, int __b)
{
  __v2df __answer = (__v2df)__a;
  __answer[0] = (double)__b;
  return (__m128d)__answer;
}

/* A double down to a float in lane zero of a float vector, the upper three lanes taken from the
 * float operand. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_cvtsd_ss(__m128 __a, __m128d __b)
{
  __v4sf __answer = (__v4sf)__a;
  __answer[0] = (float)((__v2df)__b)[0];
  return (__m128)__answer;
}

/* A float up to a double in lane zero, lane one taken from the double operand. */
static __inline__ __m128d __attribute__((__always_inline__)) _mm_cvtss_sd(__m128d __a, __m128 __b)
{
  __v2df __answer = (__v2df)__a;
  __answer[0] = (double)((__v4sf)__b)[0];
  return (__m128d)__answer;
}

#ifdef __x86_64__
static __inline__ long long __attribute__((__always_inline__)) _mm_cvtsd_si64(__m128d __a)
{
  return __rucc_double_to_long(((__v2df)__a)[0]);
}

static __inline__ long long __attribute__((__always_inline__)) _mm_cvttsd_si64(__m128d __a)
{
  return __rucc_double_to_long_trunc(((__v2df)__a)[0]);
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cvtsi64_sd(__m128d __a, long long __b)
{
  __v2df __answer = (__v2df)__a;
  __answer[0] = (double)__b;
  return (__m128d)__answer;
}

/* The `x` spellings, which are the same three under older names. */
static __inline__ long long __attribute__((__always_inline__)) _mm_cvtsd_si64x(__m128d __a)
{
  return __rucc_double_to_long(((__v2df)__a)[0]);
}

static __inline__ long long __attribute__((__always_inline__)) _mm_cvttsd_si64x(__m128d __a)
{
  return __rucc_double_to_long_trunc(((__v2df)__a)[0]);
}

static __inline__ __m128d __attribute__((__always_inline__))
_mm_cvtsi64x_sd(__m128d __a, long long __b)
{
  __v2df __answer = (__v2df)__a;
  __answer[0] = (double)__b;
  return (__m128d)__answer;
}
#endif

/* # The hints and the fences
 *
 * None of these changes what a program computes. `_mm_clflush` asks for a cache line to be
 * written back and dropped, and doing nothing is a correct implementation of that request for
 * everything except persistence, which is not something this target promises. The stream stores
 * ask not to disturb the cache and are plain stores here, by the same argument `<xmmintrin.h>`
 * gives: the request is about the cache and not about what ends up in memory.
 *
 * The two fences are real and are made stronger rather than weaker. `lfence` orders loads and
 * `mfence` orders everything, and both are written as the strongest fence the compiler has, which
 * is what `mfence` already is and is more than `lfence` asks for. Narrowing the first needs
 * inline assembly that can carry an instruction, which is `tamnd/rucc#349`. */

static __inline__ void __attribute__((__always_inline__)) _mm_clflush(void const *__p)
{
  (void)__p;
}

static __inline__ void __attribute__((__always_inline__)) _mm_lfence(void)
{
  __atomic_thread_fence(__ATOMIC_SEQ_CST);
}

static __inline__ void __attribute__((__always_inline__)) _mm_mfence(void)
{
  __atomic_thread_fence(__ATOMIC_SEQ_CST);
}

static __inline__ void __attribute__((__always_inline__)) _mm_stream_pd(double *__p, __m128d __a)
{
  *(__m128d *)__p = __a;
}

static __inline__ void __attribute__((__always_inline__))
_mm_stream_si128(__m128i *__p, __m128i __a)
{
  *__p = __a;
}

static __inline__ void __attribute__((__always_inline__)) _mm_stream_si32(int *__p, int __a)
{
  *__p = __a;
}

#ifdef __x86_64__
static __inline__ void __attribute__((__always_inline__))
_mm_stream_si64(long long *__p, long long __a)
{
  *__p = __a;
}
#endif

#endif
