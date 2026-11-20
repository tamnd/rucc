/* bmiintrin.h, the BMI1 intrinsics.
 *
 * The same names gcc 16.2.0 has here. Each one is built for `bmi`, so a call from a function that
 * is not built for it is refused, as gcc refuses it.
 *
 * gcc writes `andn`, `blsi`, `blsmsk` and `blsr` in plain C and leaves the instruction to its back
 * end. These do the same and give the same answer. The trailing zero count and the bit field
 * extract are the instruction in inline assembly, because their answer for zero and for a field
 * past the top is the instruction's own. See `tamnd/rucc#2322`.
 */

#ifndef __RUCC_BMIINTRIN_H
#define __RUCC_BMIINTRIN_H

/* The 32 bit count of the value with bit 16 set. The instruction has a 16 bit form, and this
 * gives the same answer, which is 16 for zero. */
static __inline__ unsigned short __attribute__((__always_inline__, __target__("bmi")))
__tzcnt_u16(unsigned short __X)
{
  unsigned int __R;
  __asm__("tzcntl %1, %0" : "=r"(__R) : "r"(__X | 0x10000u) : "cc");
  return (unsigned short)__R;
}

static __inline__ unsigned short __attribute__((__always_inline__, __target__("bmi")))
_tzcnt_u16(unsigned short __X)
{
  return __tzcnt_u16(__X);
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
__andn_u32(unsigned int __X, unsigned int __Y)
{
  return ~__X & __Y;
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
_andn_u32(unsigned int __X, unsigned int __Y)
{
  return __andn_u32(__X, __Y);
}

/* The field of X that starts at bit Y[7:0] and is Y[15:8] bits long. */
static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
__bextr_u32(unsigned int __X, unsigned int __Y)
{
  unsigned int __R;
  __asm__("bextrl %2, %1, %0" : "=r"(__R) : "rm"(__X), "r"(__Y) : "cc");
  return __R;
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
_bextr_u32(unsigned int __X, unsigned int __Y, unsigned __Z)
{
  return __bextr_u32(__X, (__Y & 0xff) | ((__Z & 0xff) << 8));
}

/* The lowest set bit alone. */
static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
__blsi_u32(unsigned int __X)
{
  return __X & -__X;
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
_blsi_u32(unsigned int __X)
{
  return __blsi_u32(__X);
}

/* Every bit up to and with the lowest set bit. */
static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
__blsmsk_u32(unsigned int __X)
{
  return __X ^ (__X - 1);
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
_blsmsk_u32(unsigned int __X)
{
  return __blsmsk_u32(__X);
}

/* The value with its lowest set bit cleared. */
static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
__blsr_u32(unsigned int __X)
{
  return __X & (__X - 1);
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
_blsr_u32(unsigned int __X)
{
  return __blsr_u32(__X);
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
__tzcnt_u32(unsigned int __X)
{
  unsigned int __R;
  __asm__("tzcntl %1, %0" : "=r"(__R) : "rm"(__X) : "cc");
  return __R;
}

static __inline__ unsigned int __attribute__((__always_inline__, __target__("bmi")))
_tzcnt_u32(unsigned int __X)
{
  return __tzcnt_u32(__X);
}

#ifdef __x86_64__

static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
__andn_u64(unsigned long long __X, unsigned long long __Y)
{
  return ~__X & __Y;
}

static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
_andn_u64(unsigned long long __X, unsigned long long __Y)
{
  return __andn_u64(__X, __Y);
}

/* The field of X that starts at bit Y[7:0] and is Y[15:8] bits long. */
static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
__bextr_u64(unsigned long long __X, unsigned long long __Y)
{
  unsigned long long __R;
  __asm__("bextrq %2, %1, %0" : "=r"(__R) : "rm"(__X), "r"(__Y) : "cc");
  return __R;
}

static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
_bextr_u64(unsigned long long __X, unsigned int __Y, unsigned int __Z)
{
  return __bextr_u64(__X, (__Y & 0xff) | ((__Z & 0xff) << 8));
}

/* The lowest set bit alone. */
static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
__blsi_u64(unsigned long long __X)
{
  return __X & -__X;
}

static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
_blsi_u64(unsigned long long __X)
{
  return __blsi_u64(__X);
}

/* Every bit up to and with the lowest set bit. */
static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
__blsmsk_u64(unsigned long long __X)
{
  return __X ^ (__X - 1);
}

static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
_blsmsk_u64(unsigned long long __X)
{
  return __blsmsk_u64(__X);
}

/* The value with its lowest set bit cleared. */
static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
__blsr_u64(unsigned long long __X)
{
  return __X & (__X - 1);
}

static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
_blsr_u64(unsigned long long __X)
{
  return __blsr_u64(__X);
}

static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
__tzcnt_u64(unsigned long long __X)
{
  unsigned long long __R;
  __asm__("tzcntq %1, %0" : "=r"(__R) : "rm"(__X) : "cc");
  return __R;
}

static __inline__ unsigned long long __attribute__((__always_inline__, __target__("bmi")))
_tzcnt_u64(unsigned long long __X)
{
  return __tzcnt_u64(__X);
}
#endif /* __x86_64__ */

#endif /* __RUCC_BMIINTRIN_H */
