/* avx512vlintrin.h, the AVX-512 intrinsics over 16 and 32 byte vectors.
 *
 * Reached through <immintrin.h>. What is here is the three way bit select PostgreSQL's CRC32C
 * folds its last 16 bytes with, written as inline assembly for the reason <avx512fintrin.h> gives.
 * It is `vpternlogq` on `xmm16` and up, which is what needs `avx512vl`.
 */

#ifndef __RUCC_AVX512VLINTRIN_H
#define __RUCC_AVX512VLINTRIN_H

#include <emmintrin.h>

#define _mm_ternarylogic_epi64(A, B, C, I)                                                     \
  __extension__({                                                                              \
    __m128i __rucc_a = (A), __rucc_b = (B), __rucc_c = (C), __rucc_r;                          \
    __asm__("vmovdqu64 (%1), %%xmm16\n\t"                                                      \
            "vmovdqu64 (%2), %%xmm17\n\t"                                                      \
            "vpternlogq %4, (%3), %%xmm17, %%xmm16\n\t"                                        \
            "vmovdqu64 %%xmm16, (%0)"                                                          \
            :                                                                                  \
            : "r"(&__rucc_r), "r"(&__rucc_a), "r"(&__rucc_b), "r"(&__rucc_c), "n"(I)           \
            : "memory");                                                                       \
    __rucc_r;                                                                                  \
  })

#endif /* __RUCC_AVX512VLINTRIN_H */
