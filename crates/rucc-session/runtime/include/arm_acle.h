/* arm_acle.h, the Arm C Language Extensions that are not vectors.
 *
 * What is here is the eight CRC32 intrinsics and nothing else of the ACLE yet. Each folds one more
 * value into a running checksum and is one instruction: `__crc32b`, `__crc32h`, `__crc32w` and
 * `__crc32d` over a byte, a halfword, a word and a doubleword with the CRC-32 polynomial that zlib
 * and Ethernet use, and `__crc32cb`, `__crc32ch`, `__crc32cw` and `__crc32cd` over the same with
 * the Castagnoli polynomial, CRC-32C, which is the one PostgreSQL checksums its write ahead log
 * with in `src/port/pg_crc32c_armv8.c`. The running value is 32 bits in every one of them, and
 * only the data is wider in the `d` forms. The names and types are the ACLE's and gcc's.
 *
 * # Why the steps are inline assembly and not a builtin
 *
 * For the reason `<smmintrin.h>` gives for x86-64's CRC32C steps. gcc writes each over
 * `__builtin_aarch64_crc32cb` and its siblings, which its back end turns into one instruction, and
 * the back end here does not lower a target intrinsic yet, which is `tamnd/rucc#200`. Inline
 * assembly goes from the template to the encoder, and a function that is nothing but one `crc32cb`
 * inlines into its caller like any other, so the loop PostgreSQL runs has the instruction in it.
 * The running value comes in and goes out in the same register, and the data is always a register,
 * since this machine has no form that reads memory. The instruction reads only as many bits of the
 * data register as its size, so a narrow argument needs no widening first.
 *
 * # Why each is built for `+crc`
 *
 * The instructions belong to the CRC32 extension, which is optional in Armv8.0 and part of every
 * architecture from Armv8.1-A on. gcc's copy defines the intrinsics under
 * `#pragma GCC target("+nothing+crc")`, so they are there whatever the command line said, and then
 * refuses a call from a function not built for the extension with "target specific option
 * mismatch". Each function below carries `__attribute__((target("+crc")))` to the same effect,
 * which this compiler reads the way gcc does, so:
 *
 * - `-march=armv8-a+crc`, `-march=armv8-a+crc+simd` and `-march=armv8.1-a` and later build the
 *   whole unit for the extension, define `__ARM_FEATURE_CRC32`, and any function may call these.
 * - Plain `-march=armv8-a`, which is what gcc for aarch64-linux-gnu builds for without a flag,
 *   defines no `__ARM_FEATURE_CRC32` and refuses the call, which is the answer PostgreSQL's
 *   configure reads before it tries `-march=armv8-a+crc+simd`. With that flag it builds the
 *   instructions and chooses between them and the portable loop at run time, which is what it
 *   does under gcc.
 * - A function carrying `__attribute__((target("+crc")))` is built for the extension whatever the
 *   rest of the unit is, and may call them.
 *
 * See `tamnd/rucc#2006`, and `tamnd/rucc#2002` and `tamnd/rucc#2045` for the x86-64 side.
 */

#ifndef __RUCC_ARM_ACLE_H
#define __RUCC_ARM_ACLE_H

#if !defined(__aarch64__)
#error "arm_acle.h is for AArch64"
#endif

#include <stdint.h>

/* ## CRC-32
 *
 * The polynomial 0x04C11DB7, bit reflected, which is what `crc32` in zlib computes when the value
 * starts at all ones and is inverted at the end. */

static __inline__ uint32_t __attribute__((__always_inline__, __target__("+crc")))
__crc32b(uint32_t __a, uint8_t __b)
{
  __asm__("crc32b %w0, %w0, %w1" : "+r"(__a) : "r"(__b));
  return __a;
}

static __inline__ uint32_t __attribute__((__always_inline__, __target__("+crc")))
__crc32h(uint32_t __a, uint16_t __b)
{
  __asm__("crc32h %w0, %w0, %w1" : "+r"(__a) : "r"(__b));
  return __a;
}

static __inline__ uint32_t __attribute__((__always_inline__, __target__("+crc")))
__crc32w(uint32_t __a, uint32_t __b)
{
  __asm__("crc32w %w0, %w0, %w1" : "+r"(__a) : "r"(__b));
  return __a;
}

static __inline__ uint32_t __attribute__((__always_inline__, __target__("+crc")))
__crc32d(uint32_t __a, uint64_t __b)
{
  __asm__("crc32x %w0, %w0, %x1" : "+r"(__a) : "r"(__b));
  return __a;
}

/* ## CRC-32C
 *
 * The Castagnoli polynomial 0x1EDC6F41, bit reflected, which is what x86-64's `crc32` instruction
 * computes as well, so these and `_mm_crc32_u8` and its siblings give the same answer for the same
 * data. */

static __inline__ uint32_t __attribute__((__always_inline__, __target__("+crc")))
__crc32cb(uint32_t __a, uint8_t __b)
{
  __asm__("crc32cb %w0, %w0, %w1" : "+r"(__a) : "r"(__b));
  return __a;
}

static __inline__ uint32_t __attribute__((__always_inline__, __target__("+crc")))
__crc32ch(uint32_t __a, uint16_t __b)
{
  __asm__("crc32ch %w0, %w0, %w1" : "+r"(__a) : "r"(__b));
  return __a;
}

static __inline__ uint32_t __attribute__((__always_inline__, __target__("+crc")))
__crc32cw(uint32_t __a, uint32_t __b)
{
  __asm__("crc32cw %w0, %w0, %w1" : "+r"(__a) : "r"(__b));
  return __a;
}

static __inline__ uint32_t __attribute__((__always_inline__, __target__("+crc")))
__crc32cd(uint32_t __a, uint64_t __b)
{
  __asm__("crc32cx %w0, %w0, %x1" : "+r"(__a) : "r"(__b));
  return __a;
}

#endif /* __RUCC_ARM_ACLE_H */
