/* stdnoreturn.h, one macro.
 *
 * C23 made the header obsolescent but kept the macro, and gcc and clang define it in every
 * mode. A program that writes `[[noreturn]]` after the header still works, because C23 takes
 * `[[_Noreturn]]` as the same attribute. The tools build of NetBSD includes it in nawk and then
 * writes `noreturn void FATAL(...)`. */

#ifndef __RUCC_STDNORETURN_H
#define __RUCC_STDNORETURN_H

#define noreturn _Noreturn

#endif
