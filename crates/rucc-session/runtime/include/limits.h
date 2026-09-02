/* limits.h, the ranges of the integer types.
 *
 * Every value here is the compiler's answer and not the library's, so every one of them is
 * defined over the compiler's own macros and overrides whatever the system header said. The
 * system header is included first all the same, because on a hosted implementation it is
 * where `PATH_MAX`, `NAME_MAX` and the rest of the POSIX limits live, and a program that
 * includes `<limits.h>` expects those too.
 *
 * `_GCC_LIMITS_H_` is set before the chain rather than after it. glibc's own `<limits.h>`
 * reads that name to decide whether the compiler's header has already been seen, and
 * includes the compiler's when it has not. Setting it first is what stops the two headers
 * including each other. */

#ifndef __RUCC_LIMITS_H
#define __RUCC_LIMITS_H

#ifndef _GCC_LIMITS_H_
#define _GCC_LIMITS_H_
#endif

#if defined(__STDC_HOSTED__) && __STDC_HOSTED__ && __has_include_next(<limits.h>)
#include_next <limits.h>
#endif

#undef CHAR_BIT
#define CHAR_BIT __CHAR_BIT__

#undef SCHAR_MIN
#undef SCHAR_MAX
#undef UCHAR_MAX
#define SCHAR_MIN (-__SCHAR_MAX__ - 1)
#define SCHAR_MAX __SCHAR_MAX__
#define UCHAR_MAX (__SCHAR_MAX__ * 2 + 1)

/* Whether a plain `char` is the signed one or the unsigned one is the target's choice, and
 * on the two that matter here it is made differently: x86-64 says signed and AArch64 says
 * unsigned. The compiler says which by defining the flag. */
#undef CHAR_MIN
#undef CHAR_MAX
#ifdef __CHAR_UNSIGNED__
#define CHAR_MIN 0
#define CHAR_MAX UCHAR_MAX
#else
#define CHAR_MIN SCHAR_MIN
#define CHAR_MAX SCHAR_MAX
#endif

#undef SHRT_MIN
#undef SHRT_MAX
#undef USHRT_MAX
#define SHRT_MIN (-__SHRT_MAX__ - 1)
#define SHRT_MAX __SHRT_MAX__
#define USHRT_MAX (__SHRT_MAX__ * 2 + 1)

#undef INT_MIN
#undef INT_MAX
#undef UINT_MAX
#define INT_MIN (-__INT_MAX__ - 1)
#define INT_MAX __INT_MAX__
#define UINT_MAX (__INT_MAX__ * 2U + 1U)

#undef LONG_MIN
#undef LONG_MAX
#undef ULONG_MAX
#define LONG_MIN (-__LONG_MAX__ - 1L)
#define LONG_MAX __LONG_MAX__
#define ULONG_MAX (__LONG_MAX__ * 2UL + 1UL)

#undef LLONG_MIN
#undef LLONG_MAX
#undef ULLONG_MAX
#define LLONG_MIN (-__LONG_LONG_MAX__ - 1LL)
#define LLONG_MAX __LONG_LONG_MAX__
#define ULLONG_MAX (__LONG_LONG_MAX__ * 2ULL + 1ULL)

/* The GNU spellings, which predate the C99 ones and which a great deal of code still uses
 * because it was written when `long long` was an extension. */
#undef LONG_LONG_MIN
#undef LONG_LONG_MAX
#undef ULONG_LONG_MAX
#define LONG_LONG_MIN LLONG_MIN
#define LONG_LONG_MAX LLONG_MAX
#define ULONG_LONG_MAX ULLONG_MAX

/* The longest multibyte character in any locale. One is the answer for a freestanding
 * implementation, and a hosted library that supports more will have said so already. */
#ifndef MB_LEN_MAX
#define MB_LEN_MAX 1
#endif

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 202311L
#undef BOOL_MAX
#undef BOOL_WIDTH
#undef CHAR_WIDTH
#undef SCHAR_WIDTH
#undef UCHAR_WIDTH
#undef SHRT_WIDTH
#undef USHRT_WIDTH
#undef INT_WIDTH
#undef UINT_WIDTH
#undef LONG_WIDTH
#undef ULONG_WIDTH
#undef LLONG_WIDTH
#undef ULLONG_WIDTH
#undef BITINT_MAXWIDTH
#define BOOL_MAX 1
#define BOOL_WIDTH 1
#define CHAR_WIDTH __SCHAR_WIDTH__
#define SCHAR_WIDTH __SCHAR_WIDTH__
#define UCHAR_WIDTH __SCHAR_WIDTH__
#define SHRT_WIDTH __SHRT_WIDTH__
#define USHRT_WIDTH __SHRT_WIDTH__
#define INT_WIDTH __INT_WIDTH__
#define UINT_WIDTH __INT_WIDTH__
#define LONG_WIDTH __LONG_WIDTH__
#define ULONG_WIDTH __LONG_WIDTH__
#define LLONG_WIDTH __LONG_LONG_WIDTH__
#define ULLONG_WIDTH __LONG_LONG_WIDTH__
#define BITINT_MAXWIDTH __BITINT_MAXWIDTH__
#endif

#endif
