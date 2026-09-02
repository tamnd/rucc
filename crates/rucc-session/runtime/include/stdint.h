/* stdint.h, the integer types with the widths written into their names.
 *
 * On a hosted implementation the library's own is the one to use. It is not just the same
 * typedefs under a different roof: glibc's declares `imaxdiv_t`, arranges the C++ limit
 * macro conditionals, and is what the rest of glibc's headers were written against, so
 * chaining to it is the only way the rest of the system agrees with itself. That is what
 * `#include_next` is for and it is what GCC does here.
 *
 * Freestanding, there is no library to chain to and every one of these is written out from
 * the compiler's own macros, which are the same ones the library's header would have used. */

#ifndef __RUCC_STDINT_H
#define __RUCC_STDINT_H

#if defined(__STDC_HOSTED__) && __STDC_HOSTED__ && __has_include_next(<stdint.h>)
#include_next <stdint.h>
#else

typedef __INT8_TYPE__ int8_t;
typedef __UINT8_TYPE__ uint8_t;
typedef __INT16_TYPE__ int16_t;
typedef __UINT16_TYPE__ uint16_t;
typedef __INT32_TYPE__ int32_t;
typedef __UINT32_TYPE__ uint32_t;
typedef __INT64_TYPE__ int64_t;
typedef __UINT64_TYPE__ uint64_t;

typedef __INT_LEAST8_TYPE__ int_least8_t;
typedef __UINT_LEAST8_TYPE__ uint_least8_t;
typedef __INT_LEAST16_TYPE__ int_least16_t;
typedef __UINT_LEAST16_TYPE__ uint_least16_t;
typedef __INT_LEAST32_TYPE__ int_least32_t;
typedef __UINT_LEAST32_TYPE__ uint_least32_t;
typedef __INT_LEAST64_TYPE__ int_least64_t;
typedef __UINT_LEAST64_TYPE__ uint_least64_t;

typedef __INT_FAST8_TYPE__ int_fast8_t;
typedef __UINT_FAST8_TYPE__ uint_fast8_t;
typedef __INT_FAST16_TYPE__ int_fast16_t;
typedef __UINT_FAST16_TYPE__ uint_fast16_t;
typedef __INT_FAST32_TYPE__ int_fast32_t;
typedef __UINT_FAST32_TYPE__ uint_fast32_t;
typedef __INT_FAST64_TYPE__ int_fast64_t;
typedef __UINT_FAST64_TYPE__ uint_fast64_t;

typedef __INTPTR_TYPE__ intptr_t;
typedef __UINTPTR_TYPE__ uintptr_t;
typedef __INTMAX_TYPE__ intmax_t;
typedef __UINTMAX_TYPE__ uintmax_t;

#define INT8_MAX __INT8_MAX__
#define INT8_MIN (-__INT8_MAX__ - 1)
#define UINT8_MAX __UINT8_MAX__
#define INT_LEAST8_MAX __INT_LEAST8_MAX__
#define INT_LEAST8_MIN (-__INT_LEAST8_MAX__ - 1)
#define UINT_LEAST8_MAX __UINT_LEAST8_MAX__
#define INT_FAST8_MAX __INT_FAST8_MAX__
#define INT_FAST8_MIN (-__INT_FAST8_MAX__ - 1)
#define UINT_FAST8_MAX __UINT_FAST8_MAX__
#define INT16_MAX __INT16_MAX__
#define INT16_MIN (-__INT16_MAX__ - 1)
#define UINT16_MAX __UINT16_MAX__
#define INT_LEAST16_MAX __INT_LEAST16_MAX__
#define INT_LEAST16_MIN (-__INT_LEAST16_MAX__ - 1)
#define UINT_LEAST16_MAX __UINT_LEAST16_MAX__
#define INT_FAST16_MAX __INT_FAST16_MAX__
#define INT_FAST16_MIN (-__INT_FAST16_MAX__ - 1)
#define UINT_FAST16_MAX __UINT_FAST16_MAX__
#define INT32_MAX __INT32_MAX__
#define INT32_MIN (-__INT32_MAX__ - 1)
#define UINT32_MAX __UINT32_MAX__
#define INT_LEAST32_MAX __INT_LEAST32_MAX__
#define INT_LEAST32_MIN (-__INT_LEAST32_MAX__ - 1)
#define UINT_LEAST32_MAX __UINT_LEAST32_MAX__
#define INT_FAST32_MAX __INT_FAST32_MAX__
#define INT_FAST32_MIN (-__INT_FAST32_MAX__ - 1)
#define UINT_FAST32_MAX __UINT_FAST32_MAX__
#define INT64_MAX __INT64_MAX__
#define INT64_MIN (-__INT64_MAX__ - 1)
#define UINT64_MAX __UINT64_MAX__
#define INT_LEAST64_MAX __INT_LEAST64_MAX__
#define INT_LEAST64_MIN (-__INT_LEAST64_MAX__ - 1)
#define UINT_LEAST64_MAX __UINT_LEAST64_MAX__
#define INT_FAST64_MAX __INT_FAST64_MAX__
#define INT_FAST64_MIN (-__INT_FAST64_MAX__ - 1)
#define UINT_FAST64_MAX __UINT_FAST64_MAX__

