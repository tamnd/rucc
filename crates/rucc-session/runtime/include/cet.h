/* cet.h, the parts of Intel CET that an assembly file writes by hand.
 *
 * gcc ships this header on x86, and an assembly file includes it when the configure script finds
 * it. libsodium does this for its Salsa20 and Curve25519 code. The header gives `_CET_ENDBR`,
 * which is `endbr64` or `endbr32` when the branch half of `-fcf-protection` is on, and nothing
 * otherwise.
 *
 * When `__CET__` says that a half is on, the header also writes the GNU property note that says
 * which halves the file keeps. The linker sets a half in the output only when each input says so.
 * So one assembly file without the note turns off IBT and SHSTK for the whole program or library.
 *
 * The note is NT_GNU_PROPERTY_TYPE_0 from the x86-64 psABI, with one property,
 * GNU_PROPERTY_X86_FEATURE_1_AND (0xc0000002). Bit 1 is IBT and bit 2 is SHSTK. The note is
 * aligned to 8 bytes in a 64-bit file and to 4 bytes in a 32-bit file.
 *
 * C code gets nothing from this header, because the compiler writes the note and each `endbr`.
 */

#ifndef __RUCC_CET_H
#define __RUCC_CET_H

#ifdef __ASSEMBLER__

#if defined(__CET__) && (__CET__ & 1)
#ifdef __x86_64__
#define _CET_ENDBR endbr64
#else
#define _CET_ENDBR endbr32
#endif
#else
#define _CET_ENDBR
#endif

#if defined(__CET__) && (__CET__ & 3)
#ifdef __LP64__
#define __RUCC_CET_ALIGN 3
#else
#define __RUCC_CET_ALIGN 2
#endif
	.pushsection ".note.gnu.property", "a"
	.p2align __RUCC_CET_ALIGN
	.long 1f - 0f
	.long 4f - 1f
	.long 5
0:
	.asciz "GNU"
1:
	.p2align __RUCC_CET_ALIGN
	.long 0xc0000002
	.long 3f - 2f
2:
	.long __CET__ & 3
3:
	.p2align __RUCC_CET_ALIGN
4:
	.popsection
#endif

#endif

#endif
