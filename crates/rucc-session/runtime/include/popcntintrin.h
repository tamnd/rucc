/* popcntintrin.h, the population count intrinsics.
 *
 * Two names, `_mm_popcnt_u32` and `_mm_popcnt_u64`, each the number of set bits in its argument.
 * gcc's copy writes them over `__builtin_popcount` and `__builtin_popcountll` under
 * `#pragma GCC target("popcnt")`, which leaves it to the compiler to turn the builtin into the
 * instruction because the pragma said the instruction is allowed. This compiler has no notion of
 * which instructions a function is allowed, so its `__builtin_popcount` is always the portable
 * sequence of shifts and masks. Writing the intrinsic over the builtin would give the right
 * answer and never the instruction, which is the one thing a program that names the intrinsic
 * rather than the builtin is asking for, so these are one `popcnt` each in inline assembly.
 *
 * # Why each is built for `popcnt`
 *
 * gcc defines both whatever the command line said and then refuses a call from a function that
 * was not built for the instruction, with "target specific option mismatch". A configure script
 * that tries a call without `-mpopcnt` learns that it needs the flag from that refusal, and a
 * program that picks its fastest path at run time calls them from a function carrying
 * `__attribute__((target("popcnt")))` and builds the rest of the unit for the baseline. Both
 * follow from the attribute on each function below, which this compiler reads the way gcc does.
 * See `tamnd/rucc#2002`.
 */

#ifndef __RUCC_POPCNTINTRIN_H
#define __RUCC_POPCNTINTRIN_H

static __inline__ int __attribute__((__always_inline__, __target__("popcnt")))
_mm_popcnt_u32(unsigned int __X)
{
  unsigned int __R;
  __asm__("popcntl %1, %0" : "=r"(__R) : "rm"(__X) : "cc");
  return (int)__R;
}

static __inline__ long long __attribute__((__always_inline__, __target__("popcnt")))
_mm_popcnt_u64(unsigned long long __X)
{
  unsigned long long __R;
  __asm__("popcntq %1, %0" : "=r"(__R) : "rm"(__X) : "cc");
  return (long long)__R;
}

#endif /* __RUCC_POPCNTINTRIN_H */
