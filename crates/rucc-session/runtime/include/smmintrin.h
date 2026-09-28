/* smmintrin.h, the SSE4.1 and SSE4.2 intrinsics.
 *
 * Every name gcc 16.2.0 has here. SSE4.1 is the larger half: the three tests of one vector against
 * another, the roundings, the blends, the two dot products, the minimum and maximum of the lane
 * types SSE2 left out, the thirty two bit multiplies, the inserts and extracts of a lane of every
 * width, the conversions that widen the low lanes of a vector, and a few more. SSE4.2 is the
 * sixteen string comparisons, the greater than of two vectors of `long long`, and the four CRC32C
 * steps, which with the population counts `<popcntintrin.h>` brings in are what PostgreSQL's
 * configure looks for.
 *
 * gcc's copy includes `<tmmintrin.h>` first, which includes `<pmmintrin.h>`, so a program that
 * includes this one gets SSE3 and SSSE3 as well, and so does this one.
 *
 * # How the vector operations are written
 *
 * As the instruction in inline assembly, loaded and stored through memory, which `<pmmintrin.h>`
 * gives the reasons for and holds the shapes of. The ones that answer with a flag, the three tests
 * and ten of the string comparisons, run the instruction and then the `set` that reads the flag,
 * which is what gcc's builtins become. The lane numbers, masks and rounding modes are immediates,
 * written `const int` as gcc writes them, and a call has to pass a constant, as it does for gcc.
 *
 * # Why the checksum is inline assembly and not a builtin
 *
 * gcc writes each step over `__builtin_ia32_crc32qi` and its three siblings, which its back end
 * turns into one instruction. The place in this compiler for an instruction the middle of the
 * compiler does not understand is the target intrinsic opcode, and the back end does not lower
 * that opcode yet, which is `tamnd/rucc#200`. Inline assembly already goes from the template to
 * the encoder, and a function that is nothing but one `crc32` inlines into its caller like any
 * other, so the instruction a loop runs is the one gcc's loop runs. The arguments are the
 * instruction's own: the running value comes in and goes out in the same register, and the new
 * data may be a register or memory, which is why its constraint is `rm`.
 *
 * # Why each is built for its extension
 *
 * For the reason `<popcntintrin.h>` gives at more length: gcc refuses a call from a function not
 * built for the instruction, and a function carrying `__attribute__((target("sse4.1")))`,
 * `target("sse4.2")` or `target("crc32")` is built for it whatever the rest of the unit is.
 * PostgreSQL's checksum is written that way, and its configure tries the call without a flag
 * before it tries `-msse4.2`. `-msse4.1`, `-msse4.2`, `-mcrc32` and `-march=x86-64-v2` build the
 * whole unit for them. See `tamnd/rucc#2002` and `tamnd/rucc#2045`.
 */

#ifndef __RUCC_SMMINTRIN_H
#define __RUCC_SMMINTRIN_H

#include <tmmintrin.h>
#include <popcntintrin.h>

/* The flag shape: both vectors loaded as the two shapes in `<pmmintrin.h>` load them, the
 * instruction run, and the one byte the `set` writes widened to the `int` gcc answers with. */
#define __RUCC_SSE_FLAG2(__insn, __set, __answer, __x, __y)                                        \
  __asm__("movdqu (%1), %%xmm0\n\tmovdqu (%2), %%xmm1\n\t" __insn                                  \
          " %%xmm1, %%xmm0\n\t" __set " %0"                                                        \
          : "=r"(__answer)                                                                         \
          : "r"(&(__x)), "r"(&(__y))                                                              \
          : "xmm0", "xmm1", "cc", "memory")

/* # SSE4.1
 *
 * The rounding modes, gcc's values under gcc's names. The low two bits are the direction, the
 * third says to use the one the control register holds instead, and the fourth keeps the inexact
 * exception from being raised. */
#define _MM_FROUND_TO_NEAREST_INT 0x00
#define _MM_FROUND_TO_NEG_INF 0x01
#define _MM_FROUND_TO_POS_INF 0x02
#define _MM_FROUND_TO_ZERO 0x03
#define _MM_FROUND_CUR_DIRECTION 0x04

#define _MM_FROUND_RAISE_EXC 0x00
#define _MM_FROUND_NO_EXC 0x08

