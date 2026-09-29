/* setjmp.h, where Microsoft's C runtime says how a nonlocal jump is set up.
 *
 * The MSVC toolset's copy is used, and on x64 its `setjmp` is changed. That header spells it
 * `_setjmp(buf)`, but the `_setjmp` in the C runtime reads a second argument from RDX: the frame
 * of the function that called it, which `longjmp` hands to RtlUnwindEx as the frame to unwind to.
 * cl.exe and clang pass that argument themselves, and a call with one argument leaves whatever RDX
 * held, so `longjmp` unwound to nowhere and the program died. Wine does not look at the frame,
 * which is why this only showed on Windows. mingw-w64 passes `__builtin_frame_address(0)` in its
 * own header, and this does the same. Microsoft's header declares `_setjmp` with one parameter
 * unless `_INC_SETJMPEX` says `<setjmpex.h>` has taken the name, so that is said for the length of
 * the include, and `_setjmp` is declared here with the two it has.
 *
 * Anywhere else the name belongs to the library, which already does what its compiler needs.
 */

#if defined(_MSC_VER) && defined(_M_X64) && !defined(_INC_SETJMP) && !defined(_INC_SETJMPEX)
#define __RUCC_SETJMP_FRAME
#define _INC_SETJMPEX
#endif

#include_next <setjmp.h>

#if defined(__RUCC_SETJMP_FRAME) && !defined(__RUCC_SETJMP_H)
#define __RUCC_SETJMP_H
#undef _INC_SETJMPEX
int __cdecl _setjmp(jmp_buf, void *) __attribute__((__returns_twice__));
#define setjmp(buf) _setjmp((buf), __builtin_frame_address(0))
#endif
