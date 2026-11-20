/* avxintrin.h, the AVX intrinsics.
 *
 * Reached through <immintrin.h> the way gcc reaches it. This has every name gcc 16.2.0 has in its
 * own avxintrin.h: the 32 byte vector types, the arithmetic on 32 byte vectors of floats and
 * doubles, the loads and stores, the conversions, the moves between a 16 byte and a 32 byte
 * vector, and the four comparisons by predicate that gcc puts in <xmmintrin.h> and <emmintrin.h>.
 * See `tamnd/rucc#2322`.
 *
 * # How each one is written
 *
 * The work is the instruction gcc writes, in inline assembly through memory, the way
 * <pmmintrin.h> does it for SSE3 and up. A 32 byte vector lives in memory in this compiler, so the
 * assembly gets the addresses of the operands and of the answer. It loads them into `ymm0` and
 * `ymm1`, runs the instruction and stores `ymm0`. The register allocator only uses the low 16
 * bytes of a vector register, so each block ends with `vzeroupper`. That keeps the SSE code around
 * it from the penalty for mixing SSE and AVX, and it changes no value the compiler keeps in a
 * register.
 *
 * A function that only moves lanes, such as a set, a cast, a load or a store, is plain C over the
 * vector types. A function with an immediate takes it as a parameter, and the parameter is a
 * constant in each inlined copy, as in <pmmintrin.h>.
 *
 * Each function is built for `avx`. A call from a function that is not built for it is refused
 * the way gcc refuses it.
 *
 * # The four comparisons of 16 byte vectors
 *
 * `_mm_cmp_ps`, `_mm_cmp_ss`, `_mm_cmp_pd` and `_mm_cmp_sd` are in <xmmintrin.h> and
 * <emmintrin.h>, as in gcc 16. They are plain C there and take all thirty two predicates. The
 * `_CMP_` predicate names are here.
 */

#ifndef __RUCC_AVXINTRIN_H
#define __RUCC_AVXINTRIN_H

#include <smmintrin.h>

typedef double __v4df __attribute__((__vector_size__(32)));
typedef float __v8sf __attribute__((__vector_size__(32)));
typedef long long __v4di __attribute__((__vector_size__(32)));
typedef unsigned long long __v4du __attribute__((__vector_size__(32)));
typedef int __v8si __attribute__((__vector_size__(32)));
typedef unsigned int __v8su __attribute__((__vector_size__(32)));
typedef short __v16hi __attribute__((__vector_size__(32)));
typedef unsigned short __v16hu __attribute__((__vector_size__(32)));
typedef char __v32qi __attribute__((__vector_size__(32)));
typedef signed char __v32qs __attribute__((__vector_size__(32)));
typedef unsigned char __v32qu __attribute__((__vector_size__(32)));

typedef float __m256 __attribute__((__vector_size__(32), __may_alias__));
typedef long long __m256i __attribute__((__vector_size__(32), __may_alias__));
typedef double __m256d __attribute__((__vector_size__(32), __may_alias__));

typedef float __m256_u __attribute__((__vector_size__(32), __may_alias__, __aligned__(1)));
typedef long long __m256i_u __attribute__((__vector_size__(32), __may_alias__, __aligned__(1)));
typedef double __m256d_u __attribute__((__vector_size__(32), __may_alias__, __aligned__(1)));

/* The predicates of the comparisons, gcc's values under gcc's names. */
#define _CMP_EQ_OQ 0x00
#define _CMP_LT_OS 0x01
#define _CMP_LE_OS 0x02
#define _CMP_UNORD_Q 0x03
#define _CMP_NEQ_UQ 0x04
#define _CMP_NLT_US 0x05
#define _CMP_NLE_US 0x06
#define _CMP_ORD_Q 0x07
#define _CMP_EQ_UQ 0x08
#define _CMP_NGE_US 0x09
#define _CMP_NGT_US 0x0a
#define _CMP_FALSE_OQ 0x0b
#define _CMP_NEQ_OQ 0x0c
#define _CMP_GE_OS 0x0d
#define _CMP_GT_OS 0x0e
#define _CMP_TRUE_UQ 0x0f
#define _CMP_EQ_OS 0x10
#define _CMP_LT_OQ 0x11
#define _CMP_LE_OQ 0x12
#define _CMP_UNORD_S 0x13
#define _CMP_NEQ_US 0x14
#define _CMP_NLT_UQ 0x15
#define _CMP_NLE_UQ 0x16
#define _CMP_ORD_S 0x17
#define _CMP_EQ_US 0x18
#define _CMP_NGE_UQ 0x19
#define _CMP_NGT_UQ 0x1a
#define _CMP_FALSE_OS 0x1b
#define _CMP_NEQ_OS 0x1c
#define _CMP_GE_OQ 0x1d
#define _CMP_GT_OQ 0x1e
#define _CMP_TRUE_US 0x1f

/* The shapes of the assembly. `__RUCC_AVX_OP1` loads one vector into `ymm0`, runs the instruction
 * on it in place and stores it. `__RUCC_AVX_OP2` loads a second one into `ymm1` and runs the
 * instruction with `ymm0` and `ymm1` as its two sources, which is `__insn __y, __x, __x` in the
 * order the machine writes it. The `_IMM` shapes put the immediate in front. */
#define __RUCC_AVX_LOAD1 "vmovdqu (%1), %%ymm0\n\t"
#define __RUCC_AVX_LOAD2 "vmovdqu (%1), %%ymm0\n\tvmovdqu (%2), %%ymm1\n\t"
#define __RUCC_AVX_STORE "\n\tvmovdqu %%ymm0, (%0)\n\tvzeroupper"

