/* pmmintrin.h, the SSE3 intrinsics.
 *
 * Eleven names, every one gcc 16.2.0 has here but the two `<mwaitintrin.h>` brings in. Four are
 * the horizontal sums and differences of floats, `_mm_hadd_ps` and its siblings, which add or take
 * away neighbouring lanes rather than matching ones. Two are `_mm_addsub_ps` and `_mm_addsub_pd`,
 * which take away in the even lanes and add in the odd ones, the step a complex multiply needs.
 * Three copy one lane over its neighbour, and two are loads: `_mm_loaddup_pd`, one double read
 * into both lanes, and `_mm_lddqu_si128`, the unaligned load that is faster than `movdqu` on the
 * processors it was made for when the sixteen bytes cross a cache line.
 *
 * `_mm_monitor` and `_mm_mwait` are not here. gcc builds them for `mwait`, which this compiler
 * knows the name of and does not honour, so a program calling them gets a diagnostic naming the
 * function. The two denormals macros are not here either, since `_mm_getcsr` is not, which is the
 * reason `<xmmintrin.h>` gives.
 *
 * # Why inline assembly, and why through memory
 *
 * The header below this one is C over vector types and computes each lane on its own, which is the
 * right answer and never the instruction. For most of what this header and the two above it have
 * that trade is a poor one. A rounding under the current mode, a dot product with its partial sums
 * in a particular order, or a string comparison is a page of C to get right for every input and
 * is one instruction that is right by definition. So each function here is the instruction gcc's
 * builtin becomes, in inline assembly, the way `<smmintrin.h>` writes the CRC32C steps.
 *
 * What an operand of inline assembly may not be yet is a vector, since this compiler takes
 * vectors apart into their lanes before the back end sees them, and that is `tamnd/rucc#200`. So
 * each function hands the template the address of its arguments and of its answer, and the
 * template loads them into registers it names, runs the instruction and stores the result. The
 * loads and the store are `movdqu`, which asks nothing about alignment, since the argument of an
 * inlined function is in whatever slot the caller gave it. That is three more instructions than
 * gcc writes and the arithmetic is the machine's own, lane for lane.
 *
 * # Why each is built for `sse3`
 *
 * For the reason `<popcntintrin.h>` gives at more length: gcc defines every name whatever the
 * command line said and refuses a call from a function not built for the instruction, and a
 * program that picks its path at run time calls these from a function carrying
 * `__attribute__((target("sse3")))`. See `tamnd/rucc#2045`.
 */

#ifndef __RUCC_PMMINTRIN_H
#define __RUCC_PMMINTRIN_H

#include <emmintrin.h>

/* The two shapes almost every function in this header and the two above it has. `__RUCC_SSE_OP1`
 * loads one vector into `xmm0`, runs the instruction on it in place and stores it, and
 * `__RUCC_SSE_OP2` loads a second into `xmm1` first and runs the instruction with that as its
 * source, which is `__insn __y, __x` in the order the machine writes it. Each takes the text in
 * front of the registers, so a mnemonic with an immediate is written with the immediate in it. */
#define __RUCC_SSE_OP1(__insn, __answer, __x)                                                      \
  __asm__("movdqu (%1), %%xmm0\n\t" __insn " %%xmm0, %%xmm0\n\tmovdqu %%xmm0, (%0)"               \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x))                                                         \
          : "xmm0", "memory")

#define __RUCC_SSE_OP2(__insn, __answer, __x, __y)                                                 \
  __asm__("movdqu (%1), %%xmm0\n\tmovdqu (%2), %%xmm1\n\t" __insn                                  \
          " %%xmm1, %%xmm0\n\tmovdqu %%xmm0, (%0)"                                                 \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "r"(&(__y))                                            \
          : "xmm0", "xmm1", "memory")

/* The same with an immediate, which is `%3` in the second and `%2` in the first. The immediate is
 * a parameter of the function the shape is written in, and is a constant in every copy of it
 * inlined into a caller that passed one, which is the only kind of call gcc accepts either. */
