/* vpclmulqdqintrin.h, the carryless multiplication over whole vectors.
 *
 * Reached through <immintrin.h>. `vpclmulqdq` multiplies one 64 bit half of each 16 byte lane of
 * A by one of the same lane of B, the immediate saying which halves, and it is what PostgreSQL's
 * AVX-512 CRC32C folds 64 bytes at a time with. A macro, because the immediate has to be a
 * constant where the instruction is written, and inline assembly for the reason <avx512fintrin.h>
 * gives.
 */

#ifndef __RUCC_VPCLMULQDQINTRIN_H
#define __RUCC_VPCLMULQDQINTRIN_H

#include <avx512fintrin.h>

#define _mm512_clmulepi64_epi128(A, B, I)                                                      \
  __extension__({                                                                              \
    __m512i __rucc_a = (A), __rucc_b = (B), __rucc_r;                                          \
    __asm__("vmovdqu64 (%1), %%zmm16\n\t"                                                      \
            "vpclmulqdq %3, (%2), %%zmm16, %%zmm16\n\t"                                        \
            "vmovdqu64 %%zmm16, (%0)"                                                          \
            :                                                                                  \
            : "r"(&__rucc_r), "r"(&__rucc_a), "r"(&__rucc_b), "n"(I)                           \
            : "memory");                                                                       \
    __rucc_r;                                                                                  \
  })

#endif /* __RUCC_VPCLMULQDQINTRIN_H */
