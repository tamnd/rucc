/* avx512vpopcntdqintrin.h, the AVX-512 population counts over whole vectors.
 *
 * Reached through <immintrin.h>. `vpopcntq` counts the bits of each of the eight 64 bit lanes,
 * written as inline assembly for the reason <avx512fintrin.h> gives.
 */

#ifndef __RUCC_AVX512VPOPCNTDQINTRIN_H
#define __RUCC_AVX512VPOPCNTDQINTRIN_H

#include <avx512fintrin.h>

static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512vpopcntdq")))
_mm512_popcnt_epi64(__m512i __A)
{
  __m512i __R;
  __asm__("vpopcntq (%1), %%zmm16\n\t"
          "vmovdqu64 %%zmm16, (%0)"
          :
          : "r"(&__R), "r"(&__A)
          : "memory");
  return __R;
}

#endif /* __RUCC_AVX512VPOPCNTDQINTRIN_H */
