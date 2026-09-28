/* cpuid.h, asking the processor what it has.
 *
 * The interface gcc's header gives, which is what programs write against: the macros `__cpuid`
 * and `__cpuid_count` that run the instruction and store the four registers into the four lvalues
 * given, the functions `__get_cpuid_max`, `__get_cpuid` and `__get_cpuid_count` that check the
 * leaf exists first and answer 0 when it does not, `__cpuidex` in the shape MSVC's `<intrin.h>`
 * has it, and the `bit_` names for the feature bits a program usually tests.
 *
 * Postgres includes this from its CRC32C and popcount choosers and probes for `__get_cpuid` and
 * `__get_cpuid_count` at configure time. Without the header both probes fail, and the build falls
 * back to the portable paths, which is a different program from the one gcc builds.
 *
 * The instruction is available on every x86-64 processor, so nothing here is behind a target
 * attribute, and the leaf checks are what keep a question about a leaf the processor does not
 * have from reading whatever the highest leaf it does have answers.
 */

#ifndef __RUCC_CPUID_H
#define __RUCC_CPUID_H

/* Leaf 1, %ecx. */
#define bit_SSE3 (1 << 0)
#define bit_PCLMUL (1 << 1)
#define bit_LZCNT (1 << 5)
#define bit_SSSE3 (1 << 9)
#define bit_FMA (1 << 12)
#define bit_CMPXCHG16B (1 << 13)
#define bit_SSE4_1 (1 << 19)
#define bit_SSE4_2 (1 << 20)
#define bit_MOVBE (1 << 22)
#define bit_POPCNT (1 << 23)
#define bit_AES (1 << 25)
#define bit_XSAVE (1 << 26)
#define bit_OSXSAVE (1 << 27)
#define bit_AVX (1 << 28)
#define bit_F16C (1 << 29)
#define bit_RDRND (1 << 30)

/* Leaf 1, %edx. */
#define bit_CMPXCHG8B (1 << 8)
#define bit_CMOV (1 << 15)
#define bit_MMX (1 << 23)
#define bit_FXSAVE (1 << 24)
#define bit_SSE (1 << 25)
#define bit_SSE2 (1 << 26)

/* Leaf 0x80000001, %ecx and %edx. */
#define bit_LAHF_LM (1 << 0)
#define bit_ABM (1 << 5)
#define bit_SSE4a (1 << 6)
#define bit_PRFCHW (1 << 8)
#define bit_RDTSCP (1 << 27)
#define bit_LM (1 << 29)

/* Leaf 7, subleaf 0, %ebx. */
#define bit_FSGSBASE (1 << 0)
#define bit_BMI (1 << 3)
#define bit_HLE (1 << 4)
#define bit_AVX2 (1 << 5)
#define bit_BMI2 (1 << 8)
#define bit_RTM (1 << 11)
#define bit_AVX512F (1 << 16)
#define bit_AVX512DQ (1 << 17)
#define bit_RDSEED (1 << 18)
#define bit_ADX (1 << 19)
#define bit_AVX512IFMA (1 << 21)
#define bit_CLFLUSHOPT (1 << 23)
#define bit_CLWB (1 << 24)
#define bit_AVX512CD (1 << 28)
#define bit_SHA (1 << 29)
#define bit_AVX512BW (1 << 30)
#define bit_AVX512VL (1u << 31)

/* Leaf 7, subleaf 0, %ecx. */
#define bit_AVX512VBMI (1 << 1)
#define bit_AVX512VBMI2 (1 << 6)
#define bit_GFNI (1 << 8)
#define bit_VAES (1 << 9)
#define bit_VPCLMULQDQ (1 << 10)
#define bit_AVX512VNNI (1 << 11)
#define bit_AVX512BITALG (1 << 12)
#define bit_AVX512VPOPCNTDQ (1 << 14)
#define bit_RDPID (1 << 22)

/* Leaf 0xd, subleaf 1, %eax. */
#define bit_XSAVEOPT (1 << 0)
#define bit_XSAVEC (1 << 1)
#define bit_XSAVES (1 << 3)

#define __cpuid(level, a, b, c, d)                                                                 \
  __asm__ __volatile__("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "0"(level))

#define __cpuid_count(level, count, a, b, c, d)                                                    \
  __asm__ __volatile__("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "0"(level), "2"(count))

/* The highest leaf of the range `__ext` starts, 0 or 0x80000000, with the vendor's first word
   stored through `__sig` when it is not null. */
static __inline__ unsigned int __get_cpuid_max(unsigned int __ext, unsigned int *__sig)
{
  unsigned int __eax, __ebx, __ecx, __edx;
  __cpuid(__ext, __eax, __ebx, __ecx, __edx);
  if (__sig)
    *__sig = __ebx;
  return __eax;
}

static __inline__ int __get_cpuid(unsigned int __leaf, unsigned int *__eax, unsigned int *__ebx,
                                  unsigned int *__ecx, unsigned int *__edx)
{
  unsigned int __max = __get_cpuid_max(__leaf & 0x80000000, 0);
  if (__max == 0 || __max < __leaf)
    return 0;
  __cpuid(__leaf, *__eax, *__ebx, *__ecx, *__edx);
  return 1;
}

static __inline__ int __get_cpuid_count(unsigned int __leaf, unsigned int __subleaf,
                                        unsigned int *__eax, unsigned int *__ebx,
                                        unsigned int *__ecx, unsigned int *__edx)
{
  unsigned int __max = __get_cpuid_max(__leaf & 0x80000000, 0);
  if (__max == 0 || __max < __leaf)
    return 0;
  __cpuid_count(__leaf, __subleaf, *__eax, *__ebx, *__ecx, *__edx);
  return 1;
}

static __inline__ void __cpuidex(int __info[4], int __leaf, int __subleaf)
{
  __cpuid_count(__leaf, __subleaf, __info[0], __info[1], __info[2], __info[3]);
}

#endif /* __RUCC_CPUID_H */
