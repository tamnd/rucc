/* avx512fintrin.h, the AVX-512 foundation intrinsics.
 *
 * Reached through <immintrin.h> the way gcc reaches it, and complete on its own as well.
 * What is here is what PostgreSQL's AVX-512 CRC32C and population count code call: the 64 byte
 * vector type, loads, the bitwise and add operations, the lane moves and the reduction. See
 * `tamnd/rucc#1988`.
 *
 * # How each one is written
 *
 * gcc writes these over builtins its back end turns into one instruction each. This compiler has
 * no such builtins yet (`tamnd/rucc#200`), so each operation is inline assembly running the
 * instruction gcc runs. A 64 byte vector lives in memory in this compiler rather than in a
 * register, so the assembly is handed the addresses of its operands and of its result, loads what
 * it reads into `zmm16` and up, and stores what it writes. The register allocator never hands out
 * `zmm16` to `zmm31`, so nothing the compiler keeps in a register is disturbed, and nothing needs
 * `vzeroupper` afterwards, since only the upper sixteen registers are touched. That costs a load
 * and a store around each instruction, which is slower than gcc but runs the same instructions on
 * the same data.
 *
 * The ones with an immediate are macros, because the immediate has to be a constant where the
 * instruction is written. gcc does the same when it does not optimize.
 *
 * Each function is built for `avx512f`, so a call from a function that is not is refused the way
 * gcc refuses it. The moves between a 16 byte and a 64 byte vector are plain C, since they are
 * moves and nothing more.
 */

#ifndef __RUCC_AVX512FINTRIN_H
#define __RUCC_AVX512FINTRIN_H

#include <emmintrin.h>

typedef double __v8df __attribute__((__vector_size__(64)));
typedef float __v16sf __attribute__((__vector_size__(64)));
typedef long long __v8di __attribute__((__vector_size__(64)));
typedef unsigned long long __v8du __attribute__((__vector_size__(64)));
typedef int __v16si __attribute__((__vector_size__(64)));
typedef unsigned int __v16su __attribute__((__vector_size__(64)));
typedef short __v32hi __attribute__((__vector_size__(64)));
typedef char __v64qi __attribute__((__vector_size__(64)));

typedef float __m512 __attribute__((__vector_size__(64), __may_alias__));
typedef long long __m512i __attribute__((__vector_size__(64), __may_alias__));
typedef double __m512d __attribute__((__vector_size__(64), __may_alias__));
typedef long long __m512i_u __attribute__((__vector_size__(64), __may_alias__, __aligned__(1)));

typedef unsigned char __mmask8;
typedef unsigned short __mmask16;

static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512f")))
_mm512_setzero_si512(void)
{
  __m512i __R;
  __asm__("vpxorq %%zmm16, %%zmm16, %%zmm16\n\t"
          "vmovdqu64 %%zmm16, (%0)"
          :
          : "r"(&__R)
          : "memory");
  return __R;
}

static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512f")))
_mm512_setr_epi32(int __A, int __B, int __C, int __D, int __E, int __F, int __G, int __H, int __I,
                  int __J, int __K, int __L, int __M, int __N, int __O, int __P)
{
  return (__m512i)(__v16si){__A, __B, __C, __D, __E, __F, __G, __H,
                            __I, __J, __K, __L, __M, __N, __O, __P};
}

static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512f")))
_mm512_set1_epi8(char __A)
{
  return (__m512i)(__v64qi){__A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A,
                            __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A,
                            __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A,
                            __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A,
                            __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A, __A};
}

/* Both loads are `vmovdqu64`, which is as fast as the aligned form on aligned data and does not
 * fault on data that is not. */
static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512f")))
_mm512_loadu_si512(void const *__P)
{
  __m512i __R;
  __asm__("vmovdqu64 (%1), %%zmm16\n\t"
          "vmovdqu64 %%zmm16, (%0)"
          :
          : "r"(&__R), "r"(__P)
          : "memory");
  return __R;
}

static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512f")))
_mm512_load_si512(void const *__P)
{
  return _mm512_loadu_si512(__P);
}

/* One instruction over two vectors, which is what the three below are. */
#define __RUCC_AVX512_BINARY(OP, R, A, B)                                                      \
  __asm__("vmovdqu64 (%1), %%zmm16\n\t" OP " (%2), %%zmm16, %%zmm16\n\t"                   \
          "vmovdqu64 %%zmm16, (%0)"                                                            \
          :                                                                                    \
          : "r"(&(R)), "r"(&(A)), "r"(&(B))                                                    \
          : "memory")

