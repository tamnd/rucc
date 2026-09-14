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

#endif /* __RUCC_IMMINTRIN_H */
