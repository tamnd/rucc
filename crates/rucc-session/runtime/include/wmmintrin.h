/* wmmintrin.h, the AES and carryless multiplication intrinsics.
 *
 * Seven names, the same seven gcc 16.2.0 has here. Six are the AES round steps, which are built
 * for `aes`, and one is `_mm_clmulepi64_si128`, which is built for `pclmul`. libsodium, OpenSSL
 * and zlib-ng call them from functions that carry the attribute, and libsodium's configure
 * includes this header by name to find out if the processor has them.
 *
 * Each function is the instruction in inline assembly through memory, with the shapes that
 * <pmmintrin.h> defines and for the reason it gives. The immediate of `_mm_aeskeygenassist_si128`
 * and `_mm_clmulepi64_si128` is a parameter, and it is a constant in each inlined copy. See
 * `tamnd/rucc#2322`.
 */

#ifndef __RUCC_WMMINTRIN_H
#define __RUCC_WMMINTRIN_H

#include <pmmintrin.h>

/* One AES encryption round of the state X with the round key Y. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("aes")))
_mm_aesenc_si128(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("aesenc", __R, __X, __Y);
  return __R;
}

/* The last AES encryption round, which has no column mix. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("aes")))
_mm_aesenclast_si128(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("aesenclast", __R, __X, __Y);
  return __R;
}

/* One AES decryption round of the state X with the round key Y. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("aes")))
_mm_aesdec_si128(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("aesdec", __R, __X, __Y);
  return __R;
}

/* The last AES decryption round. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("aes")))
_mm_aesdeclast_si128(__m128i __X, __m128i __Y)
{
  __m128i __R;
  __RUCC_SSE_OP2("aesdeclast", __R, __X, __Y);
  return __R;
}

/* The inverse column mix of a round key, which makes an encryption key a decryption key. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("aes")))
_mm_aesimc_si128(__m128i __X)
{
  __m128i __R;
  __RUCC_SSE_OP1("aesimc", __R, __X);
  return __R;
}

/* One step of the AES key expansion, with the round constant C. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("aes")))
_mm_aeskeygenassist_si128(__m128i __X, const int __C)
{
  __m128i __R;
  __RUCC_SSE_OP1_IMM("aeskeygenassist", __R, __X, __C);
  return __R;
}

/* The carryless product of one 64 bit half of X and one of Y. Bit 0 of I selects the half of X
 * and bit 4 selects the half of Y. */
static __inline__ __m128i __attribute__((__always_inline__, __target__("pclmul")))
_mm_clmulepi64_si128(__m128i __X, __m128i __Y, const int __I)
{
  __m128i __R;
  __RUCC_SSE_OP2_IMM("pclmulqdq", __R, __X, __Y, __I);
  return __R;
}

#endif /* __RUCC_WMMINTRIN_H */
