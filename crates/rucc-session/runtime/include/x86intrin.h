/* x86intrin.h, the widest of the x86 umbrellas.
 *
 * There are two umbrellas rather than one and the difference between them is the vendor.
 * `immintrin.h` is Intel's, and covers the families Intel published: MMX, the SSE line, AVX and
 * AVX512. This one is everything, so gcc writes it as `immintrin.h` plus AMD's own families,
 * which are 3DNow!, FMA4 and XOP, plus the general purpose header that holds the names that are
 * not vector instructions at all.
 *
 * None of those extra families is a family this compiler has, and none of them is a family any
 * current machine has either: AMD dropped 3DNow! with Bulldozer's successor, and FMA4 and XOP
 * went with Zen. So on every target this compiler supports, the widest umbrella and Intel's reach
 * the same set of names, and this header is `immintrin.h` with a reason attached.
 *
 * It is here because it is what a header asks for rather than what a program asks for. mingw-w64's
 * `<winnt.h>` includes it on line 1658, so every Windows program that includes `<windows.h>` needs
 * a file of this name to exist, whether or not the program has ever heard of an intrinsic. What
 * that header then uses out of it is the fence and cache line family, `_mm_lfence`, `_mm_sfence`,
 * `_mm_mfence`, `_mm_pause` and `_mm_clflush`, all of which are SSE and SSE2 and all of which are
 * underneath here already.
 *
 * A name this compiler does not have stays missing rather than being made to appear, which is the
 * rule `immintrin.h` states at more length. A program that includes this and then calls an XOP
 * intrinsic gets a diagnostic at the call, which is where it belongs.
 */

#ifndef __RUCC_X86INTRIN_H
#define __RUCC_X86INTRIN_H

#include <immintrin.h>

#endif /* __RUCC_X86INTRIN_H */
