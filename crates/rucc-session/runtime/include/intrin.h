/* intrin.h, Microsoft's umbrella over its intrinsics.
 *
 * The universal CRT's <wchar.h> includes this on every x64 build, so a Windows program that asks
 * for wide strings asks for it too. The copy in the MSVC toolset is written for cl.exe: it
 * declares every intrinsic cl knows as an ordinary prototype, and then includes the vendor
 * headers, which here are this compiler's own <immintrin.h>. Those define several of the same
 * names as macros, `_mm_prefetch` and `_mm_extract_epi16` among them, so a prototype of one turns
 * into a syntax error, and AMD's <ammintrin.h> after them wants names this compiler does not
 * have. clang ships an intrin.h of its own for the same reason, and this is the same answer.
 *
 * On a Microsoft row this is the vector intrinsics this compiler has and the bit scans. The four
 * 32 byte names <wchar.h> needs are described below. The other Microsoft names, `__cpuid` and the
 * `_Interlocked` family among them, are not here, and a program that calls one gets a diagnostic
 * at the call.
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
 * four operations in that loop have to exist, in a function that is not built for AVX2, for
 * <wchar.h> to compile at all. <avxintrin.h> and <avx2intrin.h> define those four with no target on
 * this row, in plain C over the lanes of the vector. */

#endif /* __RUCC_INTRIN_H */

#else

#include_next <intrin.h>

#endif
