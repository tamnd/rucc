/* immintrin.h, the umbrella over the x86 intrinsics.
 *
 * This header computes nothing. It is the one a program includes when it does not want to know
 * which family a name it uses came from, and all it does is reach the headers that have the
 * names. gcc's version of it covers everything from AVX512 down through AVX2, AVX and the SSE
 * family, each piece behind the macro the target defines, so a build for a plain x86-64 target
 * gets MMX, SSE and SSE2 out of it and nothing else. That is exactly the set this compiler ships,
 * so on this target the two headers reach the same names.
 *
 * What this does not do is make a name appear that is not here. A program that includes this and
 * then calls an AVX2 intrinsic gets a diagnostic at the call, which is a better place for it than
 * the include, and the family it asked for is work that arrives when the corpus asks for it. See
 * `tamnd/rucc#1236` for why brotli only needed the umbrella, and `tamnd/rucc#1114` for what the
 * headers underneath it are made of.
 *
 * # Why the guards are on the includes
 *
 * gcc writes its includes unconditionally here and puts `#pragma GCC target` inside each header,
 * so the names exist whatever the command line said and the compiler works out afterwards whether
 * the instruction is allowed. This compiler has no such pragma, and every name under here is
 * ordinary C over vector typed values rather than a request for an instruction, so the honest
 * spelling is the macro test: a target that does not define `__SSE2__` is a target where a
 * program has no business calling `_mm_add_epi32`, and the diagnostic says the name is unknown
 * rather than pretending it is there.
 *
 * On x86-64 all three macros are always defined, because SSE2 is in the baseline the ABI names,
 * so in practice this header is the SSE2 header plus a comment.
 *
 * `<smmintrin.h>` is reached the way gcc reaches it, whatever the command line said, and brings
 * SSE3, SSSE3, SSE4.1 and SSE4.2 with it through `<tmmintrin.h>` and `<pmmintrin.h>`, and the
 * population counts through `<popcntintrin.h>`. Every name in them is an instruction outside the
 * baseline, and each is built for its extension, so a function that is not built for it cannot
 * call it. See `tamnd/rucc#2003` and `tamnd/rucc#2045`.
 *
 * `<xsaveintrin.h>` is reached the same way and for the same reason, and holds `_xgetbv`.
 * `<cetintrin.h>` is too, and holds the shadow stack instructions that pcre2's JIT calls.
 *
 * So are the AVX-512 headers, which hold what PostgreSQL's AVX-512 CRC32C and population count
 * call and nothing more yet. See `tamnd/rucc#1988`.
 */

#ifndef __RUCC_IMMINTRIN_H
#define __RUCC_IMMINTRIN_H

#ifdef __MMX__
#include <mmintrin.h>
#endif

#ifdef __SSE__
#include <xmmintrin.h>
#endif

#ifdef __SSE2__
#include <emmintrin.h>
#endif

/* Unguarded, because every name inside it is built for its own extension, and a function built
 * without that extension is refused the call the way gcc refuses it. */
#include <smmintrin.h>

#include <xsaveintrin.h>
#include <cetintrin.h>

/* Unguarded for the same reason. Each function is built for its own extension, and a macro among
 * them writes instructions only where it is used, which is inside a function built for it. */
#include <avx512fintrin.h>
#include <avx512bwintrin.h>
#include <avx512vlintrin.h>
#include <avx512vpopcntdqintrin.h>
#include <vpclmulqdqintrin.h>

#endif /* __RUCC_IMMINTRIN_H */
