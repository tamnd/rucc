/* intrin.h, Microsoft's umbrella over its intrinsics.
 *
 * The universal CRT's <wchar.h> includes this on every x64 build, so a Windows program that asks
 * for wide strings asks for it too. The copy in the MSVC toolset is written for cl.exe: it
 * declares every intrinsic cl knows as an ordinary prototype, and then includes the vendor
 * headers, which here are this compiler's own <immintrin.h>. Those define several of the same
 * names as macros, `_mm_prefetch` and `_mm_extract_epi16` among them, so a prototype of one turns
 * into a syntax error, and AMD's <ammintrin.h> after them wants AVX types this compiler does not
 * have. clang ships an intrin.h of its own for the same reason, and this is the same answer.
 *
 * On a Microsoft row this is the vector intrinsics this compiler has, the bit scans, and the
 * handful of AVX2 names <wchar.h> needs, which are described where they are defined. The other
 * Microsoft names, `__cpuid` and the `_Interlocked` family among them, are not here, and a program
 * that calls one gets a diagnostic at the call.
 *
 * Anywhere else the name belongs to the library. mingw-w64 ships an intrin.h written for gcc, so
 * the search goes on to find it.
 */

#if defined(_MSC_VER)

#ifndef __RUCC_INTRIN_H
#define __RUCC_INTRIN_H

#include <x86intrin.h>

/* Declared the way Microsoft declares them, so that <winnt.h> declaring them again agrees, and
 * defined in this compiler's runtime archive rather than here. See runtime/builtins/bitscan.c. */
unsigned char _BitScanForward(unsigned long *_Index, unsigned long _Mask);
unsigned char _BitScanForward64(unsigned long *_Index, unsigned long long _Mask);
unsigned char _BitScanReverse(unsigned long *_Index, unsigned long _Mask);
unsigned char _BitScanReverse64(unsigned long *_Index, unsigned long long _Mask);

/* The inline `wmemchr` and `wmemcmp` in <wchar.h> have a 32 byte loop that runs when the CRT has
 * found AVX2 on the machine, and the header compiles it for any compiler that is not clang. So the
 * type and the four operations in that loop have to exist for <wchar.h> to compile at all. They
 * are plain C over the vector's lanes rather than the instructions, since this compiler has no AVX2
 * yet, which gives the same answers more slowly, and nothing else is promised by them. */
typedef long long __m256i __attribute__((__vector_size__(32), __may_alias__));
typedef short __rucc_v16hi __attribute__((__vector_size__(32)));
typedef unsigned char __rucc_v32qu __attribute__((__vector_size__(32)));

static __inline__ __m256i __attribute__((__always_inline__))
_mm256_loadu_si256(__m256i const *__p)
{
  __rucc_v32qu __to;
  const unsigned char *__from = (const unsigned char *)__p;
  int __i;
  for (__i = 0; __i < 32; __i++)
    __to[__i] = __from[__i];
  return (__m256i)__to;
}

static __inline__ __m256i __attribute__((__always_inline__))
_mm256_broadcastw_epi16(__m128i __a)
{
  short __low = ((__v8hi)__a)[0];
  __rucc_v16hi __to;
  int __i;
  for (__i = 0; __i < 16; __i++)
    __to[__i] = __low;
  return (__m256i)__to;
}

static __inline__ __m256i __attribute__((__always_inline__))
_mm256_cmpeq_epi16(__m256i __a, __m256i __b)
{
  return (__m256i)((__rucc_v16hi)__a == (__rucc_v16hi)__b);
}

static __inline__ int __attribute__((__always_inline__)) _mm256_movemask_epi8(__m256i __a)
{
  __rucc_v32qu __from = (__rucc_v32qu)__a;
  unsigned int __answer = 0;
  int __i;
  for (__i = 0; __i < 32; __i++)
    __answer |= (unsigned int)(__from[__i] >> 7) << __i;
  return (int)__answer;
}

#endif /* __RUCC_INTRIN_H */

#else

#include_next <intrin.h>

#endif
