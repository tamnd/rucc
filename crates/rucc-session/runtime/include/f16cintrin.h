/* f16cintrin.h, the half precision conversions.
 *
 * Reached through <immintrin.h> the way gcc reaches it. This has the six names gcc 16.2.0 has in
 * its own f16cintrin.h. Each function is built for `f16c`, and a call from a function that is not
 * built for it is refused the way gcc refuses it.
 *
 * Each function is the instruction in inline assembly through memory, the way <avxintrin.h> does
 * it. The rounding mode of a conversion to half precision is a parameter, and it is a constant in
 * each inlined copy, as in <wmmintrin.h>. The `_MM_FROUND_` names for it are in <smmintrin.h>. See
 * `tamnd/rucc#2322`.
 */

#ifndef __RUCC_F16CINTRIN_H
#define __RUCC_F16CINTRIN_H

#include <avxintrin.h>

/* The four half precision values in the low 8 bytes of A as single precision values. */
static __inline__ __m128 __attribute__((__always_inline__, __target__("f16c")))
_mm_cvtph_ps(__m128i __A)
{
  __m128 __R;
  __asm__("vmovdqu (%1), %%xmm0\n\tvcvtph2ps %%xmm0, %%xmm0\n\tvmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(&__A)
          : "xmm0", "memory");
  return __R;
}

/* The eight half precision values in A as single precision values. */
static __inline__ __m256 __attribute__((__always_inline__, __target__("f16c")))
_mm256_cvtph_ps(__m128i __A)
{
  __m256 __R;
  __asm__("vmovdqu (%1), %%xmm0\n\tvcvtph2ps %%xmm0, %%ymm0\n\tvmovdqu %%ymm0, (%0)\n\tvzeroupper"
          :
          : "r"(&__R), "r"(&__A)
          : "xmm0", "memory");
  return __R;
}

/* The four single precision values in A as half precision values in the low 8 bytes, rounded
 * by the mode I. The upper 8 bytes are zero. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("f16c")))
_mm_cvtps_ph(__m128 __A, const int __I)
{
  __m128i __R;
  __asm__("vmovdqu (%1), %%xmm0\n\tvcvtps2ph %2, %%xmm0, %%xmm0\n\tvmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(&__A), "i"(__I)
          : "xmm0", "memory");
  return __R;
}

/* The eight single precision values in A as half precision values, rounded by the mode I. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("f16c")))
_mm256_cvtps_ph(__m256 __A, const int __I)
{
  __m128i __R;
  __asm__("vmovdqu (%1), %%ymm0\n\tvcvtps2ph %2, %%ymm0, %%xmm0\n\tvmovdqu %%xmm0, (%0)\n\t"
          "vzeroupper"
          :
          : "r"(&__R), "r"(&__A), "i"(__I)
          : "xmm0", "memory");
  return __R;
}

/* One half precision value as a float. */
static __inline__ float __attribute__((__always_inline__, __target__("f16c")))
_cvtsh_ss(unsigned short __S)
{
  float __R;
  __asm__("vmovd %1, %%xmm0\n\tvcvtph2ps %%xmm0, %%xmm0\n\tvmovd %%xmm0, (%0)"
          :
          : "r"(&__R), "r"((unsigned int)__S)
          : "xmm0", "memory");
  return __R;
}

/* One float as a half precision value, rounded by the mode I. The float goes in by its bits and
 * not by its address, so that the answer depends on the value and not on where it lives. */
static __inline__ unsigned short __attribute__((__always_inline__, __target__("f16c")))
_cvtss_sh(float __F, const int __I)
{
  union {
    float __f;
    unsigned int __u;
  } __B;
  unsigned int __R;
  __B.__f = __F;
  __asm__("vmovd %1, %%xmm0\n\tvcvtps2ph %2, %%xmm0, %%xmm0\n\tvmovd %%xmm0, %0"
          : "=r"(__R)
          : "r"(__B.__u), "i"(__I)
          : "xmm0");
  return (unsigned short)__R;
}

#endif /* __RUCC_F16CINTRIN_H */
