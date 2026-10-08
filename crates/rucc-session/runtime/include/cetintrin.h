/* cetintrin.h, the shadow stack instructions of Intel CET.
 *
 * The names and types are those of gcc's header. pcre2 is the reason it is here: its JIT reads the
 * shadow stack pointer with `_get_ssp` when `__SHSTK__` is defined, and its configure script adds
 * `-mshstk` when the build has `-fcf-protection`. A distribution build has that flag.
 *
 * `rdssp` does nothing when the shadow stack is off, and the register keeps what it had. So
 * `_get_ssp` sets it to zero first and returns zero when there is no shadow stack, as gcc's
 * builtin does. A program asks the question that way.
 *
 * Each function is built for `shstk`, as in gcc, so a function that is not built for it cannot
 * call one. The 64-bit forms are there only on x86-64.
 */

#ifndef __RUCC_CETINTRIN_H
#define __RUCC_CETINTRIN_H

#ifdef __x86_64__
static __inline__ unsigned long long __attribute__((__always_inline__, __target__("shstk")))
_get_ssp(void)
{
  unsigned long long __r = 0;
  __asm__ __volatile__("rdsspq %0" : "+r"(__r));
  return __r;
}
#else
static __inline__ unsigned int __attribute__((__always_inline__, __target__("shstk")))
_get_ssp(void)
{
  unsigned int __r = 0;
  __asm__ __volatile__("rdsspd %0" : "+r"(__r));
  return __r;
}
#endif

static __inline__ void __attribute__((__always_inline__, __target__("shstk")))
_inc_ssp(unsigned int __B)
{
#ifdef __x86_64__
  __asm__ __volatile__("incsspq %0" : : "r"((unsigned long long)__B) : "memory");
#else
  __asm__ __volatile__("incsspd %0" : : "r"(__B) : "memory");
#endif
}

static __inline__ void __attribute__((__always_inline__, __target__("shstk")))
_saveprevssp(void)
{
  __asm__ __volatile__("saveprevssp" : : : "memory");
}

static __inline__ void __attribute__((__always_inline__, __target__("shstk")))
_rstorssp(void *__B)
{
  __asm__ __volatile__("rstorssp (%0)" : : "r"(__B) : "memory");
}

static __inline__ void __attribute__((__always_inline__, __target__("shstk")))
_wrssd(unsigned int __B, void *__C)
{
  __asm__ __volatile__("wrssd %0, (%1)" : : "r"(__B), "r"(__C) : "memory");
}

static __inline__ void __attribute__((__always_inline__, __target__("shstk")))
_wrussd(unsigned int __B, void *__C)
{
  __asm__ __volatile__("wrussd %0, (%1)" : : "r"(__B), "r"(__C) : "memory");
}

#ifdef __x86_64__
static __inline__ void __attribute__((__always_inline__, __target__("shstk")))
_wrssq(unsigned long long __B, void *__C)
{
  __asm__ __volatile__("wrssq %0, (%1)" : : "r"(__B), "r"(__C) : "memory");
}

static __inline__ void __attribute__((__always_inline__, __target__("shstk")))
_wrussq(unsigned long long __B, void *__C)
{
  __asm__ __volatile__("wrussq %0, (%1)" : : "r"(__B), "r"(__C) : "memory");
}
#endif

static __inline__ void __attribute__((__always_inline__, __target__("shstk")))
_setssbsy(void)
{
  __asm__ __volatile__("setssbsy" : : : "memory");
}

static __inline__ void __attribute__((__always_inline__, __target__("shstk")))
_clrssbsy(void *__B)
{
  __asm__ __volatile__("clrssbsy (%0)" : : "r"(__B) : "memory");
}

#endif /* __RUCC_CETINTRIN_H */
