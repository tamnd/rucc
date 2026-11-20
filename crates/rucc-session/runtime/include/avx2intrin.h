/* avx2intrin.h, the AVX2 intrinsics.
 *
 * Reached through <immintrin.h> the way gcc reaches it. This has every name gcc 16.2.0 has in its
 * own avx2intrin.h: the integer arithmetic on 32 byte vectors, the shifts, the shuffles and
 * permutes across the two halves, the broadcasts, the masked loads and stores, the shifts by a
 * vector of counts, the gathers and the reductions. See `tamnd/rucc#2322`.
 *
 * Each function is written the way <avxintrin.h> writes its own, with the shapes it defines: the
 * instruction gcc writes, in inline assembly through memory, and `vzeroupper` after an
 * instruction on `ymm` registers. A function that only moves lanes is plain C over the vector
 * types. Each one is built for `avx2`, and a call from a function that is not is refused the way
 * gcc refuses it.
 *
 * Three groups are different from gcc's.
 *
 * - A shift by an `int` count, such as `_mm256_slli_epi32`, is the shift by a count in an `xmm`
 *   register. gcc writes the immediate form when the count is a constant. Both forms give the same
 *   answer for every count, and the count does not have to be a constant here either.
 * - The gathers are plain C. Each lane whose mask has its top bit set is loaded from the base plus
 *   the index times the scale, and the other lanes come from the source. That is what the
 *   instruction does. The assembler has no row for a vector index yet.
 * - The reductions are plain C over the lanes, as gcc's are, with the arithmetic wrapping in the
 *   type of the lane.
 */

#ifndef __RUCC_AVX2INTRIN_H
#define __RUCC_AVX2INTRIN_H

#include <avxintrin.h>

/* A shift of each lane of `__x` by the count in the low 8 bytes of the 16 byte vector `__c`. */
#define __RUCC_AVX_SHIFT(__insn, __answer, __x, __c)                                               \
  __asm__(__RUCC_AVX_LOAD1 "vmovdqu (%2), %%xmm1\n\t" __insn " %%xmm1, %%ymm0, %%ymm0"             \
          __RUCC_AVX_STORE                                                                         \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "r"(&(__c))                                             \
          : "xmm0", "xmm1", "memory")

/* An instruction that reads a 16 byte vector and writes a 32 byte one, which is each widening
 * move. */
#define __RUCC_AVX_WIDEN(__insn, __answer, __x)                                                    \
  __asm__("vmovdqu (%1), %%xmm0\n\t" __insn " %%xmm0, %%ymm0" __RUCC_AVX_STORE                     \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x))                                                          \
          : "xmm0", "memory")

/* # Arithmetic
 *
 * Each of these is the instruction on every lane, with the first argument as the first source. The
 * `s` forms saturate, and the others wrap. */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_abs_epi8(__m256i __A)
{
  __m256i __R;
  __RUCC_AVX_OP1("vpabsb", __R, __A);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_abs_epi16(__m256i __A)
{
  __m256i __R;
  __RUCC_AVX_OP1("vpabsw", __R, __A);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_abs_epi32(__m256i __A)
{
  __m256i __R;
  __RUCC_AVX_OP1("vpabsd", __R, __A);
  return __R;
}


static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_add_epi8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpaddb", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_add_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpaddw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_add_epi32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpaddd", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_add_epi64(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpaddq", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_adds_epi8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpaddsb", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_adds_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpaddsw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_adds_epu8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpaddusb", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_adds_epu16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpaddusw", __R, __A, __B);
  return __R;
}


static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sub_epi8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsubb", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sub_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsubw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sub_epi32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsubd", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sub_epi64(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsubq", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_subs_epi8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsubsb", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_subs_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsubsw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_subs_epu8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsubusb", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_subs_epu16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsubusw", __R, __A, __B);
  return __R;
}


static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_avg_epu8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpavgb", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_avg_epu16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpavgw", __R, __A, __B);
  return __R;
}


/* The sums of adjacent pairs, the four from X and then the four from Y in each 16 byte half. */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_hadd_epi16(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vphaddw", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_hadd_epi32(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vphaddd", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_hadds_epi16(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vphaddsw", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_hsub_epi16(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vphsubw", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_hsub_epi32(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vphsubd", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_hsubs_epi16(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vphsubsw", __R, __X, __Y);
  return __R;
}


/* The products. `maddubs` multiplies unsigned bytes of X by signed bytes of Y. */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_maddubs_epi16(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmaddubsw", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_madd_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmaddwd", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mul_epi32(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmuldq", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mul_epu32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmuludq", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mulhrs_epi16(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmulhrsw", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mulhi_epu16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmulhuw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mulhi_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmulhw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mullo_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmullw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mullo_epi32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmulld", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sad_epu8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsadbw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mpsadbw_epu8(__m256i __X, __m256i __Y, const int __M)
{
  __m256i __R;
  __RUCC_AVX_OP2_IMM("vmpsadbw", __R, __X, __Y, __M);
  return __R;
}


static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_max_epi8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmaxsb", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_max_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmaxsw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_max_epi32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmaxsd", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_max_epu8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmaxub", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_max_epu16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmaxuw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_max_epu32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpmaxud", __R, __A, __B);
  return __R;
}


static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_min_epi8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpminsb", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_min_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpminsw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_min_epi32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpminsd", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_min_epu8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpminub", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_min_epu16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpminuw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_min_epu32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpminud", __R, __A, __B);
  return __R;
}


