/* tmmintrin.h, the SSSE3 intrinsics.
 *
 * All thirty two names gcc 16.2.0 has here, sixteen operations each at two widths: `_epi` on the
 * sixteen byte vector and `_pi` on the eight byte `__m64` of `<mmintrin.h>`. They are the
 * horizontal sums and differences of neighbouring integer lanes, the multiply and add of unsigned
 * bytes by signed ones, the rounded high half of a product of shorts, the shuffle of bytes by a
 * vector of indices, the concatenation of two vectors shifted right by a number of bytes, and the
 * absolute value and the sign transfer of each lane.
 *
 * Each is the instruction in inline assembly, loaded and stored through memory, for the reasons
 * `<pmmintrin.h>` gives, and built for `ssse3` for the reasons it gives as well.
 *
 * # The eight byte forms
 *
 * gcc writes the `_pi` forms with the MMX registers. Nothing in this compiler uses one, which
 * `<mmintrin.h>` explains, and this header does not start: an MMX register is an x87 register
 * under another name, and a program that went on to use `long double` after one of these without
 * `_mm_empty` would get a wrong answer from gcc and should not get one from here. Each is the
 * sixteen byte instruction instead, run on the eight bytes placed so that the low eight bytes of
 * the answer are what the MMX form gives. For the operations that work lane by lane that is the
 * value in the low half and anything in the high half. The rest say how below.
 */

#ifndef __RUCC_TMMINTRIN_H
#define __RUCC_TMMINTRIN_H

#include <pmmintrin.h>

/* The eight byte shape: both values loaded into the low halves of `xmm0` and `xmm1`, which zeroes
 * the high halves, and the low half of `xmm0` stored after the instruction. */
#define __RUCC_SSE_PI_OP2(__insn, __answer, __x, __y)                                              \
  __asm__("movq (%1), %%xmm0\n\tmovq (%2), %%xmm1\n\t" __insn                                      \
          " %%xmm1, %%xmm0\n\tmovq %%xmm0, (%0)"                                                   \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "r"(&(__y))                                            \
          : "xmm0", "xmm1", "memory")

#define __RUCC_SSE_PI_OP1(__insn, __answer, __x)                                                   \
  __asm__("movq (%1), %%xmm0\n\t" __insn " %%xmm0, %%xmm0\n\tmovq %%xmm0, (%0)"                   \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x))                                                         \
          : "xmm0", "memory")

/* The horizontal shape. The MMX form puts the sums from its first operand in the low lanes of the
 * answer and those from its second above them, all within eight bytes, so the two are joined into
 * one register first and the instruction is run on it with itself, whose low half is then exactly
 * the first operand's pairs followed by the second's. */
#define __RUCC_SSE_PI_HORIZONTAL(__insn, __answer, __x, __y)                                       \
  __asm__("movq (%1), %%xmm0\n\tmovq (%2), %%xmm1\n\tpunpcklqdq %%xmm1, %%xmm0\n\t" __insn        \
          " %%xmm0, %%xmm0\n\tmovq %%xmm0, (%0)"                                                   \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "r"(&(__y))                                            \
          : "xmm0", "xmm1", "memory")