#define _MM_FROUND_NINT (_MM_FROUND_TO_NEAREST_INT | _MM_FROUND_RAISE_EXC)
#define _MM_FROUND_FLOOR (_MM_FROUND_TO_NEG_INF | _MM_FROUND_RAISE_EXC)
#define _MM_FROUND_CEIL (_MM_FROUND_TO_POS_INF | _MM_FROUND_RAISE_EXC)
#define _MM_FROUND_TRUNC (_MM_FROUND_TO_ZERO | _MM_FROUND_RAISE_EXC)
#define _MM_FROUND_RINT (_MM_FROUND_CUR_DIRECTION | _MM_FROUND_RAISE_EXC)
#define _MM_FROUND_NEARBYINT (_MM_FROUND_CUR_DIRECTION | _MM_FROUND_NO_EXC)

/* ## Tests
 *
 * `_mm_testz_si128` is whether `__M & __V` is all zeros, which is the zero flag `ptest` sets, and
 * `_mm_testc_si128` is whether `__V & ~__M` is, which is the carry flag. `_mm_testnzc_si128` is
 * neither, which is the condition `seta` reads. */

static __inline__ int __attribute__((__always_inline__, __target__("sse4.1")))
_mm_testz_si128(__m128i __M, __m128i __V)
{
  unsigned char __R;
  __RUCC_SSE_FLAG2("ptest", "sete", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.1")))
_mm_testc_si128(__m128i __M, __m128i __V)
{
  unsigned char __R;
  __RUCC_SSE_FLAG2("ptest", "setb", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.1")))
_mm_testnzc_si128(__m128i __M, __m128i __V)
{
  unsigned char __R;
  __RUCC_SSE_FLAG2("ptest", "seta", __R, __M, __V);
  return __R;
}

#define _mm_test_all_zeros(M, V) _mm_testz_si128((M), (V))

#define _mm_test_all_ones(V) _mm_testc_si128((V), _mm_cmpeq_epi32((V), (V)))

#define _mm_test_mix_ones_zeros(M, V) _mm_testnzc_si128((M), (V))

/* ## Rounding
 *
 * Each lane to an integral value by the mode `__M` names, which is the instruction's to get right
 * for every input and the reason these are not C. The scalar forms round the low lane of `__V`
 * and take the rest of the answer from `__D`. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("sse4.1")))
_mm_round_pd(__m128d __V, const int __M)
{
  __m128d __R;
  __RUCC_SSE_OP1_IMM("roundpd", __R, __V, __M);
  return __R;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("sse4.1")))
_mm_round_sd(__m128d __D, __m128d __V, const int __M)
{
  __m128d __R;
  __RUCC_SSE_OP2_IMM("roundsd", __R, __D, __V, __M);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("sse4.1")))
_mm_round_ps(__m128 __V, const int __M)
{
  __m128 __R;
  __RUCC_SSE_OP1_IMM("roundps", __R, __V, __M);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("sse4.1")))
_mm_round_ss(__m128 __D, __m128 __V, const int __M)
{
  __m128 __R;
  __RUCC_SSE_OP2_IMM("roundss", __R, __D, __V, __M);
  return __R;
}

#define _mm_ceil_pd(V) _mm_round_pd((V), _MM_FROUND_CEIL)
#define _mm_ceil_sd(D, V) _mm_round_sd((D), (V), _MM_FROUND_CEIL)

#define _mm_floor_pd(V) _mm_round_pd((V), _MM_FROUND_FLOOR)
#define _mm_floor_sd(D, V) _mm_round_sd((D), (V), _MM_FROUND_FLOOR)

#define _mm_ceil_ps(V) _mm_round_ps((V), _MM_FROUND_CEIL)
#define _mm_ceil_ss(D, V) _mm_round_ss((D), (V), _MM_FROUND_CEIL)

#define _mm_floor_ps(V) _mm_round_ps((V), _MM_FROUND_FLOOR)
#define _mm_floor_ss(D, V) _mm_round_ss((D), (V), _MM_FROUND_FLOOR)

/* ## Blends
 *
 * Each lane from `__Y` where the mask says so and from `__X` where it does not. The immediate
 * forms read one bit of `__M` a lane. The variable forms read the top bit of each lane of `__M`,
 * which the instruction wants in `xmm0` and nowhere else, so the two vectors go in the next two
 * registers. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_blend_epi16(__m128i __X, __m128i __Y, const int __M)
{
  __m128i __R;
  __RUCC_SSE_OP2_IMM("pblendw", __R, __X, __Y, __M);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_blendv_epi8(__m128i __X, __m128i __Y, __m128i __M)
{
  __m128i __R;
  __asm__("movdqu (%3), %%xmm0\n\tmovdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\t"
          "pblendvb %%xmm0, %%xmm2, %%xmm1\n\tmovdqu %%xmm1, (%0)"
          :
          : "r"(&__R), "r"(&__X), "r"(&__Y), "r"(&__M)
          : "xmm0", "xmm1", "xmm2", "memory");
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("sse4.1")))
_mm_blend_ps(__m128 __X, __m128 __Y, const int __M)
{
  __m128 __R;
  __RUCC_SSE_OP2_IMM("blendps", __R, __X, __Y, __M);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("sse4.1")))
_mm_blendv_ps(__m128 __X, __m128 __Y, __m128 __M)
{
  __m128 __R;
  __asm__("movdqu (%3), %%xmm0\n\tmovdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\t"
          "blendvps %%xmm0, %%xmm2, %%xmm1\n\tmovdqu %%xmm1, (%0)"
          :
          : "r"(&__R), "r"(&__X), "r"(&__Y), "r"(&__M)
          : "xmm0", "xmm1", "xmm2", "memory");
  return __R;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("sse4.1")))
_mm_blend_pd(__m128d __X, __m128d __Y, const int __M)
{
  __m128d __R;
  __RUCC_SSE_OP2_IMM("blendpd", __R, __X, __Y, __M);
  return __R;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("sse4.1")))
_mm_blendv_pd(__m128d __X, __m128d __Y, __m128d __M)
{
  __m128d __R;
  __asm__("movdqu (%3), %%xmm0\n\tmovdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\t"
          "blendvpd %%xmm0, %%xmm2, %%xmm1\n\tmovdqu %%xmm1, (%0)"
          :
          : "r"(&__R), "r"(&__X), "r"(&__Y), "r"(&__M)
          : "xmm0", "xmm1", "xmm2", "memory");
  return __R;
}

/* ## Dot products
 *
 * The high bits of `__M` say which lanes are multiplied and summed, and the low bits which lanes of
 * the answer get the sum, the rest being zero. The order the products are added in is the
 * instruction's, which a sum written in C would have to copy to give the same float. */

static __inline__ __m128 __attribute__((__always_inline__, __target__("sse4.1")))
_mm_dp_ps(__m128 __X, __m128 __Y, const int __M)
{
  __m128 __R;
  __RUCC_SSE_OP2_IMM("dpps", __R, __X, __Y, __M);
  return __R;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("sse4.1")))
_mm_dp_pd(__m128d __X, __m128d __Y, const int __M)
{
  __m128d __R;
  __RUCC_SSE_OP2_IMM("dppd", __R, __X, __Y, __M);
  return __R;
}

/* ## Integer comparisons, minimums and maximums
 *
 * The lane types SSE2 has no instruction for. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cmpeq_epi64(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pcmpeqq", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_min_epi8(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pminsb", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_max_epi8(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pmaxsb", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_min_epu16(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pminuw", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_max_epu16(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pmaxuw", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_min_epi32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pminsd", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_max_epi32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pmaxsd", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_min_epu32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pminud", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_max_epu32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pmaxud", __R, __X, __Y);
  return __R;
}

/* ## Multiplies
 *
 * `_mm_mullo_epi32` keeps the low thirty two bits of each product, and `_mm_mul_epi32` multiplies
 * the even lanes as signed values into two sixty four bit products, the signed twin of SSE2's
 * `_mm_mul_epu32`. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_mullo_epi32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pmulld", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_mul_epi32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pmuldq", __R, __X, __Y);
  return __R;
}

/* ## Inserts and extracts
 *
 * `_mm_insert_ps` copies the lane of `__S` that bits six and seven of `__N` name into the lane of
 * `__D` bits four and five name, and then clears the lanes the low four bits name. */

static __inline__ __m128 __attribute__((__always_inline__, __target__("sse4.1")))
_mm_insert_ps(__m128 __D, __m128 __S, const int __N)
{
  __m128 __R;
  __RUCC_SSE_OP2_IMM("insertps", __R, __D, __S, __N);
  return __R;
}

#define _MM_MK_INSERTPS_NDX(S, D, M) (((S) << 6) | ((D) << 4) | (M))

/* The bits of a float lane as an `int`, which is what the instruction writes to a general
 * register. */

static __inline__ int __attribute__((__always_inline__, __target__("sse4.1")))
_mm_extract_ps(__m128 __X, const int __N)
{
  int __R;
  __asm__("movdqu (%1), %%xmm0\n\textractps %2, %%xmm0, %0"
          : "=r"(__R)
          : "r"(&__X), "i"(__N)
          : "xmm0", "memory");
  return __R;
}

/* The float itself rather than its bits. gcc's is a lane read and so is this one. */
#define _MM_EXTRACT_FLOAT(D, S, N)                                                                 \
  {                                                                                                \
    (D) = ((__v4sf)(S))[(N)];                                                                      \
  }

/* One lane of `__X` in the low lane of an otherwise zero vector. */
#define _MM_PICK_OUT_PS(X, N)                                                                      \
  _mm_insert_ps(_mm_setzero_ps(), (X), _MM_MK_INSERTPS_NDX((N), 0, 0x0e))

/* A general register into lane `__N`, and lane `__N` out into one. The byte extract gives the
 * byte zero extended, as the instruction does. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_insert_epi8(__m128i __D, int __S, const int __N)
{
  __m128i __R;
  __asm__("movdqu (%1), %%xmm0\n\tpinsrb %3, %2, %%xmm0\n\tmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(&__D), "r"(__S), "i"(__N)
          : "xmm0", "memory");
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_insert_epi32(__m128i __D, int __S, const int __N)
{
  __m128i __R;
  __asm__("movdqu (%1), %%xmm0\n\tpinsrd %3, %2, %%xmm0\n\tmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(&__D), "r"(__S), "i"(__N)
          : "xmm0", "memory");
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_insert_epi64(__m128i __D, long long __S, const int __N)
{
  __m128i __R;
  __asm__("movdqu (%1), %%xmm0\n\tpinsrq %3, %2, %%xmm0\n\tmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(&__D), "r"(__S), "i"(__N)
          : "xmm0", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.1")))
_mm_extract_epi8(__m128i __X, const int __N)
{
  int __R;
  __asm__("movdqu (%1), %%xmm0\n\tpextrb %2, %%xmm0, %0"
          : "=r"(__R)
          : "r"(&__X), "i"(__N)
          : "xmm0", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.1")))
_mm_extract_epi32(__m128i __X, const int __N)
{
  int __R;
  __asm__("movdqu (%1), %%xmm0\n\tpextrd %2, %%xmm0, %0"
          : "=r"(__R)
          : "r"(&__X), "i"(__N)
          : "xmm0", "memory");
  return __R;
}

static __inline__ long long __attribute__((__always_inline__, __target__("sse4.1")))
_mm_extract_epi64(__m128i __X, const int __N)
{
  long long __R;
  __asm__("movdqu (%1), %%xmm0\n\tpextrq %2, %%xmm0, %0"
          : "=r"(__R)
          : "r"(&__X), "i"(__N)
          : "xmm0", "memory");
  return __R;
}

/* ## The rest of SSE4.1
 *
 * The smallest unsigned short and where it is, in the low two lanes of the answer. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_minpos_epu16(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("phminposuw", __R, __X);
  return __R;
}

/* The low lanes widened, with the sign or with zeros. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepi8_epi32(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovsxbd", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepi16_epi32(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovsxwd", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepi8_epi64(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovsxbq", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepi32_epi64(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovsxdq", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepi16_epi64(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovsxwq", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepi8_epi16(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovsxbw", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepu8_epi32(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovzxbd", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepu16_epi32(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovzxwd", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepu8_epi64(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovzxbq", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepu32_epi64(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovzxdq", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepu16_epi64(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovzxwq", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_cvtepu8_epi16(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pmovzxbw", __R, __X);
  return __R;
}

/* Ints packed into unsigned shorts, saturating, the four from `__X` below the four from `__Y`. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_packus_epi32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("packusdw", __R, __X, __Y);
  return __R;
}

/* Eight sums of absolute differences of four bytes each, with `__M` choosing which bytes. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_mpsadbw_epu8(__m128i __X, __m128i __Y, const int __M)
{
  __m128i __R;
  __RUCC_SSE_OP2_IMM("mpsadbw", __R, __X, __Y, __M);
  return __R;
}

/* A load that asks for sixteen aligned bytes and does not keep them in the cache, for memory a
 * device wrote. Like gcc's it is the program's promise that `__X` is aligned. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.1")))
_mm_stream_load_si128(__m128i *__X)
{
  __m128i __R;
  __asm__("movntdqa (%1), %%xmm0\n\tmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(__X)
          : "xmm0", "memory");
  return __R;
}

/* # SSE4.2
 *
 * The string comparisons read `__M` for what the bytes or shorts are, what comparing means, which
 * way round to take the result and what form to give it in, and these are the names for its parts.
 */
#define _SIDD_UBYTE_OPS 0x00
#define _SIDD_UWORD_OPS 0x01
#define _SIDD_SBYTE_OPS 0x02
#define _SIDD_SWORD_OPS 0x03

#define _SIDD_CMP_EQUAL_ANY 0x00
#define _SIDD_CMP_RANGES 0x04
#define _SIDD_CMP_EQUAL_EACH 0x08
#define _SIDD_CMP_EQUAL_ORDERED 0x0c

#define _SIDD_POSITIVE_POLARITY 0x00
#define _SIDD_NEGATIVE_POLARITY 0x10
#define _SIDD_MASKED_POSITIVE_POLARITY 0x20
#define _SIDD_MASKED_NEGATIVE_POLARITY 0x30

#define _SIDD_LEAST_SIGNIFICANT 0x00
#define _SIDD_MOST_SIGNIFICANT 0x40

#define _SIDD_BIT_MASK 0x00
#define _SIDD_UNIT_MASK 0x40

/* ## String comparisons
 *
 * The `i` forms find the length of each string at its first zero, and the `e` forms are told the
 * lengths in `eax` and `edx`. The `m` forms answer with a mask, which the instruction writes to
 * `xmm0`, so the two strings go in the next two registers. The `i` forms answer with an index,
 * which it writes to `ecx`. The rest answer with one of the flags either kind sets, and are the `i`
 * form with the flag read afterwards: `a` is neither carry nor zero, `c` is the carry, which says
 * the mask is not empty, `o` is the low bit of the mask, `s` says the first string ended early and
 * `z` says the second did. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpistrm(__m128i __X, __m128i __Y, const int __M)
{
  __m128i __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpistrm %3, %%xmm2, %%xmm1\n\t"
          "movdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(&__X), "r"(&__Y), "i"(__M)
          : "xmm0", "xmm1", "xmm2", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpistri(__m128i __X, __m128i __Y, const int __M)
{
  int __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpistri %3, %%xmm2, %%xmm1"
          : "=c"(__R)
          : "r"(&__X), "r"(&__Y), "i"(__M)
          : "xmm1", "xmm2", "cc", "memory");
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpestrm(__m128i __X, int __LX, __m128i __Y, int __LY, const int __M)
{
  __m128i __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpestrm %5, %%xmm2, %%xmm1\n\t"
          "movdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(&__X), "r"(&__Y), "a"(__LX), "d"(__LY), "i"(__M)
          : "xmm0", "xmm1", "xmm2", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpestri(__m128i __X, int __LX, __m128i __Y, int __LY, const int __M)
{
  int __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpestri %5, %%xmm2, %%xmm1"
          : "=c"(__R)
          : "r"(&__X), "r"(&__Y), "a"(__LX), "d"(__LY), "i"(__M)
          : "xmm1", "xmm2", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpistra(__m128i __X, __m128i __Y, const int __M)
{
  unsigned char __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpistri %3, %%xmm2, %%xmm1\n\t"
          "seta %0"
          : "=r"(__R)
          : "r"(&__X), "r"(&__Y), "i"(__M)
          : "xmm1", "xmm2", "rcx", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpistrc(__m128i __X, __m128i __Y, const int __M)
{
  unsigned char __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpistri %3, %%xmm2, %%xmm1\n\t"
          "setb %0"
          : "=r"(__R)
          : "r"(&__X), "r"(&__Y), "i"(__M)
          : "xmm1", "xmm2", "rcx", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpistro(__m128i __X, __m128i __Y, const int __M)
{
  unsigned char __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpistri %3, %%xmm2, %%xmm1\n\t"
          "seto %0"
          : "=r"(__R)
          : "r"(&__X), "r"(&__Y), "i"(__M)
          : "xmm1", "xmm2", "rcx", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpistrs(__m128i __X, __m128i __Y, const int __M)
{
  unsigned char __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpistri %3, %%xmm2, %%xmm1\n\t"
          "sets %0"
          : "=r"(__R)
          : "r"(&__X), "r"(&__Y), "i"(__M)
          : "xmm1", "xmm2", "rcx", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpistrz(__m128i __X, __m128i __Y, const int __M)
{
  unsigned char __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpistri %3, %%xmm2, %%xmm1\n\t"
          "sete %0"
          : "=r"(__R)
          : "r"(&__X), "r"(&__Y), "i"(__M)
          : "xmm1", "xmm2", "rcx", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpestra(__m128i __X, int __LX, __m128i __Y, int __LY, const int __M)
{
  unsigned char __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpestri %5, %%xmm2, %%xmm1\n\t"
          "seta %0"
          : "=r"(__R)
          : "r"(&__X), "r"(&__Y), "a"(__LX), "d"(__LY), "i"(__M)
          : "xmm1", "xmm2", "rcx", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpestrc(__m128i __X, int __LX, __m128i __Y, int __LY, const int __M)
{
  unsigned char __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpestri %5, %%xmm2, %%xmm1\n\t"
          "setb %0"
          : "=r"(__R)
          : "r"(&__X), "r"(&__Y), "a"(__LX), "d"(__LY), "i"(__M)
          : "xmm1", "xmm2", "rcx", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpestro(__m128i __X, int __LX, __m128i __Y, int __LY, const int __M)
{
  unsigned char __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpestri %5, %%xmm2, %%xmm1\n\t"
          "seto %0"
          : "=r"(__R)
          : "r"(&__X), "r"(&__Y), "a"(__LX), "d"(__LY), "i"(__M)
          : "xmm1", "xmm2", "rcx", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpestrs(__m128i __X, int __LX, __m128i __Y, int __LY, const int __M)
{
  unsigned char __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpestri %5, %%xmm2, %%xmm1\n\t"
          "sets %0"
          : "=r"(__R)
          : "r"(&__X), "r"(&__Y), "a"(__LX), "d"(__LY), "i"(__M)
          : "xmm1", "xmm2", "rcx", "cc", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpestrz(__m128i __X, int __LX, __m128i __Y, int __LY, const int __M)
{
  unsigned char __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tpcmpestri %5, %%xmm2, %%xmm1\n\t"
          "sete %0"
          : "=r"(__R)
          : "r"(&__X), "r"(&__Y), "a"(__LX), "d"(__LY), "i"(__M)
          : "xmm1", "xmm2", "rcx", "cc", "memory");
  return __R;
}

/* ## The one vector comparison
 *
 * Each `long long` lane of `__X` greater than the matching one of `__Y`, as signed values. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse4.2")))
_mm_cmpgt_epi64(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pcmpgtq", __R, __X, __Y);
  return __R;
}

/* ## The CRC32C steps
 *
 * Each folds one more value of its width into a running checksum. */

static __inline__ unsigned int __attribute__((__always_inline__, __target__("crc32")))
_mm_crc32_u8(unsigned int __C, unsigned char __V)
{
  __asm__("crc32b %1, %0" : "+r"(__C) : "rm"(__V));
  return __C;
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("crc32")))
_mm_crc32_u16(unsigned int __C, unsigned short __V)
{
  __asm__("crc32w %1, %0" : "+r"(__C) : "rm"(__V));
  return __C;
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("crc32")))
_mm_crc32_u32(unsigned int __C, unsigned int __V)
{
  __asm__("crc32l %1, %0" : "+r"(__C) : "rm"(__V));
  return __C;
}

/* The only one whose running value is sixty four bits wide, because the instruction's is: the
 * sixty four bit form writes a whole register whose top half is always zero, and gcc's copy
 * hands that back as the `unsigned long long` it is. */
static __inline__ unsigned long long __attribute__((__always_inline__, __target__("crc32")))
_mm_crc32_u64(unsigned long long __C, unsigned long long __V)
{
  __asm__("crc32q %1, %0" : "+r"(__C) : "rm"(__V));
  return __C;
}

/* SSE4.1's `pextrq`, the 64 bit lane N of X. Written as a lane read, which is what the instruction
 * does, since the vector is in memory here and the lane is one load away. A macro because N has to
 * be a constant, as it does for gcc. */
#define _mm_extract_epi64(X, N) ((long long)((__v2di)(__m128i)(X))[(N) & 1])

#endif /* __RUCC_SMMINTRIN_H */
