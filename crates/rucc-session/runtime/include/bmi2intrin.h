/* bmi2intrin.h, the BMI2 intrinsics.
 *
 * The same names gcc 16.2.0 has here. Each one is built for `bmi2`, so a call from a function
 * that is not built for it is refused, as gcc refuses it. `bzhi`, `pdep` and `pext` are the
 * instruction in inline assembly. The wide multiply is plain C over a type twice as wide, as gcc
 * writes it. See `tamnd/rucc#2322`.
 */

#ifndef __RUCC_BMI2INTRIN_H
#define __RUCC_BMI2INTRIN_H

/* X with every bit from bit Y[7:0] up cleared. */
static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi2")))
_bzhi_u32(unsigned int __X, unsigned int __Y)
{
  unsigned int __R;
  __asm__("bzhil %2, %1, %0" : "=r"(__R) : "rm"(__X), "r"(__Y) : "cc");
  return __R;
}

/* The low bits of X put in the places of the set bits of the mask Y. */
static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi2")))
_pdep_u32(unsigned int __X, unsigned int __Y)
{
  unsigned int __R;
  __asm__("pdepl %2, %1, %0" : "=r"(__R) : "r"(__X), "rm"(__Y));
  return __R;
}

/* The bits of X at the set bits of the mask Y, packed into the low bits. */
static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi2")))
_pext_u32(unsigned int __X, unsigned int __Y)
{
  unsigned int __R;
  __asm__("pextl %2, %1, %0" : "=r"(__R) : "r"(__X), "rm"(__Y));
  return __R;
}

#ifdef __x86_64__

/* X with every bit from bit Y[7:0] up cleared. */
static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi2")))
_bzhi_u64(unsigned long long __X, unsigned long long __Y)
{
  unsigned long long __R;
  __asm__("bzhiq %2, %1, %0" : "=r"(__R) : "rm"(__X), "r"(__Y) : "cc");
  return __R;
}

/* The low bits of X put in the places of the set bits of the mask Y. */
static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi2")))
_pdep_u64(unsigned long long __X, unsigned long long __Y)
{
  unsigned long long __R;
  __asm__("pdepq %2, %1, %0" : "=r"(__R) : "r"(__X), "rm"(__Y));
  return __R;
}

/* The bits of X at the set bits of the mask Y, packed into the low bits. */
static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi2")))
_pext_u64(unsigned long long __X, unsigned long long __Y)
{
  unsigned long long __R;
  __asm__("pextq %2, %1, %0" : "=r"(__R) : "r"(__X), "rm"(__Y));
  return __R;
}

/* The full product of X and Y. The high half goes to P and the low half is returned. */
static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi2")))
_mulx_u64(unsigned long long __X, unsigned long long __Y, unsigned long long *__P)
{
  unsigned __int128 __R = (unsigned __int128)__X * __Y;
  *__P = (unsigned long long)(__R >> 64);
  return (unsigned long long)__R;
}

#else /* !__x86_64__ */

/* The full product of X and Y. The high half goes to P and the low half is returned. */
static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi2")))
_mulx_u32(unsigned int __X, unsigned int __Y, unsigned int *__P)
{
  unsigned long long __R = (unsigned long long)__X * __Y;
  *__P = (unsigned int)(__R >> 32);
  return (unsigned int)__R;
}

#endif /* __x86_64__ */

#endif /* __RUCC_BMI2INTRIN_H */