#define __RUCC_AVX_OP1(__insn, __answer, __x)                                                      \
  __asm__(__RUCC_AVX_LOAD1 __insn " %%ymm0, %%ymm0" __RUCC_AVX_STORE                               \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x))                                                          \
          : "xmm0", "memory")

#define __RUCC_AVX_OP2(__insn, __answer, __x, __y)                                                 \
  __asm__(__RUCC_AVX_LOAD2 __insn " %%ymm1, %%ymm0, %%ymm0" __RUCC_AVX_STORE                       \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "r"(&(__y))                                             \
          : "xmm0", "xmm1", "memory")

#define __RUCC_AVX_OP1_IMM(__insn, __answer, __x, __imm)                                           \
  __asm__(__RUCC_AVX_LOAD1 __insn " %2, %%ymm0, %%ymm0" __RUCC_AVX_STORE                           \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "i"(__imm)                                              \
          : "xmm0", "memory")

#define __RUCC_AVX_OP2_IMM(__insn, __answer, __x, __y, __imm)                                      \
  __asm__(__RUCC_AVX_LOAD2 __insn " %3, %%ymm1, %%ymm0, %%ymm0" __RUCC_AVX_STORE                   \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "r"(&(__y)), "i"(__imm)                                 \
          : "xmm0", "xmm1", "memory")

/* The same with 16 byte vectors in `xmm0` and `xmm1`. A VEX instruction on an `xmm` register
 * clears the upper half, so these need no `vzeroupper`. */
#define __RUCC_AVX_OP2_128(__insn, __answer, __x, __y)                                             \
  __asm__("vmovdqu (%1), %%xmm0\n\tvmovdqu (%2), %%xmm1\n\t" __insn                                \
          " %%xmm1, %%xmm0, %%xmm0\n\tvmovdqu %%xmm0, (%0)"                                        \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "r"(&(__y))                                             \
          : "xmm0", "xmm1", "memory")

#define __RUCC_AVX_OP1_IMM_128(__insn, __answer, __x, __imm)                                       \
  __asm__("vmovdqu (%1), %%xmm0\n\t" __insn " %2, %%xmm0, %%xmm0\n\tvmovdqu %%xmm0, (%0)"          \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "i"(__imm)                                              \
          : "xmm0", "memory")

#define __RUCC_AVX_OP2_IMM_128(__insn, __answer, __x, __y, __imm)                                  \
  __asm__("vmovdqu (%1), %%xmm0\n\tvmovdqu (%2), %%xmm1\n\t" __insn                                \
          " %3, %%xmm1, %%xmm0, %%xmm0\n\tvmovdqu %%xmm0, (%0)"                                    \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "r"(&(__y)), "i"(__imm)                                 \
          : "xmm0", "xmm1", "memory")

/* The flag shape: the two vectors loaded, the test run, and the byte the `set` writes widened to
 * the `int` gcc answers with. `__w` is `x` or `y`, for the width. */
#define __RUCC_AVX_FLAG2(__insn, __set, __w, __answer, __x, __y)                                   \
  __asm__ __volatile__("vmovdqu (%1), %%" __w "mm0\n\tvmovdqu (%2), %%" __w "mm1\n\t" __insn       \
                       " %%" __w "mm1, %%" __w "mm0\n\t" __set " %0\n\tvzeroupper"                 \
                       : "=r"(__answer)                                                            \
                       : "r"(&(__x)), "r"(&(__y))                                                  \
                       : "xmm0", "xmm1", "cc", "memory")

/* # Arithmetic
 *
 * Each of these is the instruction on every lane, with the first argument as the first source. */

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_add_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vaddpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_add_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vaddps", __R, __A, __B);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_addsub_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vaddsubpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_addsub_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vaddsubps", __R, __A, __B);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_and_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vandpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_and_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vandps", __R, __A, __B);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_andnot_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vandnpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_andnot_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vandnps", __R, __A, __B);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_div_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vdivpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_div_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vdivps", __R, __A, __B);
  return __R;
}

