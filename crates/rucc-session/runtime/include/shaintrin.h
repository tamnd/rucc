/* shaintrin.h, the SHA-1 and SHA-256 intrinsics.
 *
 * Seven names, the same seven gcc 16.2.0 has here, and each is built for `sha`. libiberty's
 * sha1.c calls the four SHA-1 ones from a function built for `sse4.1,sha`, after it has asked
 * cpuid whether the processor has them, and it reaches this header through <x86intrin.h>.
 *
 * Each function is the instruction in inline assembly through memory, with the shapes that
 * <pmmintrin.h> defines and for the reason it gives. The immediate of `_mm_sha1rnds4_epu32` is a
 * parameter, and it is a constant in each inlined copy. `sha256rnds2` reads its third operand from
 * `xmm0` and from nowhere else, so `_mm_sha256rnds2_epu32` has a shape of its own that loads the
 * round keys there.
 */

#ifndef __RUCC_SHAINTRIN_H
#define __RUCC_SHAINTRIN_H

#include <pmmintrin.h>

/* The next four rounds of SHA-1 from the state A and the message words and E in B. Bits 0 and 1 of
 * F select the logic function and the round constant. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("sha")))
_mm_sha1rnds4_epu32(__m128i __A, __m128i __B, const int __F)
{
  __m128i __R;
  __RUCC_SSE_OP2_IMM("sha1rnds4", __R, __A, __B, __F);
  return __R;
}

/* The state variable E after four rounds of SHA-1, added to the message words in B. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("sha")))
_mm_sha1nexte_epu32(__m128i __A, __m128i __B)
{
  __m128i __R;
  __RUCC_SSE_OP2("sha1nexte", __R, __A, __B);
  return __R;
}

/* The first part of the SHA-1 message schedule, from the previous message words. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("sha")))
_mm_sha1msg1_epu32(__m128i __A, __m128i __B)
{
  __m128i __R;
  __RUCC_SSE_OP2("sha1msg1", __R, __A, __B);
  return __R;
}

/* The last part of the SHA-1 message schedule. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("sha")))
_mm_sha1msg2_epu32(__m128i __A, __m128i __B)
{
  __m128i __R;
  __RUCC_SSE_OP2("sha1msg2", __R, __A, __B);
  return __R;
}

/* The first part of the SHA-256 message schedule. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("sha")))
_mm_sha256msg1_epu32(__m128i __A, __m128i __B)
{
  __m128i __R;
  __RUCC_SSE_OP2("sha256msg1", __R, __A, __B);
  return __R;
}

/* The last part of the SHA-256 message schedule. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("sha")))
_mm_sha256msg2_epu32(__m128i __A, __m128i __B)
{
  __m128i __R;
  __RUCC_SSE_OP2("sha256msg2", __R, __A, __B);
  return __R;
}

/* Two rounds of SHA-256 on the state halves A and B, with the message words and round constants
 * in K. The instruction reads K from `xmm0`, so A goes in `xmm1` and B in `xmm2`. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("sha")))
_mm_sha256rnds2_epu32(__m128i __A, __m128i __B, __m128i __K)
{
  __m128i __R;
  __asm__("movdqu (%1), %%xmm1\n\tmovdqu (%2), %%xmm2\n\tmovdqu (%3), %%xmm0\n\t"
          "sha256rnds2 %%xmm0, %%xmm2, %%xmm1\n\tmovdqu %%xmm1, (%0)"
          :
          : "r"(&__R), "r"(&__A), "r"(&__B), "r"(&__K)
          : "xmm0", "xmm1", "xmm2", "memory");
  return __R;
}

#endif /* __RUCC_SHAINTRIN_H */
