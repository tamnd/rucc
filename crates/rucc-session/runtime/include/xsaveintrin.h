/* xsaveintrin.h, reading the extended control register.
 *
 * One name so far, `_xgetbv`, which returns the register `__A` selects as `%edx:%eax` joined into
 * one value. Register 0 is the one programs ask about: its bits say which register state the
 * operating system saves on a context switch, and a program that wants AVX or AVX-512 checks them
 * after `cpuid`, since a processor with the registers is no use when the kernel does not keep
 * them. Postgres's CRC32C and popcount choosers are the reason it is here.
 *
 * Built for `xsave` for the reason the population counts are built for `popcnt`: gcc defines the
 * name whatever the command line said and refuses a call from a function not built for it, and a
 * configure probe learns from that refusal whether it needs `__attribute__((target("xsave")))`.
 * Postgres's probe writes the attribute, and gets the same answer from both compilers.
 */

#ifndef __RUCC_XSAVEINTRIN_H
#define __RUCC_XSAVEINTRIN_H

static __inline__ unsigned long long __attribute__((__always_inline__, __target__("xsave")))
_xgetbv(unsigned int __A)
{
  unsigned int __lo, __hi;
  __asm__ __volatile__("xgetbv" : "=a"(__lo), "=d"(__hi) : "c"(__A));
  return ((unsigned long long)__hi << 32) | __lo;
}

#endif /* __RUCC_XSAVEINTRIN_H */