/* Bit 0 of M says which argument the lowest lane comes from, bit 1 the next one, and so on. */

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_blend_pd(__m256d __X, __m256d __Y, const int __M)
{
  __m256d __R;
  __RUCC_AVX_OP2_IMM("vblendpd", __R, __X, __Y, __M);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_blend_ps(__m256 __X, __m256 __Y, const int __M)
{
  __m256 __R;
  __RUCC_AVX_OP2_IMM("vblendps", __R, __X, __Y, __M);
  return __R;
}

/* The top bit of each lane of M says which argument that lane comes from. */

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_blendv_pd(__m256d __X, __m256d __Y, __m256d __M)
{
  __m256d __R;
  __asm__("vmovdqu (%1), %%ymm0\n\tvmovdqu (%2), %%ymm1\n\tvmovdqu (%3), %%ymm2\n\t"
          "vblendvpd %%ymm2, %%ymm1, %%ymm0, %%ymm0" __RUCC_AVX_STORE
          :
          : "r"(&__R), "r"(&__X), "r"(&__Y), "r"(&__M)
          : "xmm0", "xmm1", "xmm2", "memory");
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_blendv_ps(__m256 __X, __m256 __Y, __m256 __M)
{
  __m256 __R;
  __asm__("vmovdqu (%1), %%ymm0\n\tvmovdqu (%2), %%ymm1\n\tvmovdqu (%3), %%ymm2\n\t"
          "vblendvps %%ymm2, %%ymm1, %%ymm0, %%ymm0" __RUCC_AVX_STORE
          :
          : "r"(&__R), "r"(&__X), "r"(&__Y), "r"(&__M)
          : "xmm0", "xmm1", "xmm2", "memory");
  return __R;
}

/* The dot product of each 16 byte half, with the lanes M selects. */

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_dp_ps(__m256 __X, __m256 __Y, const int __M)
{
  __m256 __R;
  __RUCC_AVX_OP2_IMM("vdpps", __R, __X, __Y, __M);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_hadd_pd(__m256d __X, __m256d __Y)
{
  __m256d __R;
  __RUCC_AVX_OP2("vhaddpd", __R, __X, __Y);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_hadd_ps(__m256 __X, __m256 __Y)
{
  __m256 __R;
  __RUCC_AVX_OP2("vhaddps", __R, __X, __Y);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_hsub_pd(__m256d __X, __m256d __Y)
{
  __m256d __R;
  __RUCC_AVX_OP2("vhsubpd", __R, __X, __Y);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_hsub_ps(__m256 __X, __m256 __Y)
{
  __m256 __R;
  __RUCC_AVX_OP2("vhsubps", __R, __X, __Y);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_max_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vmaxpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_max_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vmaxps", __R, __A, __B);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_min_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vminpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_min_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vminps", __R, __A, __B);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_mul_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vmulpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_mul_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vmulps", __R, __A, __B);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_or_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vorpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_or_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vorps", __R, __A, __B);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_shuffle_pd(__m256d __X, __m256d __Y, const int __mask)
{
  __m256d __R;
  __RUCC_AVX_OP2_IMM("vshufpd", __R, __X, __Y, __mask);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_shuffle_ps(__m256 __X, __m256 __Y, const int __mask)
{
  __m256 __R;
  __RUCC_AVX_OP2_IMM("vshufps", __R, __X, __Y, __mask);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_sub_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vsubpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_sub_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vsubps", __R, __A, __B);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_xor_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vxorpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_xor_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vxorps", __R, __A, __B);
  return __R;
}

/* # Comparisons
 *
 * Each lane of the answer is all ones where the predicate P holds and zero where it does not. */

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_cmp_pd(__m256d __X, __m256d __Y, const int __P)
{
  __m256d __R;
  __RUCC_AVX_OP2_IMM("vcmppd", __R, __X, __Y, __P);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_cmp_ps(__m256 __X, __m256 __Y, const int __P)
{
  __m256 __R;
  __RUCC_AVX_OP2_IMM("vcmpps", __R, __X, __Y, __P);
  return __R;
}

/* # Conversions */

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_cvtsi256_si32(__m256i __A)
{
  return ((__v8si)__A)[0];
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_cvtepi32_pd(__m128i __A)
{
  __m256d __R;
  __asm__("vmovdqu (%1), %%xmm0\n\tvcvtdq2pd %%xmm0, %%ymm0" __RUCC_AVX_STORE
          :
          : "r"(&__R), "r"(&__A)
          : "xmm0", "memory");
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_cvtepi32_ps(__m256i __A)
{
  __m256 __R;
  __RUCC_AVX_OP1("vcvtdq2ps", __R, __A);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx")))
_mm256_cvtpd_ps(__m256d __A)
{
  __m128 __R;
  __asm__(__RUCC_AVX_LOAD1 "vcvtpd2ps %%ymm0, %%xmm0\n\tvmovdqu %%xmm0, (%0)\n\tvzeroupper"
          :
          : "r"(&__R), "r"(&__A)
          : "xmm0", "memory");
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_cvtps_epi32(__m256 __A)
{
  __m256i __R;
  __RUCC_AVX_OP1("vcvtps2dq", __R, __A);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_cvtps_pd(__m128 __A)
{
  __m256d __R;
  __asm__("vmovdqu (%1), %%xmm0\n\tvcvtps2pd %%xmm0, %%ymm0" __RUCC_AVX_STORE
          :
          : "r"(&__R), "r"(&__A)
          : "xmm0", "memory");
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx")))
_mm256_cvttpd_epi32(__m256d __A)
{
  __m128i __R;
  __asm__(__RUCC_AVX_LOAD1 "vcvttpd2dq %%ymm0, %%xmm0\n\tvmovdqu %%xmm0, (%0)\n\tvzeroupper"
          :
          : "r"(&__R), "r"(&__A)
          : "xmm0", "memory");
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx")))
_mm256_cvtpd_epi32(__m256d __A)
{
  __m128i __R;
  __asm__(__RUCC_AVX_LOAD1 "vcvtpd2dq %%ymm0, %%xmm0\n\tvmovdqu %%xmm0, (%0)\n\tvzeroupper"
          :
          : "r"(&__R), "r"(&__A)
          : "xmm0", "memory");
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_cvttps_epi32(__m256 __A)
{
  __m256i __R;
  __RUCC_AVX_OP1("vcvttps2dq", __R, __A);
  return __R;
}

static __inline__ double __attribute__((__always_inline__, __target__("avx")))
_mm256_cvtsd_f64(__m256d __A)
{
  return ((__v4df)__A)[0];
}

static __inline__ float __attribute__((__always_inline__, __target__("avx")))
_mm256_cvtss_f32(__m256 __A)
{
  return ((__v8sf)__A)[0];
}

/* # Moving lanes
 *
 * Only bit 0 of N counts in the moves of a 16 byte half, and only the low bits that index a lane
 * count in the moves of one lane, as in gcc. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("avx")))
_mm256_extractf128_pd(__m256d __X, const int __N)
{
  __v4di __x = (__v4di)__X;
  int __i = (__N & 1) * 2;
  return (__m128d)(__v2di){ __x[__i], __x[__i + 1] };
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx")))
_mm256_extractf128_ps(__m256 __X, const int __N)
{
  __v4di __x = (__v4di)__X;
  int __i = (__N & 1) * 2;
  return (__m128)(__v2di){ __x[__i], __x[__i + 1] };
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx")))
_mm256_extractf128_si256(__m256i __X, const int __N)
{
  __v4di __x = (__v4di)__X;
  int __i = (__N & 1) * 2;
  return (__m128i)(__v2di){ __x[__i], __x[__i + 1] };
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_extract_epi32(__m256i __X, int const __N)
{
  return ((__v8si)__X)[__N & 7];
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_extract_epi16(__m256i __X, int const __N)
{
  return ((__v16hu)__X)[__N & 15];
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_extract_epi8(__m256i __X, int const __N)
{
  return ((__v32qu)__X)[__N & 31];
}

#ifdef __x86_64__
static __inline__ long long __attribute__((__always_inline__, __target__("avx")))
_mm256_extract_epi64(__m256i __X, const int __N)
{
  return ((__v4di)__X)[__N & 3];
}

#endif

/* Both clear the upper half of every vector register, and `vzeroall` clears the lower half as
 * well, so it names all of them. */

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_zeroall(void)
{
  __asm__ __volatile__("vzeroall"
                       :
                       :
                       : "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5", "xmm6", "xmm7", "xmm8",
                         "xmm9", "xmm10", "xmm11", "xmm12", "xmm13", "xmm14", "xmm15");
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_zeroupper(void)
{
  __asm__ __volatile__("vzeroupper");
}

/* Each lane picked from within its own 16 byte half, by a control vector or by the bits of C. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("avx")))
_mm_permutevar_pd(__m128d __A, __m128i __C)
{
  __m128d __R;
  __RUCC_AVX_OP2_128("vpermilpd", __R, __A, __C);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_permutevar_pd(__m256d __A, __m256i __C)
{
  __m256d __R;
  __RUCC_AVX_OP2("vpermilpd", __R, __A, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx")))
_mm_permutevar_ps(__m128 __A, __m128i __C)
{
  __m128 __R;
  __RUCC_AVX_OP2_128("vpermilps", __R, __A, __C);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_permutevar_ps(__m256 __A, __m256i __C)
{
  __m256 __R;
  __RUCC_AVX_OP2("vpermilps", __R, __A, __C);
  return __R;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("avx")))
_mm_permute_pd(__m128d __X, const int __C)
{
  __m128d __R;
  __RUCC_AVX_OP1_IMM_128("vpermilpd", __R, __X, __C);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_permute_pd(__m256d __X, const int __C)
{
  __m256d __R;
  __RUCC_AVX_OP1_IMM("vpermilpd", __R, __X, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx")))
_mm_permute_ps(__m128 __X, const int __C)
{
  __m128 __R;
  __RUCC_AVX_OP1_IMM_128("vpermilps", __R, __X, __C);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_permute_ps(__m256 __X, const int __C)
{
  __m256 __R;
  __RUCC_AVX_OP1_IMM("vpermilps", __R, __X, __C);
  return __R;
}

/* Each 16 byte half of the answer is a half of X or Y, or zero, as C says. */

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_permute2f128_pd(__m256d __X, __m256d __Y, const int __C)
{
  __m256d __R;
  __RUCC_AVX_OP2_IMM("vperm2f128", __R, __X, __Y, __C);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_permute2f128_ps(__m256 __X, __m256 __Y, const int __C)
{
  __m256 __R;
  __RUCC_AVX_OP2_IMM("vperm2f128", __R, __X, __Y, __C);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_permute2f128_si256(__m256i __X, __m256i __Y, const int __C)
{
  __m256i __R;
  __RUCC_AVX_OP2_IMM("vperm2f128", __R, __X, __Y, __C);
  return __R;
}

/* # Broadcasts */

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx")))
_mm_broadcast_ss(float const *__X)
{
  float __a = *__X;
  return (__m128)(__v4sf){ __a, __a, __a, __a };
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_broadcast_sd(double const *__X)
{
  double __a = *__X;
  return (__m256d)(__v4df){ __a, __a, __a, __a };
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_broadcast_ss(float const *__X)
{
  float __a = *__X;
  return (__m256)(__v8sf){ __a, __a, __a, __a, __a, __a, __a, __a };
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_broadcast_pd(__m128d const *__X)
{
  __v2df __a = (__v2df)_mm_loadu_pd((double const *)__X);
  return (__m256d)(__v4df){ __a[0], __a[1], __a[0], __a[1] };
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_broadcast_ps(__m128 const *__X)
{
  __v2di __a = (__v2di)_mm_loadu_ps((float const *)__X);
  return (__m256)(__v4di){ __a[0], __a[1], __a[0], __a[1] };
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_insertf128_pd(__m256d __X, __m128d __Y, const int __O)
{
  __v4di __x = (__v4di)__X;
  __v2di __y = (__v2di)__Y;
  int __i = (__O & 1) * 2;
  __x[__i] = __y[0];
  __x[__i + 1] = __y[1];
  return (__m256d)__x;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_insertf128_ps(__m256 __X, __m128 __Y, const int __O)
{
  __v4di __x = (__v4di)__X;
  __v2di __y = (__v2di)__Y;
  int __i = (__O & 1) * 2;
  __x[__i] = __y[0];
  __x[__i + 1] = __y[1];
  return (__m256)__x;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_insertf128_si256(__m256i __X, __m128i __Y, const int __O)
{
  __v4di __x = (__v4di)__X;
  __v2di __y = (__v2di)__Y;
  int __i = (__O & 1) * 2;
  __x[__i] = __y[0];
  __x[__i + 1] = __y[1];
  return (__m256i)__x;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_insert_epi32(__m256i __X, int __D, int const __N)
{
  __v8si __x = (__v8si)__X;
  __x[__N & 7] = (int)__D;
  return (__m256i)__x;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_insert_epi16(__m256i __X, int __D, int const __N)
{
  __v16hi __x = (__v16hi)__X;
  __x[__N & 15] = (short)__D;
  return (__m256i)__x;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_insert_epi8(__m256i __X, int __D, int const __N)
{
  __v32qi __x = (__v32qi)__X;
  __x[__N & 31] = (char)__D;
  return (__m256i)__x;
}

#ifdef __x86_64__
static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_insert_epi64(__m256i __X, long long __D, int const __N)
{
  __v4di __x = (__v4di)__X;
  __x[__N & 3] = __D;
  return (__m256i)__x;
}

#endif

/* # Loads and stores
 *
 * The aligned ones read and write through the aligned type, and the others through the type with
 * an alignment of one. */

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_load_pd(double const *__P)
{
  return *(__m256d const *)__P;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_store_pd(double *__P, __m256d __A)
{
  *(__m256d *)__P = __A;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_loadu_pd(double const *__P)
{
  return *(__m256d_u const *)__P;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_storeu_pd(double *__P, __m256d __A)
{
  *(__m256d_u *)__P = __A;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_load_ps(float const *__P)
{
  return *(__m256 const *)__P;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_store_ps(float *__P, __m256 __A)
{
  *(__m256 *)__P = __A;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_loadu_ps(float const *__P)
{
  return *(__m256_u const *)__P;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_storeu_ps(float *__P, __m256 __A)
{
  *(__m256_u *)__P = __A;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_load_si256(__m256i const *__P)
{
  return *__P;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_store_si256(__m256i *__P, __m256i __A)
{
  *__P = __A;
}

/* On a Microsoft row this one is plain C and built for no extension, because the CRT's
 * <wchar.h> calls it from a function that is not built for AVX. See <intrin.h>. */
#ifdef _MSC_VER
static __inline__ __m256i __attribute__((__always_inline__))
#else
static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
#endif
_mm256_loadu_si256(__m256i_u const *__P)
{
  return *__P;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_storeu_si256(__m256i_u *__P, __m256i __A)
{
  *__P = __A;
}

/* The masked moves touch only the lanes whose mask has the top bit set, so a lane outside the
 * mask may be memory that cannot be read. That is why they are the instruction and not C. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("avx")))
_mm_maskload_pd(double const *__P, __m128i __M)
{
  __m128d __R;
  __asm__("vmovdqu (%2), %%xmm1\n\tvmaskmovpd (%1), %%xmm1, %%xmm0\n\t"
          "vmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(__P), "r"(&__M)
          : "xmm0", "xmm1", "memory");
  return __R;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm_maskstore_pd(double *__P, __m128i __M, __m128d __A)
{
  __asm__("vmovdqu (%1), %%xmm1\n\tvmovdqu (%2), %%xmm0\n\t"
          "vmaskmovpd %%xmm0, %%xmm1, (%0)"
          :
          : "r"(__P), "r"(&__M), "r"(&__A)
          : "xmm0", "xmm1", "memory");
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_maskload_pd(double const *__P, __m256i __M)
{
  __m256d __R;
  __asm__("vmovdqu (%2), %%ymm1\n\tvmaskmovpd (%1), %%ymm1, %%ymm0\n\t"
          "vmovdqu %%ymm0, (%0)\n\tvzeroupper"
          :
          : "r"(&__R), "r"(__P), "r"(&__M)
          : "xmm0", "xmm1", "memory");
  return __R;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_maskstore_pd(double *__P, __m256i __M, __m256d __A)
{
  __asm__("vmovdqu (%1), %%ymm1\n\tvmovdqu (%2), %%ymm0\n\t"
          "vmaskmovpd %%ymm0, %%ymm1, (%0)\n\tvzeroupper"
          :
          : "r"(__P), "r"(&__M), "r"(&__A)
          : "xmm0", "xmm1", "memory");
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx")))
_mm_maskload_ps(float const *__P, __m128i __M)
{
  __m128 __R;
  __asm__("vmovdqu (%2), %%xmm1\n\tvmaskmovps (%1), %%xmm1, %%xmm0\n\t"
          "vmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(__P), "r"(&__M)
          : "xmm0", "xmm1", "memory");
  return __R;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm_maskstore_ps(float *__P, __m128i __M, __m128 __A)
{
  __asm__("vmovdqu (%1), %%xmm1\n\tvmovdqu (%2), %%xmm0\n\t"
          "vmaskmovps %%xmm0, %%xmm1, (%0)"
          :
          : "r"(__P), "r"(&__M), "r"(&__A)
          : "xmm0", "xmm1", "memory");
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_maskload_ps(float const *__P, __m256i __M)
{
  __m256 __R;
  __asm__("vmovdqu (%2), %%ymm1\n\tvmaskmovps (%1), %%ymm1, %%ymm0\n\t"
          "vmovdqu %%ymm0, (%0)\n\tvzeroupper"
          :
          : "r"(&__R), "r"(__P), "r"(&__M)
          : "xmm0", "xmm1", "memory");
  return __R;
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_maskstore_ps(float *__P, __m256i __M, __m256 __A)
{
  __asm__("vmovdqu (%1), %%ymm1\n\tvmovdqu (%2), %%ymm0\n\t"
          "vmaskmovps %%ymm0, %%ymm1, (%0)\n\tvzeroupper"
          :
          : "r"(__P), "r"(&__M), "r"(&__A)
          : "xmm0", "xmm1", "memory");
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_movehdup_ps(__m256 __A)
{
  __m256 __R;
  __RUCC_AVX_OP1("vmovshdup", __R, __A);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_moveldup_ps(__m256 __A)
{
  __m256 __R;
  __RUCC_AVX_OP1("vmovsldup", __R, __A);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_movedup_pd(__m256d __A)
{
  __m256d __R;
  __RUCC_AVX_OP1("vmovddup", __R, __A);
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_lddqu_si256(__m256i const *__P)
{
  return *(__m256i_u const *)__P;
}

/* The stores that go around the cache. The address must be aligned to 32 bytes. */

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_stream_si256(__m256i *__P, __m256i __A)
{
  __asm__(__RUCC_AVX_LOAD1 "vmovntdq %%ymm0, (%0)\n\tvzeroupper"
          :
          : "r"(__P), "r"(&__A)
          : "xmm0", "memory");
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_stream_pd(double *__P, __m256d __A)
{
  __asm__(__RUCC_AVX_LOAD1 "vmovntpd %%ymm0, (%0)\n\tvzeroupper"
          :
          : "r"(__P), "r"(&__A)
          : "xmm0", "memory");
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_stream_ps(float *__P, __m256 __A)
{
  __asm__(__RUCC_AVX_LOAD1 "vmovntps %%ymm0, (%0)\n\tvzeroupper"
          :
          : "r"(__P), "r"(&__A)
          : "xmm0", "memory");
}

/* # Square roots and estimates */

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_rcp_ps(__m256 __A)
{
  __m256 __R;
  __RUCC_AVX_OP1("vrcpps", __R, __A);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_rsqrt_ps(__m256 __A)
{
  __m256 __R;
  __RUCC_AVX_OP1("vrsqrtps", __R, __A);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_sqrt_pd(__m256d __A)
{
  __m256d __R;
  __RUCC_AVX_OP1("vsqrtpd", __R, __A);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_sqrt_ps(__m256 __A)
{
  __m256 __R;
  __RUCC_AVX_OP1("vsqrtps", __R, __A);
  return __R;
}

/* M is one of the rounding modes of <smmintrin.h>. */

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_round_pd(__m256d __X, const int __M)
{
  __m256d __R;
  __RUCC_AVX_OP1_IMM("vroundpd", __R, __X, __M);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_round_ps(__m256 __X, const int __M)
{
  __m256 __R;
  __RUCC_AVX_OP1_IMM("vroundps", __R, __X, __M);
  return __R;
}

#define _mm256_ceil_pd(V) _mm256_round_pd((V), _MM_FROUND_CEIL)
#define _mm256_floor_pd(V) _mm256_round_pd((V), _MM_FROUND_FLOOR)
#define _mm256_ceil_ps(V) _mm256_round_ps((V), _MM_FROUND_CEIL)
#define _mm256_floor_ps(V) _mm256_round_ps((V), _MM_FROUND_FLOOR)

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_unpackhi_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vunpckhpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_unpacklo_pd(__m256d __A, __m256d __B)
{
  __m256d __R;
  __RUCC_AVX_OP2("vunpcklpd", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_unpackhi_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vunpckhps", __R, __A, __B);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_unpacklo_ps(__m256 __A, __m256 __B)
{
  __m256 __R;
  __RUCC_AVX_OP2("vunpcklps", __R, __A, __B);
  return __R;
}

/* # Tests
 *
 * `testz` is whether `M & V` is zero in the bits tested, which is the zero flag. `testc` is whether
 * `~M & V` is, which is the carry flag. `testnzc` is whether neither is, which is both flags clear.
 * The `_pd` and `_ps` forms test only the sign bit of each lane. */

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm_testz_pd(__m128d __M, __m128d __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestpd", "sete", "x", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm_testc_pd(__m128d __M, __m128d __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestpd", "setb", "x", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm_testnzc_pd(__m128d __M, __m128d __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestpd", "seta", "x", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm_testz_ps(__m128 __M, __m128 __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestps", "sete", "x", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm_testc_ps(__m128 __M, __m128 __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestps", "setb", "x", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm_testnzc_ps(__m128 __M, __m128 __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestps", "seta", "x", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_testz_pd(__m256d __M, __m256d __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestpd", "sete", "y", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_testc_pd(__m256d __M, __m256d __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestpd", "setb", "y", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_testnzc_pd(__m256d __M, __m256d __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestpd", "seta", "y", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_testz_ps(__m256 __M, __m256 __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestps", "sete", "y", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_testc_ps(__m256 __M, __m256 __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestps", "setb", "y", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_testnzc_ps(__m256 __M, __m256 __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vtestps", "seta", "y", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_testz_si256(__m256i __M, __m256i __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vptest", "sete", "y", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_testc_si256(__m256i __M, __m256i __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vptest", "setb", "y", __R, __M, __V);
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_testnzc_si256(__m256i __M, __m256i __V)
{
  unsigned char __R;
  __RUCC_AVX_FLAG2("vptest", "seta", "y", __R, __M, __V);
  return __R;
}

/* The sign bit of each lane, lane 0 in bit 0. */

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_movemask_pd(__m256d __A)
{
  int __R;
  __asm__ __volatile__("vmovdqu (%1), %%ymm0\n\tvmovmskpd %%ymm0, %0\n\tvzeroupper"
                       : "=r"(__R)
                       : "r"(&__A)
                       : "xmm0", "memory");
  return __R;
}

static __inline__ int __attribute__((__always_inline__, __target__("avx")))
_mm256_movemask_ps(__m256 __A)
{
  int __R;
  __asm__ __volatile__("vmovdqu (%1), %%ymm0\n\tvmovmskps %%ymm0, %0\n\tvzeroupper"
                       : "=r"(__R)
                       : "r"(&__A)
                       : "xmm0", "memory");
  return __R;
}

/* # Building vectors
 *
 * The `set` forms take the highest lane first and the `setr` forms the lowest lane first. */

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_undefined_pd(void)
{
  __m256d __R;
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_undefined_ps(void)
{
  __m256 __R;
  return __R;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_undefined_si256(void)
{
  __m256i __R;
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_setzero_pd(void)
{
  return (__m256d)(__v4df){ 0.0, 0.0, 0.0, 0.0 };
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_setzero_ps(void)
{
  return (__m256)(__v8sf){ 0.0f, 0.0f, 0.0f, 0.0f, 0.0f, 0.0f, 0.0f, 0.0f };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_setzero_si256(void)
{
  return (__m256i)(__v4di){ 0, 0, 0, 0 };
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_set_pd(double __q3, double __q2, double __q1, double __q0)
{
  return (__m256d)(__v4df){ __q0, __q1, __q2, __q3 };
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_set_ps(float __q7, float __q6, float __q5, float __q4, float __q3, float __q2, float __q1,
              float __q0)
{
  return (__m256)(__v8sf){ __q0, __q1, __q2, __q3, __q4, __q5, __q6, __q7 };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_set_epi32(int __q7, int __q6, int __q5, int __q4, int __q3, int __q2, int __q1, int __q0)
{
  return (__m256i)(__v8si){ __q0, __q1, __q2, __q3, __q4, __q5, __q6, __q7 };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_set_epi16(short __q15, short __q14, short __q13, short __q12, short __q11, short __q10,
                 short __q9, short __q8, short __q7, short __q6, short __q5, short __q4,
                 short __q3, short __q2, short __q1, short __q0)
{
  return (__m256i)(__v16hi){ __q0, __q1, __q2, __q3, __q4, __q5, __q6, __q7, __q8, __q9, __q10,
                             __q11, __q12, __q13, __q14, __q15 };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_set_epi8(char __q31, char __q30, char __q29, char __q28, char __q27, char __q26,
                char __q25, char __q24, char __q23, char __q22, char __q21, char __q20,
                char __q19, char __q18, char __q17, char __q16, char __q15, char __q14,
                char __q13, char __q12, char __q11, char __q10, char __q9, char __q8, char __q7,
                char __q6, char __q5, char __q4, char __q3, char __q2, char __q1, char __q0)
{
  return (__m256i)(__v32qi){ __q0, __q1, __q2, __q3, __q4, __q5, __q6, __q7, __q8, __q9, __q10,
                             __q11, __q12, __q13, __q14, __q15, __q16, __q17, __q18, __q19, __q20,
                             __q21, __q22, __q23, __q24, __q25, __q26, __q27, __q28, __q29, __q30,
                             __q31 };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_set_epi64x(long long __q3, long long __q2, long long __q1, long long __q0)
{
  return (__m256i)(__v4di){ __q0, __q1, __q2, __q3 };
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_set1_pd(double __A)
{
  return (__m256d)(__v4df){ __A, __A, __A, __A };
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_set1_ps(float __A)
{
  return (__m256)(__v8sf){ __A, __A, __A, __A, __A, __A, __A, __A };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_set1_epi32(int __A)
{
  return (__m256i)(__v8si){ __A, __A, __A, __A, __A, __A, __A, __A };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_set1_epi16(short __A)
{
  return (__m256i)(__v16hi){ __A, __A, __A, __A, __A, __A, __A, __A,
                             __A, __A, __A, __A, __A, __A, __A, __A };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_set1_epi8(char __A)
{
  return (__m256i)(__v32qi){ __A, __A, __A, __A, __A, __A, __A, __A,
                             __A, __A, __A, __A, __A, __A, __A, __A,
                             __A, __A, __A, __A, __A, __A, __A, __A,
                             __A, __A, __A, __A, __A, __A, __A, __A };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_set1_epi64x(long long __A)
{
  return (__m256i)(__v4di){ __A, __A, __A, __A };
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_setr_pd(double __q0, double __q1, double __q2, double __q3)
{
  return (__m256d)(__v4df){ __q0, __q1, __q2, __q3 };
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_setr_ps(float __q0, float __q1, float __q2, float __q3, float __q4, float __q5,
               float __q6, float __q7)
{
  return (__m256)(__v8sf){ __q0, __q1, __q2, __q3, __q4, __q5, __q6, __q7 };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_setr_epi32(int __q0, int __q1, int __q2, int __q3, int __q4, int __q5, int __q6, int __q7)
{
  return (__m256i)(__v8si){ __q0, __q1, __q2, __q3, __q4, __q5, __q6, __q7 };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_setr_epi16(short __q0, short __q1, short __q2, short __q3, short __q4, short __q5,
                  short __q6, short __q7, short __q8, short __q9, short __q10, short __q11,
                  short __q12, short __q13, short __q14, short __q15)
{
  return (__m256i)(__v16hi){ __q0, __q1, __q2, __q3, __q4, __q5, __q6, __q7, __q8, __q9, __q10,
                             __q11, __q12, __q13, __q14, __q15 };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_setr_epi8(char __q0, char __q1, char __q2, char __q3, char __q4, char __q5, char __q6,
                 char __q7, char __q8, char __q9, char __q10, char __q11, char __q12, char __q13,
                 char __q14, char __q15, char __q16, char __q17, char __q18, char __q19,
                 char __q20, char __q21, char __q22, char __q23, char __q24, char __q25,
                 char __q26, char __q27, char __q28, char __q29, char __q30, char __q31)
{
  return (__m256i)(__v32qi){ __q0, __q1, __q2, __q3, __q4, __q5, __q6, __q7, __q8, __q9, __q10,
                             __q11, __q12, __q13, __q14, __q15, __q16, __q17, __q18, __q19, __q20,
                             __q21, __q22, __q23, __q24, __q25, __q26, __q27, __q28, __q29, __q30,
                             __q31 };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_setr_epi64x(long long __q0, long long __q1, long long __q2, long long __q3)
{
  return (__m256i)(__v4di){ __q0, __q1, __q2, __q3 };
}

/* # Casts
 *
 * A cast between two 32 byte types is the same bytes. A cast to a 16 byte type is the lower half. A
 * cast from one leaves the upper half undefined in gcc, and it is zero here, which is what the
 * `zext` forms promise. */

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_castpd_ps(__m256d __A)
{
  return (__m256)__A;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_castpd_si256(__m256d __A)
{
  return (__m256i)__A;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_castps_pd(__m256 __A)
{
  return (__m256d)__A;
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_castps_si256(__m256 __A)
{
  return (__m256i)__A;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_castsi256_ps(__m256i __A)
{
  return (__m256)__A;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_castsi256_pd(__m256i __A)
{
  return (__m256d)__A;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("avx")))
_mm256_castpd256_pd128(__m256d __A)
{
  __v4di __a = (__v4di)__A;
  return (__m128d)(__v2di){ __a[0], __a[1] };
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("avx")))
_mm256_castps256_ps128(__m256 __A)
{
  __v4di __a = (__v4di)__A;
  return (__m128)(__v2di){ __a[0], __a[1] };
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx")))
_mm256_castsi256_si128(__m256i __A)
{
  __v4di __a = (__v4di)__A;
  return (__m128i)(__v2di){ __a[0], __a[1] };
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_castpd128_pd256(__m128d __A)
{
  __v2di __a = (__v2di)__A;
  return (__m256d)(__v4di){ __a[0], __a[1], 0, 0 };
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_castps128_ps256(__m128 __A)
{
  __v2di __a = (__v2di)__A;
  return (__m256)(__v4di){ __a[0], __a[1], 0, 0 };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_castsi128_si256(__m128i __A)
{
  __v2di __a = (__v2di)__A;
  return (__m256i)(__v4di){ __a[0], __a[1], 0, 0 };
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_zextpd128_pd256(__m128d __A)
{
  __v2di __a = (__v2di)__A;
  return (__m256d)(__v4di){ __a[0], __a[1], 0, 0 };
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_zextps128_ps256(__m128 __A)
{
  __v2di __a = (__v2di)__A;
  return (__m256)(__v4di){ __a[0], __a[1], 0, 0 };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_zextsi128_si256(__m128i __A)
{
  __v2di __a = (__v2di)__A;
  return (__m256i)(__v4di){ __a[0], __a[1], 0, 0 };
}

/* Two 16 byte halves into one 32 byte vector. H is the upper half and L the lower. */

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_set_m128(__m128 __H, __m128 __L)
{
  __v2di __h = (__v2di)__H, __l = (__v2di)__L;
  return (__m256)(__v4di){ __l[0], __l[1], __h[0], __h[1] };
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_setr_m128(__m128 __L, __m128 __H)
{
  return _mm256_set_m128(__H, __L);
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_set_m128d(__m128d __H, __m128d __L)
{
  __v2di __h = (__v2di)__H, __l = (__v2di)__L;
  return (__m256d)(__v4di){ __l[0], __l[1], __h[0], __h[1] };
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_setr_m128d(__m128d __L, __m128d __H)
{
  return _mm256_set_m128d(__H, __L);
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_set_m128i(__m128i __H, __m128i __L)
{
  __v2di __h = (__v2di)__H, __l = (__v2di)__L;
  return (__m256i)(__v4di){ __l[0], __l[1], __h[0], __h[1] };
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_setr_m128i(__m128i __L, __m128i __H)
{
  return _mm256_set_m128i(__H, __L);
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("avx")))
_mm256_loadu2_m128(float const *__PH, float const *__PL)
{
  return _mm256_set_m128(_mm_loadu_ps(__PH), _mm_loadu_ps(__PL));
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_storeu2_m128(float *__PH, float *__PL, __m256 __A)
{
  _mm_storeu_ps(__PL, _mm256_castps256_ps128(__A));
  _mm_storeu_ps(__PH, _mm256_extractf128_ps(__A, 1));
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("avx")))
_mm256_loadu2_m128d(double const *__PH, double const *__PL)
{
  return _mm256_set_m128d(_mm_loadu_pd(__PH), _mm_loadu_pd(__PL));
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_storeu2_m128d(double *__PH, double *__PL, __m256d __A)
{
  _mm_storeu_pd(__PL, _mm256_castpd256_pd128(__A));
  _mm_storeu_pd(__PH, _mm256_extractf128_pd(__A, 1));
}

static __inline__ __m256i __attribute__((__always_inline__, __target__("avx")))
_mm256_loadu2_m128i(__m128i_u const *__PH, __m128i_u const *__PL)
{
  return _mm256_set_m128i(_mm_loadu_si128(__PH), _mm_loadu_si128(__PL));
}

static __inline__ void __attribute__((__always_inline__, __target__("avx")))
_mm256_storeu2_m128i(__m128i_u *__PH, __m128i_u *__PL, __m256i __A)
{
  _mm_storeu_si128(__PL, _mm256_castsi256_si128(__A));
  _mm_storeu_si128(__PH, _mm256_extractf128_si256(__A, 1));
}

#endif /* __RUCC_AVXINTRIN_H */