/* Each lane of X with the sign of the same lane of Y, or zero where that lane is zero. */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sign_epi8(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsignb", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sign_epi16(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsignw", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sign_epi32(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsignd", __R, __X, __Y);
  return __R;
}


/* # Bits and comparisons */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_and_si256(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpand", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_andnot_si256(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpandn", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_or_si256(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpor", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_xor_si256(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpxor", __R, __A, __B);
  return __R;
}


static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cmpeq_epi8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpcmpeqb", __R, __A, __B);
  return __R;
}

/* `_mm256_cmpeq_epi16`, `_mm256_movemask_epi8` and `_mm256_broadcastw_epi16` are plain C with no
 * target on a Microsoft row. The inline `wmemchr` and `wmemcmp` of the CRT's <wchar.h> call them
 * from a function that is not built for AVX2. See <intrin.h>. */

#ifdef _MSC_VER
static __inline__ __m256i __attribute__((__always_inline__))
_mm256_cmpeq_epi16(__m256i __A, __m256i __B)
{
  return (__m256i)((__v16hi)__A == (__v16hi)__B);
}
#else
static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cmpeq_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpcmpeqw", __R, __A, __B);
  return __R;
}
#endif

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cmpeq_epi32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpcmpeqd", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cmpeq_epi64(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpcmpeqq", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cmpgt_epi8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpcmpgtb", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cmpgt_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpcmpgtw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cmpgt_epi32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpcmpgtd", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cmpgt_epi64(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpcmpgtq", __R, __A, __B);
  return __R;
}


/* # Packing and unpacking */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_packs_epi32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpackssdw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_packs_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpacksswb", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_packus_epi32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpackusdw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_packus_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpackuswb", __R, __A, __B);
  return __R;
}


static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_unpackhi_epi8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpunpckhbw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_unpackhi_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpunpckhwd", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_unpackhi_epi32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpunpckhdq", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_unpackhi_epi64(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpunpckhqdq", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_unpacklo_epi8(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpunpcklbw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_unpacklo_epi16(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpunpcklwd", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_unpacklo_epi32(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpunpckldq", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_unpacklo_epi64(__m256i __A, __m256i __B)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpunpcklqdq", __R, __A, __B);
  return __R;
}


