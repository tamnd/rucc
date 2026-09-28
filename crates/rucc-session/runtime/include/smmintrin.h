/* smmintrin.h, the SSE4.1 and SSE4.2 intrinsics.
 *
 * What is here is the part of SSE4.2 that is not about vectors at all: the four CRC32C steps,
 * `_mm_crc32_u8`, `_mm_crc32_u16`, `_mm_crc32_u32` and `_mm_crc32_u64`, each of which folds one
 * more value of its width into a running checksum, and through `<popcntintrin.h>` the two
 * population counts that gcc's copy of this header also brings with it. These are the names
 * PostgreSQL's configure looks for, and a checksum that is computed a byte at a time in software
 * is what a program that names them is trying not to do.
 *
 * The SSE4.1 vector operations and the SSE4.2 string comparisons are not here yet, and neither is
 * `<tmmintrin.h>`, the SSSE3 header gcc's copy includes first. A program that calls one gets a
 * diagnostic naming the function. `__SSE4_1__` is still defined when the command line asks for
 * it, because gcc defines it and configure scripts read it as a statement about the machine the
 * code will run on rather than about which names a header has.
 *
 * # Why inline assembly and not a builtin
 *
 * gcc writes each step over `__builtin_ia32_crc32qi` and its three siblings, which its back end
 * turns into one instruction. The place in this compiler for an instruction the middle of the
 * compiler does not understand is the target intrinsic opcode, and the back end does not lower
 * that opcode yet, which is `tamnd/rucc#200`. Inline assembly already goes from the template to
 * the encoder, and a function that is nothing but one `crc32` inlines into its caller like any
 * other, so the instruction a loop runs is the one gcc's loop runs. The arguments are the
 * instruction's own: the running value comes in and goes out in the same register, and the new
 * data may be a register or memory, which is why its constraint is `rm`.
 *
 * # Why they are behind a macro
 *
 * For the reason `<popcntintrin.h>` gives at more length: gcc refuses a call from a function not
 * built for the instruction, this compiler cannot yet tell functions apart, so the names exist
 * when `__CRC32__` is defined and not otherwise. `-mcrc32`, `-msse4.2` and `-march=x86-64-v2`
 * define it, which is what gcc does. See `tamnd/rucc#2003`.
 */

#ifndef __RUCC_SMMINTRIN_H
#define __RUCC_SMMINTRIN_H

#include <emmintrin.h>
#include <popcntintrin.h>

#ifdef __CRC32__

static __inline__ unsigned int __attribute__((__always_inline__))
_mm_crc32_u8(unsigned int __C, unsigned char __V)
{
  __asm__("crc32b %1, %0" : "+r"(__C) : "rm"(__V));
  return __C;
}

static __inline__ unsigned int __attribute__((__always_inline__))
_mm_crc32_u16(unsigned int __C, unsigned short __V)
{
  __asm__("crc32w %1, %0" : "+r"(__C) : "rm"(__V));
  return __C;
}

static __inline__ unsigned int __attribute__((__always_inline__))
_mm_crc32_u32(unsigned int __C, unsigned int __V)
{
  __asm__("crc32l %1, %0" : "+r"(__C) : "rm"(__V));
  return __C;
}

/* The only one whose running value is sixty four bits wide, because the instruction's is: the
 * sixty four bit form writes a whole register whose top half is always zero, and gcc's copy
 * hands that back as the `unsigned long long` it is. */
static __inline__ unsigned long long __attribute__((__always_inline__))
_mm_crc32_u64(unsigned long long __C, unsigned long long __V)
{
  __asm__("crc32q %1, %0" : "+r"(__C) : "rm"(__V));
  return __C;
}

#endif /* __CRC32__ */

#endif /* __RUCC_SMMINTRIN_H */