/* # Horizontal sums and differences
 *
 * Each lane of the answer is one pair of neighbouring lanes added or taken away, the first
 * operand's pairs in the low half and the second's in the high half. The `s` forms saturate. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_hadd_epi16(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("phaddw", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_hadd_epi32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("phaddd", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_hadds_epi16(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("phaddsw", __R, __X, __Y);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_hadd_pi16(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __RUCC_SSE_PI_HORIZONTAL("phaddw", __R, __X, __Y);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_hadd_pi32(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __RUCC_SSE_PI_HORIZONTAL("phaddd", __R, __X, __Y);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_hadds_pi16(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __RUCC_SSE_PI_HORIZONTAL("phaddsw", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_hsub_epi16(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("phsubw", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_hsub_epi32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("phsubd", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_hsubs_epi16(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("phsubsw", __R, __X, __Y);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_hsub_pi16(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __RUCC_SSE_PI_HORIZONTAL("phsubw", __R, __X, __Y);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_hsub_pi32(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __RUCC_SSE_PI_HORIZONTAL("phsubd", __R, __X, __Y);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_hsubs_pi16(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __RUCC_SSE_PI_HORIZONTAL("phsubsw", __R, __X, __Y);
  return __R;
}

/* # Multiplies
 *
 * `maddubs` multiplies the unsigned bytes of the first operand by the signed bytes of the second
 * and adds each neighbouring pair of products into a short, saturating. `mulhrs` keeps bits
 * fifteen to thirty of each product of shorts, rounded. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_maddubs_epi16(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pmaddubsw", __R, __X, __Y);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_maddubs_pi16(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __RUCC_SSE_PI_OP2("pmaddubsw", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_mulhrs_epi16(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pmulhrsw", __R, __X, __Y);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_mulhrs_pi16(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __RUCC_SSE_PI_OP2("pmulhrsw", __R, __X, __Y);
  return __R;
}

/* # Shuffles
 *
 * Each byte of the answer is the byte of the first operand the matching byte of the second
 * names, or zero when that byte has its top bit set. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_shuffle_epi8(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("pshufb", __R, __X, __Y);
  return __R;
}

/* The MMX form reads three bits of each index and the sixteen byte form reads four, so the eight
 * bytes are copied into both halves first, where the fourth bit picks between two copies of the
 * same byte. */
static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_shuffle_pi8(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __asm__("movq (%1), %%xmm0\n\tpunpcklqdq %%xmm0, %%xmm0\n\tmovq (%2), %%xmm1\n\t"
          "pshufb %%xmm1, %%xmm0\n\tmovq %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(&__X), "r"(&__Y)
          : "xmm0", "xmm1", "memory");
  return __R;
}

/* The first operand above the second, shifted right by `__N` bytes, with the low sixteen kept.
 * Zero once the shift is past both. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_alignr_epi8(__m128i __X, __m128i __Y, const int __N)
{
  __m128i __R;
  __RUCC_SSE_OP2_IMM("palignr", __R, __X, __Y, __N);
  return __R;
}

/* The same over eight bytes, which is the sixteen the two operands make shifted right, so the
 * second goes in the low half and the first in the high one and the shift is `psrldq`, which gives
 * zero past sixteen bytes the way the MMX form does. */
static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_alignr_pi8(__m64 __X, __m64 __Y, const int __N)
{
  __m64 __R;
  __asm__("movq (%2), %%xmm0\n\tmovq (%1), %%xmm1\n\tpunpcklqdq %%xmm1, %%xmm0\n\t"
          "psrldq %3, %%xmm0\n\tmovq %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(&__X), "r"(&__Y), "i"(__N)
          : "xmm0", "xmm1", "memory");
  return __R;
}

/* # Signs
 *
 * Each lane of the first operand negated where the matching lane of the second is negative, set
 * to zero where it is zero and left alone where it is positive. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_sign_epi8(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("psignb", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_sign_epi16(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("psignw", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_sign_epi32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("psignd", __R, __X, __Y);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_sign_pi8(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __RUCC_SSE_PI_OP2("psignb", __R, __X, __Y);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_sign_pi16(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __RUCC_SSE_PI_OP2("psignw", __R, __X, __Y);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_sign_pi32(__m64 __X, __m64 __Y)
{
  __m64 __R;
  __RUCC_SSE_PI_OP2("psignd", __R, __X, __Y);
  return __R;
}

/* # Absolute values
 *
 * The most negative value of a lane has no positive twin and comes back as itself, which read as
 * unsigned is its absolute value. */

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_abs_epi8(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pabsb", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_abs_epi16(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pabsw", __R, __X);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("ssse3")))
_mm_abs_epi32(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("pabsd", __R, __X);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_abs_pi8(__m64 __X)
{
  __m64 __R;
  __RUCC_SSE_PI_OP1("pabsb", __R, __X);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_abs_pi16(__m64 __X)
{
  __m64 __R;
  __RUCC_SSE_PI_OP1("pabsw", __R, __X);
  return __R;
}

static __inline__ __m64 __attribute__((__always_inline__, __target__("ssse3")))
_mm_abs_pi32(__m64 __X)
{
  __m64 __R;
  __RUCC_SSE_PI_OP1("pabsd", __R, __X);
  return __R;
}

#endif /* __RUCC_TMMINTRIN_H */