/* The widening moves, from the low lanes of a 16 byte vector. */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepi8_epi16(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovsxbw", __R, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepu8_epi16(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovzxbw", __R, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepi8_epi32(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovsxbd", __R, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepu8_epi32(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovzxbd", __R, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepi8_epi64(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovsxbq", __R, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepu8_epi64(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovzxbq", __R, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepi16_epi32(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovsxwd", __R, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepu16_epi32(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovzxwd", __R, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepi16_epi64(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovsxwq", __R, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepu16_epi64(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovzxwq", __R, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepi32_epi64(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovsxdq", __R, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_cvtepu32_epi64(__m128i __X)
{
  __m256i __R;
  __RUCC_AVX_WIDEN("vpmovzxdq", __R, __X);
  return __R;
}


/* # Blends and shuffles */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_blendv_epi8(__m256i __X, __m256i __Y, __m256i __M)
{
  __m256i __R;
  __asm__("vmovdqu (%1), %%ymm0\n\tvmovdqu (%2), %%ymm1\n\tvmovdqu (%3), %%ymm2\n\t"
          "vpblendvb %%ymm2, %%ymm1, %%ymm0, %%ymm0" __RUCC_AVX_STORE
          :
          : "r"(&__R), "r"(&__X), "r"(&__Y), "r"(&__M)
          : "xmm0", "xmm1", "xmm2", "memory");
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_blend_epi16(__m256i __X, __m256i __Y, const int __M)
{
  __m256i __R;
  __RUCC_AVX_OP2_IMM("vpblendw", __R, __X, __Y, __M);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_blend_epi32(__m256i __X, __m256i __Y, const int __M)
{
  __m256i __R;
  __RUCC_AVX_OP2_IMM("vpblendd", __R, __X, __Y, __M);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_blend_epi32(__m128i __X, __m128i __Y, const int __M)
{
  __m128i __R;
  __RUCC_AVX_OP2_IMM_128("vpblendd", __R, __X, __Y, __M);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_alignr_epi8(__m256i __A, __m256i __B, const int __N)
{
  __m256i __R;
  __RUCC_AVX_OP2_IMM("vpalignr", __R, __A, __B, __N);
  return __R;
}


static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_shuffle_epi8(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpshufb", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_shuffle_epi32(__m256i __A, const int __mask)
{
  __m256i __R;
  __RUCC_AVX_OP1_IMM("vpshufd", __R, __A, __mask);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_shufflehi_epi16(__m256i __A, const int __mask)
{
  __m256i __R;
  __RUCC_AVX_OP1_IMM("vpshufhw", __R, __A, __mask);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_shufflelo_epi16(__m256i __A, const int __mask)
{
  __m256i __R;
  __RUCC_AVX_OP1_IMM("vpshuflw", __R, __A, __mask);
  return __R;
}


/* The permutes across the two 16 byte halves. The control vector is the first source of the
 * instruction, so it goes in `ymm0`. */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_permutevar8x32_epi32(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpermd", __R, __Y, __X);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx2")))
_mm256_permutevar8x32_ps(__m256 __X, __m256i __Y)
{
  __m256 __R;
  __RUCC_AVX_OP2("vpermps", __R, __Y, __X);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_permute4x64_epi64(__m256i __X, const int __M)
{
  __m256i __R;
  __RUCC_AVX_OP1_IMM("vpermq", __R, __X, __M);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx2")))
_mm256_permute4x64_pd(__m256d __X, const int __M)
{
  __m256d __R;
  __RUCC_AVX_OP1_IMM("vpermpd", __R, __X, __M);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_permute2x128_si256(__m256i __X, __m256i __Y, const int __M)
{
  __m256i __R;
  __RUCC_AVX_OP2_IMM("vperm2i128", __R, __X, __Y, __M);
  return __R;
}


static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm256_extracti128_si256(__m256i __X, const int __M)
{
  __v4di __x = (__v4di)__X;
  int __i = (__M & 1) * 2;
  return (__m128i)(__v2di){ __x[__i], __x[__i + 1] };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_inserti128_si256(__m256i __X, __m128i __Y, const int __M)
{
  __v4di __x = (__v4di)__X;
  __v2di __y = (__v2di)__Y;
  int __i = (__M & 1) * 2;
  __x[__i] = __y[0];
  __x[__i + 1] = __y[1];
  return (__m256i)__x;
}

/* # Shifts
 *
 * A count past the width of the lane gives zero, or the sign in every bit for `sra`. The byte
 * shifts move each 16 byte half on its own. */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sll_epi16(__m256i __A, __m128i __B)
{
  __m256i __R;
  __RUCC_AVX_SHIFT("vpsllw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_slli_epi16(__m256i __A, int __B)
{
  __m256i __R;
  __m128i __C = _mm_cvtsi32_si128(__B);
  __RUCC_AVX_SHIFT("vpsllw", __R, __A, __C);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sll_epi32(__m256i __A, __m128i __B)
{
  __m256i __R;
  __RUCC_AVX_SHIFT("vpslld", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_slli_epi32(__m256i __A, int __B)
{
  __m256i __R;
  __m128i __C = _mm_cvtsi32_si128(__B);
  __RUCC_AVX_SHIFT("vpslld", __R, __A, __C);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sll_epi64(__m256i __A, __m128i __B)
{
  __m256i __R;
  __RUCC_AVX_SHIFT("vpsllq", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_slli_epi64(__m256i __A, int __B)
{
  __m256i __R;
  __m128i __C = _mm_cvtsi32_si128(__B);
  __RUCC_AVX_SHIFT("vpsllq", __R, __A, __C);
  return __R;
}


static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srl_epi16(__m256i __A, __m128i __B)
{
  __m256i __R;
  __RUCC_AVX_SHIFT("vpsrlw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srli_epi16(__m256i __A, int __B)
{
  __m256i __R;
  __m128i __C = _mm_cvtsi32_si128(__B);
  __RUCC_AVX_SHIFT("vpsrlw", __R, __A, __C);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srl_epi32(__m256i __A, __m128i __B)
{
  __m256i __R;
  __RUCC_AVX_SHIFT("vpsrld", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srli_epi32(__m256i __A, int __B)
{
  __m256i __R;
  __m128i __C = _mm_cvtsi32_si128(__B);
  __RUCC_AVX_SHIFT("vpsrld", __R, __A, __C);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srl_epi64(__m256i __A, __m128i __B)
{
  __m256i __R;
  __RUCC_AVX_SHIFT("vpsrlq", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srli_epi64(__m256i __A, int __B)
{
  __m256i __R;
  __m128i __C = _mm_cvtsi32_si128(__B);
  __RUCC_AVX_SHIFT("vpsrlq", __R, __A, __C);
  return __R;
}


static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sra_epi16(__m256i __A, __m128i __B)
{
  __m256i __R;
  __RUCC_AVX_SHIFT("vpsraw", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srai_epi16(__m256i __A, int __B)
{
  __m256i __R;
  __m128i __C = _mm_cvtsi32_si128(__B);
  __RUCC_AVX_SHIFT("vpsraw", __R, __A, __C);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sra_epi32(__m256i __A, __m128i __B)
{
  __m256i __R;
  __RUCC_AVX_SHIFT("vpsrad", __R, __A, __B);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srai_epi32(__m256i __A, int __B)
{
  __m256i __R;
  __m128i __C = _mm_cvtsi32_si128(__B);
  __RUCC_AVX_SHIFT("vpsrad", __R, __A, __C);
  return __R;
}


static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_bslli_epi128(__m256i __A, const int __N)
{
  __m256i __R;
  __RUCC_AVX_OP1_IMM("vpslldq", __R, __A, __N);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_slli_si256(__m256i __A, const int __N)
{
  __m256i __R;
  __RUCC_AVX_OP1_IMM("vpslldq", __R, __A, __N);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_bsrli_epi128(__m256i __A, const int __N)
{
  __m256i __R;
  __RUCC_AVX_OP1_IMM("vpsrldq", __R, __A, __N);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srli_si256(__m256i __A, const int __N)
{
  __m256i __R;
  __RUCC_AVX_OP1_IMM("vpsrldq", __R, __A, __N);
  return __R;
}


/* The shifts of each lane by the count in the same lane of Y. */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sllv_epi32(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsllvd", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_sllv_epi32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_AVX_OP2_128("vpsllvd", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_sllv_epi64(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsllvq", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_sllv_epi64(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_AVX_OP2_128("vpsllvq", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srav_epi32(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsravd", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_srav_epi32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_AVX_OP2_128("vpsravd", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srlv_epi32(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsrlvd", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_srlv_epi32(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_AVX_OP2_128("vpsrlvd", __R, __X, __Y);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_srlv_epi64(__m256i __X, __m256i __Y)
{
  __m256i __R;
  __RUCC_AVX_OP2("vpsrlvq", __R, __X, __Y);
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_srlv_epi64(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_AVX_OP2_128("vpsrlvq", __R, __X, __Y);
  return __R;
}


/* # Moves
 *
 * The sign bit of every byte, and the non-temporal load. */

#ifdef _MSC_VER
static __inline__ int __attribute__((__always_inline__))
_mm256_movemask_epi8(__m256i __A)
{
  __v32qu __from = (__v32qu)__A;
  unsigned int __answer = 0;
  int __i;
  for (__i = 0; __i < 32; __i++)
    __answer |= (unsigned int)(__from[__i] >> 7) << __i;
  return (int)__answer;
}
#else
static __inline__ int __attribute__((__always_inline__, __target__("avx2")))
_mm256_movemask_epi8(__m256i __A)
{
  int __R;
  __asm__ __volatile__("vmovdqu (%1), %%ymm0\n\tvpmovmskb %%ymm0, %0\n\tvzeroupper"
                       : "=r"(__R)
                       : "r"(&__A)
                       : "xmm0", "memory");
  return __R;
}
#endif

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_stream_load_si256(__m256i const *__X)
{
  __m256i __R;
  __asm__("vmovntdqa (%1), %%ymm0" __RUCC_AVX_STORE
          :
          : "r"(&__R), "r"(__X)
          : "xmm0", "memory");
  return __R;
}

/* The broadcasts. Each one copies the lowest lane of X, or all of X, to every place. */

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx2")))
_mm_broadcastss_ps(__m128 __X)
{
  float __a = ((__v4sf)__X)[0];
  return (__m128)(__v4sf){ __a, __a, __a, __a };
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx2")))
_mm256_broadcastss_ps(__m128 __X)
{
  float __a = ((__v4sf)__X)[0];
  return (__m256)(__v8sf){ __a, __a, __a, __a, __a, __a, __a, __a };
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx2")))
_mm256_broadcastsd_pd(__m128d __X)
{
  double __a = ((__v2df)__X)[0];
  return (__m256d)(__v4df){ __a, __a, __a, __a };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_broadcastsi128_si256(__m128i __X)
{
  __v2di __x = (__v2di)__X;
  return (__m256i)(__v4di){ __x[0], __x[1], __x[0], __x[1] };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_broadcastb_epi8(__m128i __X)
{
  char __a = ((__v16qi)__X)[0];
  __v32qi __r;
  int __i;
  for (__i = 0; __i < 32; __i++)
    __r[__i] = __a;
  return (__m256i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_broadcastb_epi8(__m128i __X)
{
  char __a = ((__v16qi)__X)[0];
  __v16qi __r;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __r[__i] = __a;
  return (__m128i)__r;
}

#ifdef _MSC_VER
static __inline__ __m256i __attribute__((__always_inline__))
_mm256_broadcastw_epi16(__m128i __X)
{
  short __a = ((__v8hi)__X)[0];
  __v16hi __r;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __r[__i] = __a;
  return (__m256i)__r;
}
#else
static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_broadcastw_epi16(__m128i __X)
{
  short __a = ((__v8hi)__X)[0];
  __v16hi __r;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __r[__i] = __a;
  return (__m256i)__r;
}
#endif

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_broadcastw_epi16(__m128i __X)
{
  short __a = ((__v8hi)__X)[0];
  __v8hi __r;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __r[__i] = __a;
  return (__m128i)__r;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_broadcastd_epi32(__m128i __X)
{
  int __a = ((__v4si)__X)[0];
  __v8si __r;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __r[__i] = __a;
  return (__m256i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_broadcastd_epi32(__m128i __X)
{
  int __a = ((__v4si)__X)[0];
  __v4si __r;
  int __i;
  for (__i = 0; __i < 4; __i++)
    __r[__i] = __a;
  return (__m128i)__r;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_broadcastq_epi64(__m128i __X)
{
  long long __a = ((__v2di)__X)[0];
  __v4di __r;
  int __i;
  for (__i = 0; __i < 4; __i++)
    __r[__i] = __a;
  return (__m256i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_broadcastq_epi64(__m128i __X)
{
  long long __a = ((__v2di)__X)[0];
  __v2di __r;
  int __i;
  for (__i = 0; __i < 2; __i++)
    __r[__i] = __a;
  return (__m128i)__r;
}

#define _mm_broadcastsi128_si256(X) _mm256_broadcastsi128_si256(X)
#define _mm_broadcastsd_pd(X) _mm_movedup_pd(X)

/* The masked loads and stores. A lane whose mask has its top bit clear is read as zero, and is
 * not written. */

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_maskload_epi32(int const *__X, __m256i __M)
{
  __m256i __R;
  __asm__("vmovdqu (%2), %%ymm1\n\tvpmaskmovd (%1), %%ymm1, %%ymm0\n\t"
          "vmovdqu %%ymm0, (%0)\n\tvzeroupper"
          :
          : "r"(&__R), "r"(__X), "r"(&__M)
          : "xmm0", "xmm1", "memory");
  return __R;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx2")))
_mm256_maskstore_epi32(int *__X, __m256i __M, __m256i __Y)
{
  __asm__("vmovdqu (%1), %%ymm1\n\tvmovdqu (%2), %%ymm0\n\t"
          "vpmaskmovd %%ymm0, %%ymm1, (%0)\n\tvzeroupper"
          :
          : "r"(__X), "r"(&__M), "r"(&__Y)
          : "xmm0", "xmm1", "memory");
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_maskload_epi64(long long const *__X, __m256i __M)
{
  __m256i __R;
  __asm__("vmovdqu (%2), %%ymm1\n\tvpmaskmovq (%1), %%ymm1, %%ymm0\n\t"
          "vmovdqu %%ymm0, (%0)\n\tvzeroupper"
          :
          : "r"(&__R), "r"(__X), "r"(&__M)
          : "xmm0", "xmm1", "memory");
  return __R;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx2")))
_mm256_maskstore_epi64(long long *__X, __m256i __M, __m256i __Y)
{
  __asm__("vmovdqu (%1), %%ymm1\n\tvmovdqu (%2), %%ymm0\n\t"
          "vpmaskmovq %%ymm0, %%ymm1, (%0)\n\tvzeroupper"
          :
          : "r"(__X), "r"(&__M), "r"(&__Y)
          : "xmm0", "xmm1", "memory");
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_maskload_epi32(int const *__X, __m128i __M)
{
  __m128i __R;
  __asm__("vmovdqu (%2), %%xmm1\n\tvpmaskmovd (%1), %%xmm1, %%xmm0\n\t"
          "vmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(__X), "r"(&__M)
          : "xmm0", "xmm1", "memory");
  return __R;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx2")))
_mm_maskstore_epi32(int *__X, __m128i __M, __m128i __Y)
{
  __asm__("vmovdqu (%1), %%xmm1\n\tvmovdqu (%2), %%xmm0\n\t"
          "vpmaskmovd %%xmm0, %%xmm1, (%0)"
          :
          : "r"(__X), "r"(&__M), "r"(&__Y)
          : "xmm0", "xmm1", "memory");
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_maskload_epi64(long long const *__X, __m128i __M)
{
  __m128i __R;
  __asm__("vmovdqu (%2), %%xmm1\n\tvpmaskmovq (%1), %%xmm1, %%xmm0\n\t"
          "vmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(__X), "r"(&__M)
          : "xmm0", "xmm1", "memory");
  return __R;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx2")))
_mm_maskstore_epi64(long long *__X, __m128i __M, __m128i __Y)
{
  __asm__("vmovdqu (%1), %%xmm1\n\tvmovdqu (%2), %%xmm0\n\t"
          "vpmaskmovq %%xmm0, %%xmm1, (%0)"
          :
          : "r"(__X), "r"(&__M), "r"(&__Y)
          : "xmm0", "xmm1", "memory");
}

/* # Gathers
 *
 * Lane I of the answer is read from BASE plus lane I of INDEX times SCALE bytes. The `mask`
 * forms read only the lanes whose mask has its top bit set, and take the others from SRC. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("avx2")))
_mm_i32gather_pd(double const *__base, __m128i __index, const int __scale)
{
  __v2df __r;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    __r[__k] = *(double const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128d)__r;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("avx2")))
_mm_mask_i32gather_pd(__m128d __src, double const *__base, __m128i __index, __m128d __mask,
                      const int __scale)
{
  __v2df __r = (__v2df)__src;
  __v2di __m = (__v2di)__mask;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(double const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128d)__r;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx2")))
_mm256_i32gather_pd(double const *__base, __m128i __index, const int __scale)
{
  __v4df __r;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    __r[__k] = *(double const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256d)__r;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx2")))
_mm256_mask_i32gather_pd(__m256d __src, double const *__base, __m128i __index, __m256d __mask,
                         const int __scale)
{
  __v4df __r = (__v4df)__src;
  __v4di __m = (__v4di)__mask;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(double const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256d)__r;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("avx2")))
_mm_i64gather_pd(double const *__base, __m128i __index, const int __scale)
{
  __v2df __r;
  __v2di __i = (__v2di)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    __r[__k] = *(double const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128d)__r;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("avx2")))
_mm_mask_i64gather_pd(__m128d __src, double const *__base, __m128i __index, __m128d __mask,
                      const int __scale)
{
  __v2df __r = (__v2df)__src;
  __v2di __m = (__v2di)__mask;
  __v2di __i = (__v2di)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(double const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128d)__r;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx2")))
_mm256_i64gather_pd(double const *__base, __m256i __index, const int __scale)
{
  __v4df __r;
  __v4di __i = (__v4di)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    __r[__k] = *(double const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256d)__r;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx2")))
_mm256_mask_i64gather_pd(__m256d __src, double const *__base, __m256i __index, __m256d __mask,
                         const int __scale)
{
  __v4df __r = (__v4df)__src;
  __v4di __m = (__v4di)__mask;
  __v4di __i = (__v4di)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(double const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256d)__r;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx2")))
_mm_i32gather_ps(float const *__base, __m128i __index, const int __scale)
{
  __v4sf __r;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    __r[__k] = *(float const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128)__r;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx2")))
_mm_mask_i32gather_ps(__m128 __src, float const *__base, __m128i __index, __m128 __mask,
                      const int __scale)
{
  __v4sf __r = (__v4sf)__src;
  __v4si __m = (__v4si)__mask;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(float const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128)__r;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx2")))
_mm256_i32gather_ps(float const *__base, __m256i __index, const int __scale)
{
  __v8sf __r;
  __v8si __i = (__v8si)__index;
  int __k;
  for (__k = 0; __k < 8; __k++)
    __r[__k] = *(float const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256)__r;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx2")))
_mm256_mask_i32gather_ps(__m256 __src, float const *__base, __m256i __index, __m256 __mask,
                         const int __scale)
{
  __v8sf __r = (__v8sf)__src;
  __v8si __m = (__v8si)__mask;
  __v8si __i = (__v8si)__index;
  int __k;
  for (__k = 0; __k < 8; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(float const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256)__r;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx2")))
_mm_i64gather_ps(float const *__base, __m128i __index, const int __scale)
{
  __v4sf __r = { 0 };
  __v2di __i = (__v2di)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    __r[__k] = *(float const *)((char const *)__base + __i[__k] * __scale);
  return (__m128)__r;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx2")))
_mm_mask_i64gather_ps(__m128 __src, float const *__base, __m128i __index, __m128 __mask,
                      const int __scale)
{
  __v4sf __r = (__v4sf)__src;
  __v4si __m = (__v4si)__mask;
  __v2di __i = (__v2di)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(float const *)((char const *)__base + __i[__k] * __scale);
  for (; __k < 4; __k++)
    __r[__k] = 0;
  return (__m128)__r;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx2")))
_mm256_i64gather_ps(float const *__base, __m256i __index, const int __scale)
{
  __v4sf __r = { 0 };
  __v4di __i = (__v4di)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    __r[__k] = *(float const *)((char const *)__base + __i[__k] * __scale);
  return (__m128)__r;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx2")))
_mm256_mask_i64gather_ps(__m128 __src, float const *__base, __m256i __index, __m128 __mask,
                         const int __scale)
{
  __v4sf __r = (__v4sf)__src;
  __v4si __m = (__v4si)__mask;
  __v4di __i = (__v4di)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(float const *)((char const *)__base + __i[__k] * __scale);
  for (; __k < 4; __k++)
    __r[__k] = 0;
  return (__m128)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_i32gather_epi64(long long const *__base, __m128i __index, const int __scale)
{
  __v2di __r;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    __r[__k] = *(long long const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_mask_i32gather_epi64(__m128i __src, long long const *__base, __m128i __index, __m128i __mask,
                         const int __scale)
{
  __v2di __r = (__v2di)__src;
  __v2di __m = (__v2di)__mask;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(long long const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128i)__r;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_i32gather_epi64(long long const *__base, __m128i __index, const int __scale)
{
  __v4di __r;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    __r[__k] = *(long long const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256i)__r;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mask_i32gather_epi64(__m256i __src, long long const *__base, __m128i __index, __m256i __mask,
                            const int __scale)
{
  __v4di __r = (__v4di)__src;
  __v4di __m = (__v4di)__mask;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(long long const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_i64gather_epi64(long long const *__base, __m128i __index, const int __scale)
{
  __v2di __r;
  __v2di __i = (__v2di)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    __r[__k] = *(long long const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_mask_i64gather_epi64(__m128i __src, long long const *__base, __m128i __index, __m128i __mask,
                         const int __scale)
{
  __v2di __r = (__v2di)__src;
  __v2di __m = (__v2di)__mask;
  __v2di __i = (__v2di)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(long long const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128i)__r;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_i64gather_epi64(long long const *__base, __m256i __index, const int __scale)
{
  __v4di __r;
  __v4di __i = (__v4di)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    __r[__k] = *(long long const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256i)__r;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mask_i64gather_epi64(__m256i __src, long long const *__base, __m256i __index, __m256i __mask,
                            const int __scale)
{
  __v4di __r = (__v4di)__src;
  __v4di __m = (__v4di)__mask;
  __v4di __i = (__v4di)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(long long const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_i32gather_epi32(int const *__base, __m128i __index, const int __scale)
{
  __v4si __r;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    __r[__k] = *(int const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_mask_i32gather_epi32(__m128i __src, int const *__base, __m128i __index, __m128i __mask,
                         const int __scale)
{
  __v4si __r = (__v4si)__src;
  __v4si __m = (__v4si)__mask;
  __v4si __i = (__v4si)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(int const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m128i)__r;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_i32gather_epi32(int const *__base, __m256i __index, const int __scale)
{
  __v8si __r;
  __v8si __i = (__v8si)__index;
  int __k;
  for (__k = 0; __k < 8; __k++)
    __r[__k] = *(int const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256i)__r;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mask_i32gather_epi32(__m256i __src, int const *__base, __m256i __index, __m256i __mask,
                            const int __scale)
{
  __v8si __r = (__v8si)__src;
  __v8si __m = (__v8si)__mask;
  __v8si __i = (__v8si)__index;
  int __k;
  for (__k = 0; __k < 8; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(int const *)((char const *)__base + (long long)__i[__k] * __scale);
  return (__m256i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_i64gather_epi32(int const *__base, __m128i __index, const int __scale)
{
  __v4si __r = { 0 };
  __v2di __i = (__v2di)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    __r[__k] = *(int const *)((char const *)__base + __i[__k] * __scale);
  return (__m128i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm_mask_i64gather_epi32(__m128i __src, int const *__base, __m128i __index, __m128i __mask,
                         const int __scale)
{
  __v4si __r = (__v4si)__src;
  __v4si __m = (__v4si)__mask;
  __v2di __i = (__v2di)__index;
  int __k;
  for (__k = 0; __k < 2; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(int const *)((char const *)__base + __i[__k] * __scale);
  for (; __k < 4; __k++)
    __r[__k] = 0;
  return (__m128i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm256_i64gather_epi32(int const *__base, __m256i __index, const int __scale)
{
  __v4si __r = { 0 };
  __v4di __i = (__v4di)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    __r[__k] = *(int const *)((char const *)__base + __i[__k] * __scale);
  return (__m128i)__r;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx2")))
_mm256_mask_i64gather_epi32(__m128i __src, int const *__base, __m256i __index, __m128i __mask,
                            const int __scale)
{
  __v4si __r = (__v4si)__src;
  __v4si __m = (__v4si)__mask;
  __v4di __i = (__v4di)__index;
  int __k;
  for (__k = 0; __k < 4; __k++)
    if (__m[__k] < 0)
      __r[__k] = *(int const *)((char const *)__base + __i[__k] * __scale);
  for (; __k < 4; __k++)
    __r[__k] = 0;
  return (__m128i)__r;
}

/* # Reductions
 *
 * The add, multiply, and, or, max and min of every lane. */

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_add_epi16(__m128i __W)
{
  __v8hi __w = (__v8hi)__W;
  unsigned short __r = 0;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __r = (unsigned short)(__r + (unsigned short)__w[__i]);
  return (short)__r;
}

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_mul_epi16(__m128i __W)
{
  __v8hi __w = (__v8hi)__W;
  unsigned short __r = 1;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __r = (unsigned short)(__r * (unsigned short)__w[__i]);
  return (short)__r;
}

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_and_epi16(__m128i __W)
{
  __v8hi __w = (__v8hi)__W;
  unsigned short __r = (unsigned short)-1;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __r = (unsigned short)(__r & (unsigned short)__w[__i]);
  return (short)__r;
}

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_or_epi16(__m128i __W)
{
  __v8hi __w = (__v8hi)__W;
  unsigned short __r = 0;
  int __i;
  for (__i = 0; __i < 8; __i++)
    __r = (unsigned short)(__r | (unsigned short)__w[__i]);
  return (short)__r;
}

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_max_epi16(__m128i __V)
{
  __v8hi __v = (__v8hi)__V;
  short __r = (short)__v[0];
  int __i;
  for (__i = 1; __i < 8; __i++)
    if ((short)__v[__i] > __r)
      __r = (short)__v[__i];
  return __r;
}

static __inline__ unsigned short __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_max_epu16(__m128i __V)
{
  __v8hi __v = (__v8hi)__V;
  unsigned short __r = (unsigned short)__v[0];
  int __i;
  for (__i = 1; __i < 8; __i++)
    if ((unsigned short)__v[__i] > __r)
      __r = (unsigned short)__v[__i];
  return __r;
}

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_min_epi16(__m128i __V)
{
  __v8hi __v = (__v8hi)__V;
  short __r = (short)__v[0];
  int __i;
  for (__i = 1; __i < 8; __i++)
    if ((short)__v[__i] < __r)
      __r = (short)__v[__i];
  return __r;
}

static __inline__ unsigned short __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_min_epu16(__m128i __V)
{
  __v8hi __v = (__v8hi)__V;
  unsigned short __r = (unsigned short)__v[0];
  int __i;
  for (__i = 1; __i < 8; __i++)
    if ((unsigned short)__v[__i] < __r)
      __r = (unsigned short)__v[__i];
  return __r;
}

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_add_epi16(__m256i __W)
{
  __v16hi __w = (__v16hi)__W;
  unsigned short __r = 0;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __r = (unsigned short)(__r + (unsigned short)__w[__i]);
  return (short)__r;
}

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_mul_epi16(__m256i __W)
{
  __v16hi __w = (__v16hi)__W;
  unsigned short __r = 1;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __r = (unsigned short)(__r * (unsigned short)__w[__i]);
  return (short)__r;
}

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_and_epi16(__m256i __W)
{
  __v16hi __w = (__v16hi)__W;
  unsigned short __r = (unsigned short)-1;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __r = (unsigned short)(__r & (unsigned short)__w[__i]);
  return (short)__r;
}

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_or_epi16(__m256i __W)
{
  __v16hi __w = (__v16hi)__W;
  unsigned short __r = 0;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __r = (unsigned short)(__r | (unsigned short)__w[__i]);
  return (short)__r;
}

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_max_epi16(__m256i __V)
{
  __v16hi __v = (__v16hi)__V;
  short __r = (short)__v[0];
  int __i;
  for (__i = 1; __i < 16; __i++)
    if ((short)__v[__i] > __r)
      __r = (short)__v[__i];
  return __r;
}

static __inline__ unsigned short __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_max_epu16(__m256i __V)
{
  __v16hi __v = (__v16hi)__V;
  unsigned short __r = (unsigned short)__v[0];
  int __i;
  for (__i = 1; __i < 16; __i++)
    if ((unsigned short)__v[__i] > __r)
      __r = (unsigned short)__v[__i];
  return __r;
}

static __inline__ short __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_min_epi16(__m256i __V)
{
  __v16hi __v = (__v16hi)__V;
  short __r = (short)__v[0];
  int __i;
  for (__i = 1; __i < 16; __i++)
    if ((short)__v[__i] < __r)
      __r = (short)__v[__i];
  return __r;
}

static __inline__ unsigned short __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_min_epu16(__m256i __V)
{
  __v16hi __v = (__v16hi)__V;
  unsigned short __r = (unsigned short)__v[0];
  int __i;
  for (__i = 1; __i < 16; __i++)
    if ((unsigned short)__v[__i] < __r)
      __r = (unsigned short)__v[__i];
  return __r;
}

static __inline__ char __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_add_epi8(__m128i __W)
{
  __v16qi __w = (__v16qi)__W;
  unsigned char __r = 0;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __r = (unsigned char)(__r + (unsigned char)__w[__i]);
  return (char)__r;
}

static __inline__ char __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_mul_epi8(__m128i __W)
{
  __v16qi __w = (__v16qi)__W;
  unsigned char __r = 1;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __r = (unsigned char)(__r * (unsigned char)__w[__i]);
  return (char)__r;
}

static __inline__ char __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_and_epi8(__m128i __W)
{
  __v16qi __w = (__v16qi)__W;
  unsigned char __r = (unsigned char)-1;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __r = (unsigned char)(__r & (unsigned char)__w[__i]);
  return (char)__r;
}

static __inline__ char __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_or_epi8(__m128i __W)
{
  __v16qi __w = (__v16qi)__W;
  unsigned char __r = 0;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __r = (unsigned char)(__r | (unsigned char)__w[__i]);
  return (char)__r;
}

static __inline__ signed char __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_max_epi8(__m128i __V)
{
  __v16qi __v = (__v16qi)__V;
  signed char __r = (signed char)__v[0];
  int __i;
  for (__i = 1; __i < 16; __i++)
    if ((signed char)__v[__i] > __r)
      __r = (signed char)__v[__i];
  return __r;
}

static __inline__ unsigned char __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_max_epu8(__m128i __V)
{
  __v16qi __v = (__v16qi)__V;
  unsigned char __r = (unsigned char)__v[0];
  int __i;
  for (__i = 1; __i < 16; __i++)
    if ((unsigned char)__v[__i] > __r)
      __r = (unsigned char)__v[__i];
  return __r;
}

static __inline__ signed char __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_min_epi8(__m128i __V)
{
  __v16qi __v = (__v16qi)__V;
  signed char __r = (signed char)__v[0];
  int __i;
  for (__i = 1; __i < 16; __i++)
    if ((signed char)__v[__i] < __r)
      __r = (signed char)__v[__i];
  return __r;
}

static __inline__ unsigned char __attribute__((__always_inline__, __target__("avx2")))
_mm_reduce_min_epu8(__m128i __V)
{
  __v16qi __v = (__v16qi)__V;
  unsigned char __r = (unsigned char)__v[0];
  int __i;
  for (__i = 1; __i < 16; __i++)
    if ((unsigned char)__v[__i] < __r)
      __r = (unsigned char)__v[__i];
  return __r;
}

static __inline__ char __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_add_epi8(__m256i __W)
{
  __v32qi __w = (__v32qi)__W;
  unsigned char __r = 0;
  int __i;
  for (__i = 0; __i < 32; __i++)
    __r = (unsigned char)(__r + (unsigned char)__w[__i]);
  return (char)__r;
}

static __inline__ char __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_mul_epi8(__m256i __W)
{
  __v32qi __w = (__v32qi)__W;
  unsigned char __r = 1;
  int __i;
  for (__i = 0; __i < 32; __i++)
    __r = (unsigned char)(__r * (unsigned char)__w[__i]);
  return (char)__r;
}

static __inline__ char __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_and_epi8(__m256i __W)
{
  __v32qi __w = (__v32qi)__W;
  unsigned char __r = (unsigned char)-1;
  int __i;
  for (__i = 0; __i < 32; __i++)
    __r = (unsigned char)(__r & (unsigned char)__w[__i]);
  return (char)__r;
}

static __inline__ char __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_or_epi8(__m256i __W)
{
  __v32qi __w = (__v32qi)__W;
  unsigned char __r = 0;
  int __i;
  for (__i = 0; __i < 32; __i++)
    __r = (unsigned char)(__r | (unsigned char)__w[__i]);
  return (char)__r;
}

static __inline__ signed char __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_max_epi8(__m256i __V)
{
  __v32qi __v = (__v32qi)__V;
  signed char __r = (signed char)__v[0];
  int __i;
  for (__i = 1; __i < 32; __i++)
    if ((signed char)__v[__i] > __r)
      __r = (signed char)__v[__i];
  return __r;
}

static __inline__ unsigned char __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_max_epu8(__m256i __V)
{
  __v32qi __v = (__v32qi)__V;
  unsigned char __r = (unsigned char)__v[0];
  int __i;
  for (__i = 1; __i < 32; __i++)
    if ((unsigned char)__v[__i] > __r)
      __r = (unsigned char)__v[__i];
  return __r;
}

static __inline__ signed char __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_min_epi8(__m256i __V)
{
  __v32qi __v = (__v32qi)__V;
  signed char __r = (signed char)__v[0];
  int __i;
  for (__i = 1; __i < 32; __i++)
    if ((signed char)__v[__i] < __r)
      __r = (signed char)__v[__i];
  return __r;
}

static __inline__ unsigned char __attribute__((__always_inline__, __target__("avx2")))
_mm256_reduce_min_epu8(__m256i __V)
{
  __v32qi __v = (__v32qi)__V;
  unsigned char __r = (unsigned char)__v[0];
  int __i;
  for (__i = 1; __i < 32; __i++)
    if ((unsigned char)__v[__i] < __r)
      __r = (unsigned char)__v[__i];
  return __r;
}

#endif /* __RUCC_AVX2INTRIN_H */
