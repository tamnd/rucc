/* avx512bwintrin.h, the AVX-512 byte and word intrinsics.
 *
 * Reached through <immintrin.h>. What is here is the masked byte load PostgreSQL's population
 * count reads the ends of a buffer with, written as inline assembly for the reason
 * <avx512fintrin.h> gives. The load is `vmovdqu8` under a mask with zeroing, so the bytes the mask
 * leaves out are zero and are not read at all, which is what lets the caller load a whole aligned
 * 64 bytes around a buffer that is shorter. The mask goes through `k1`, which the compiler never
 * uses for anything else.
 */

#ifndef __RUCC_IMMINTRIN_H
#error "Never use <avx512bwintrin.h> directly; include <immintrin.h> instead."
#endif

#ifndef __RUCC_AVX512BWINTRIN_H
#define __RUCC_AVX512BWINTRIN_H

typedef unsigned int __mmask32;
typedef unsigned long long __mmask64;

static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512bw")))
_mm512_maskz_loadu_epi8(__mmask64 __M, void const *__P)
{
  __m512i __R;
  __asm__("kmovq %2, %%k1\n\t"
          "vmovdqu8 (%1), %%zmm16%{%%k1%}%{z%}\n\t"
          "vmovdqu64 %%zmm16, (%0)"
          :
          : "r"(&__R), "r"(__P), "r"(__M)
          : "memory");
  return __R;
}

#endif /* __RUCC_AVX512BWINTRIN_H */