static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512f")))
_mm512_add_epi64(__m512i __A, __m512i __B)
{
  __m512i __R;
  __RUCC_AVX512_BINARY("vpaddq", __R, __A, __B);
  return __R;
}

static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512f")))
_mm512_and_si512(__m512i __A, __m512i __B)
{
  __m512i __R;
  __RUCC_AVX512_BINARY("vpandq", __R, __A, __B);
  return __R;
}

static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512f")))
_mm512_xor_si512(__m512i __A, __m512i __B)
{
  __m512i __R;
  __RUCC_AVX512_BINARY("vpxorq", __R, __A, __B);
  return __R;
}

/* `vpternlogq`: each bit of the result is the bit of the immediate the three bits of A, B and C
 * pick, A being the highest. A is also the destination, which is why it is the one loaded. */
#define _mm512_ternarylogic_epi64(A, B, C, I)                                                  \
  __extension__({                                                                              \
    __m512i __rucc_a = (A), __rucc_b = (B), __rucc_c = (C), __rucc_r;                          \
    __asm__("vmovdqu64 (%1), %%zmm16\n\t"                                                      \
            "vmovdqu64 (%2), %%zmm17\n\t"                                                      \
            "vpternlogq %4, (%3), %%zmm17, %%zmm16\n\t"                                        \
            "vmovdqu64 %%zmm16, (%0)"                                                          \
            :                                                                                  \
            : "r"(&__rucc_r), "r"(&__rucc_a), "r"(&__rucc_b), "r"(&__rucc_c), "n"(I)           \
            : "memory");                                                                       \
    __rucc_r;                                                                                  \
  })

/* The lane of four ints the immediate names, stored straight from the register. */
#define _mm512_extracti32x4_epi32(A, I)                                                        \
  __extension__({                                                                              \
    __m512i __rucc_a = (A);                                                                    \
    __m128i __rucc_r;                                                                          \
    __asm__("vmovdqu64 (%1), %%zmm16\n\t"                                                      \
            "vextracti32x4 %2, %%zmm16, (%0)"                                                  \
            :                                                                                  \
            : "r"(&__rucc_r), "r"(&__rucc_a), "n"((I) & 3)                                     \
            : "memory");                                                                       \
    __rucc_r;                                                                                  \
  })

static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512f")))
_mm512_broadcast_i32x4(__m128i __A)
{
  __m512i __R;
  __asm__("vbroadcasti32x4 (%1), %%zmm16\n\t"
          "vmovdqu64 %%zmm16, (%0)"
          :
          : "r"(&__R), "r"(&__A)
          : "memory");
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("avx512f")))
_mm512_castsi512_si128(__m512i __A)
{
  return (__m128i){__A[0], __A[1]};
}

static __inline__ __m512i __attribute__((__always_inline__, __target__("avx512f")))
_mm512_zextsi128_si512(__m128i __A)
{
  return (__m512i){__A[0], __A[1], 0, 0, 0, 0, 0, 0};
}

/* The sum of the eight lanes, halving the vector three times with the upper sixteen registers
 * only at their full width, so that this needs nothing beyond `avx512f`. */
static __inline__ long long __attribute__((__always_inline__, __target__("avx512f")))
_mm512_reduce_add_epi64(__m512i __A)
{
  long long __R;
  __asm__("vmovdqu64 (%1), %%zmm16\n\t"
          "vshufi64x2 $0x4e, %%zmm16, %%zmm16, %%zmm17\n\t"
          "vpaddq %%zmm17, %%zmm16, %%zmm16\n\t"
          "vshufi64x2 $0xb1, %%zmm16, %%zmm16, %%zmm17\n\t"
          "vpaddq %%zmm17, %%zmm16, %%zmm16\n\t"
          "vpshufd $0x4e, %%zmm16, %%zmm17\n\t"
          "vpaddq %%zmm17, %%zmm16, %%zmm16\n\t"
          "vmovq %%xmm16, %0"
          : "=r"(__R)
          : "r"(&__A)
          : "memory");
  return __R;
}

#endif /* __RUCC_AVX512FINTRIN_H */