#define __RUCC_SSE_OP1_IMM(__insn, __answer, __x, __imm)                                           \
  __asm__("movdqu (%1), %%xmm0\n\t" __insn " %2, %%xmm0, %%xmm0\n\tmovdqu %%xmm0, (%0)"           \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "i"(__imm)                                             \
          : "xmm0", "memory")

#define __RUCC_SSE_OP2_IMM(__insn, __answer, __x, __y, __imm)                                      \
  __asm__("movdqu (%1), %%xmm0\n\tmovdqu (%2), %%xmm1\n\t" __insn                                  \
          " %3, %%xmm1, %%xmm0\n\tmovdqu %%xmm0, (%0)"                                             \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__x)), "r"(&(__y)), "i"(__imm)                                \
          : "xmm0", "xmm1", "memory")

/* # Floats
 *
 * The horizontal ones put the two answers from the first operand in the low half and the two from
 * the second in the high half. */

static __inline__ __m128 __attribute__((__always_inline__, __target__("sse3")))
_mm_addsub_ps(__m128 __X, __m128 __Y)
{
  __m128 __R;
  __RUCC_SSE_OP2("addsubps", __R, __X, __Y);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("sse3")))
_mm_hadd_ps(__m128 __X, __m128 __Y)
{
  __m128 __R;
  __RUCC_SSE_OP2("haddps", __R, __X, __Y);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("sse3")))
_mm_hsub_ps(__m128 __X, __m128 __Y)
{
  __m128 __R;
  __RUCC_SSE_OP2("hsubps", __R, __X, __Y);
  return __R;
}

/* Each odd lane copied over the even one below it, and each even lane over the odd one above. */
static __inline__ __m128 __attribute__((__always_inline__, __target__("sse3")))
_mm_movehdup_ps(__m128 __X)
{
  __m128 __R;
  __RUCC_SSE_OP1("movshdup", __R, __X);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("sse3")))
_mm_moveldup_ps(__m128 __X)
{
  __m128 __R;
  __RUCC_SSE_OP1("movsldup", __R, __X);
  return __R;
}

/* # Doubles */

static __inline__ __m128d __attribute__((__always_inline__, __target__("sse3")))
_mm_addsub_pd(__m128d __X, __m128d __Y)
{
  __m128d __R;
  __RUCC_SSE_OP2("addsubpd", __R, __X, __Y);
  return __R;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("sse3")))
_mm_hadd_pd(__m128d __X, __m128d __Y)
{
  __m128d __R;
  __RUCC_SSE_OP2("haddpd", __R, __X, __Y);
  return __R;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("sse3")))
_mm_hsub_pd(__m128d __X, __m128d __Y)
{
  __m128d __R;
  __RUCC_SSE_OP2("hsubpd", __R, __X, __Y);
  return __R;
}

/* The low double in both lanes. */
static __inline__ __m128d __attribute__((__always_inline__, __target__("sse3")))
_mm_movedup_pd(__m128d __X)
{
  __m128d __R;
  __RUCC_SSE_OP1("movddup", __R, __X);
  return __R;
}

/* # Loads
 *
 * Each reads through the pointer it was given, which is the one operand here that is the
 * program's memory rather than a slot of this function's. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("sse3")))
_mm_loaddup_pd(double const *__P)
{
  __m128d __R;
  __asm__("movddup (%1), %%xmm0\n\tmovdqu %%xmm0, (%0)"
          :
          : "r"(&__R), "r"(__P)
          : "xmm0", "memory");
  return __R;
}

static __inline__ __m128i __attribute__((__always_inline__, __target__("sse3")))
_mm_lddqu_si128(__m128i const *__P)
{
  __m128i __R;
  __asm__("lddqu (%1), %%xmm0\n\tmovdqu %%xmm0, (%0)" : : "r"(&__R), "r"(__P) : "xmm0", "memory");
  return __R;
}

#endif /* __RUCC_PMMINTRIN_H */
