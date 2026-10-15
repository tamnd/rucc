/* lzcntintrin.h, the leading zero count intrinsics.
 *
 * The same five names gcc 16.2.0 has here. Each one is `lzcnt` in inline assembly and is built
 * for `lzcnt`, so a call from a function that is not built for it is refused, as gcc refuses it.
 * The count of zero is the width, which is what the instruction gives and what `bsr` does not.
 * See `tamnd/rucc#2322`.
 */

#ifndef __RUCC_LZCNTINTRIN_H
#define __RUCC_LZCNTINTRIN_H

/* The 32 bit count less 16. The instruction has a 16 bit form, and this gives the same answer,
 * which is 16 for zero. */
static __inline__ unsigned short __attribute__((__always_inline__, __target__("lzcnt")))
__lzcnt16(unsigned short __X)
{
  unsigned int __R;
  __asm__("lzcntl %1, %0" : "=r"(__R) : "r"((unsigned int)__X) : "cc");
  return (unsigned short)(__R - 16);
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("lzcnt")))
__lzcnt32(unsigned int __X)
{
  unsigned int __R;
  __asm__("lzcntl %1, %0" : "=r"(__R) : "rm"(__X) : "cc");
  return __R;
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("lzcnt")))
_lzcnt_u32(unsigned int __X)
{
  return __lzcnt32(__X);
}

#ifdef __x86_64__
static __inline__ unsigned long long __attribute__((__always_inline__, __target__("lzcnt")))
__lzcnt64(unsigned long long __X)
{
  unsigned long long __R;
  __asm__("lzcntq %1, %0" : "=r"(__R) : "rm"(__X) : "cc");
  return __R;
}

static __inline__ unsigned long long __attribute__((__always_inline__, __target__("lzcnt")))
_lzcnt_u64(unsigned long long __X)
{
  return __lzcnt64(__X);
}
#endif /* __x86_64__ */

#endif /* __RUCC_LZCNTINTRIN_H */