#define INTPTR_MAX __INTPTR_MAX__
#define INTPTR_MIN (-__INTPTR_MAX__ - 1)
#define UINTPTR_MAX __UINTPTR_MAX__
#define INTMAX_MAX __INTMAX_MAX__
#define INTMAX_MIN (-__INTMAX_MAX__ - 1)
#define UINTMAX_MAX __UINTMAX_MAX__

#define PTRDIFF_MAX __PTRDIFF_MAX__
#define PTRDIFF_MIN (-__PTRDIFF_MAX__ - 1)
#define SIZE_MAX __SIZE_MAX__
#define SIG_ATOMIC_MAX __SIG_ATOMIC_MAX__
#define SIG_ATOMIC_MIN __SIG_ATOMIC_MIN__
#define WCHAR_MAX __WCHAR_MAX__
#define WCHAR_MIN __WCHAR_MIN__
#define WINT_MAX __WINT_MAX__
#define WINT_MIN __WINT_MIN__

#define INT8_C(value) __INT8_C(value)
#define UINT8_C(value) __UINT8_C(value)
#define INT16_C(value) __INT16_C(value)
#define UINT16_C(value) __UINT16_C(value)
#define INT32_C(value) __INT32_C(value)
#define UINT32_C(value) __UINT32_C(value)
#define INT64_C(value) __INT64_C(value)
#define UINT64_C(value) __UINT64_C(value)
#define INTMAX_C(value) __INTMAX_C(value)
#define UINTMAX_C(value) __UINTMAX_C(value)

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 202311L
#define INT8_WIDTH 8
#define UINT8_WIDTH 8
#define INT_LEAST8_WIDTH __INT_LEAST8_WIDTH__
#define UINT_LEAST8_WIDTH __INT_LEAST8_WIDTH__
#define INT_FAST8_WIDTH __INT_FAST8_WIDTH__
#define UINT_FAST8_WIDTH __INT_FAST8_WIDTH__
#define INT16_WIDTH 16
#define UINT16_WIDTH 16
#define INT_LEAST16_WIDTH __INT_LEAST16_WIDTH__
#define UINT_LEAST16_WIDTH __INT_LEAST16_WIDTH__
#define INT_FAST16_WIDTH __INT_FAST16_WIDTH__
#define UINT_FAST16_WIDTH __INT_FAST16_WIDTH__
#define INT32_WIDTH 32
#define UINT32_WIDTH 32
#define INT_LEAST32_WIDTH __INT_LEAST32_WIDTH__
#define UINT_LEAST32_WIDTH __INT_LEAST32_WIDTH__
#define INT_FAST32_WIDTH __INT_FAST32_WIDTH__
#define UINT_FAST32_WIDTH __INT_FAST32_WIDTH__
#define INT64_WIDTH 64
#define UINT64_WIDTH 64
#define INT_LEAST64_WIDTH __INT_LEAST64_WIDTH__
#define UINT_LEAST64_WIDTH __INT_LEAST64_WIDTH__
#define INT_FAST64_WIDTH __INT_FAST64_WIDTH__
#define UINT_FAST64_WIDTH __INT_FAST64_WIDTH__
#define INTPTR_WIDTH __INTPTR_WIDTH__
#define UINTPTR_WIDTH __INTPTR_WIDTH__
#define INTMAX_WIDTH __INTMAX_WIDTH__
#define UINTMAX_WIDTH __INTMAX_WIDTH__
#define PTRDIFF_WIDTH __PTRDIFF_WIDTH__
#define SIZE_WIDTH __SIZE_WIDTH__
#define SIG_ATOMIC_WIDTH __SIG_ATOMIC_WIDTH__
#define WCHAR_WIDTH __WCHAR_WIDTH__
#define WINT_WIDTH __WINT_WIDTH__
#endif

#endif /* hosted */
#endif
