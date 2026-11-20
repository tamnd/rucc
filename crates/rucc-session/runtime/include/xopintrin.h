/* xopintrin.h, AMD's XOP rotates and byte permute.
 *
 * Reached through <x86intrin.h> the way gcc reaches it. This has five names of gcc 16.2.0's
 * xopintrin.h: the four rotates of each lane by a constant and the permute of the bytes of two
 * vectors by a third. FreeBSD's BLAKE2 for XOP calls three of them. The rest of gcc's header is
 * not here yet. Each function is built for `xop`, and a call from a function that is not built
 * for it is refused the way gcc refuses it.
 *
 * Each function is the instruction in inline assembly through memory, the way <avxintrin.h> does
 * it. The count of a rotate is a parameter, and it is a constant in each inlined copy, as for
 * `_mm_alignr_epi8`. A positive count rotates left and a negative one rotates right, as in the
 * instruction. See `tamnd/rucc#2322`.
 */

#ifndef __RUCC_XOPINTRIN_H
#define __RUCC_XOPINTRIN_H

#include <avxintrin.h>

/* The shape of a rotate. A goes in register 0 and is rotated there by the constant. */
#define __RUCC_XOP_ROTI(__insn, __answer, __a, __n)                                                \
  __asm__("vmovdqu (%1), %%xmm0\n\t" __insn " %2, %%xmm0, %%xmm0\n\tvmovdqu %%xmm0, (%0)"          \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__a)), "i"(__n)                                               \
          : "xmm0", "memory")

/* Each byte lane of A rotated by N bits. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("xop")))
_mm_roti_epi8(__m128i __A, const int __N)
{
  __m128i __R;
  __RUCC_XOP_ROTI("vprotb", __R, __A, __N);
  return __R;
}

/* Each 16 bit lane of A rotated by N bits. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("xop")))
_mm_roti_epi16(__m128i __A, const int __N)
{
  __m128i __R;
  __RUCC_XOP_ROTI("vprotw", __R, __A, __N);
  return __R;
}

/* Each 32 bit lane of A rotated by N bits. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("xop")))
_mm_roti_epi32(__m128i __A, const int __N)
{
  __m128i __R;
  __RUCC_XOP_ROTI("vprotd", __R, __A, __N);
  return __R;
}

/* Each 64 bit lane of A rotated by N bits. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("xop")))
_mm_roti_epi64(__m128i __A, const int __N)
{
  __m128i __R;
  __RUCC_XOP_ROTI("vprotq", __R, __A, __N);
  return __R;
}

/* Each byte of the answer is a byte of A or of B, picked by the low five bits of the byte of C in
 * the same place, and changed as its top three bits say. A is register 0, B register 1 and C
 * register 2, which is the order gcc writes `vpperm` in. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("xop")))
_mm_perm_epi8(__m128i __A, __m128i __B, __m128i __C)
{
  __m128i __R;
  __asm__("vmovdqu (%1), %%xmm0\n\tvmovdqu (%2), %%xmm1\n\tvmovdqu (%3), %%xmm2\n\t"
          "vpperm %%xmm2, %%xmm1, %%xmm0, %%xmm0\n\tvmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(&__A), "r"(&__B), "r"(&__C)
          : "xmm0", "xmm1", "xmm2", "memory");
  return __R;
}

#endif /* __RUCC_XOPINTRIN_H */
