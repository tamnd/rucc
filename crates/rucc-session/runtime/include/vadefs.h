/* vadefs.h, where Microsoft's C runtime says how a variable argument list is walked.
 *
 * The MSVC toolset's copy is used, and one line of it is changed. On x64 it starts a list with
 * `__va_start(&ap, x)`, which is a function to nobody but cl.exe: cl turns the call into the
 * address of the home slot after `x`, and no library defines it. clang knows the name as a builtin.
 * This compiler knows `__builtin_va_start` instead, and on the Windows rows its `va_list` is the
 * `char *` Microsoft's is, so the one macro is pointed at that and the rest of the header, which
 * walks the list with pointer arithmetic, stands as written. The universal CRT's inline `printf`
 * family starts every list through this macro.
 *
 * Anywhere else the name belongs to the library, and mingw-w64 has a vadefs.h of its own.
 */

#include_next <vadefs.h>

#if defined(_MSC_VER) && defined(_M_X64) && !defined(__RUCC_VADEFS_H)
#define __RUCC_VADEFS_H
#undef __crt_va_start_a
#define __crt_va_start_a(ap, x) ((void)__builtin_va_start(ap, x))
#endif
