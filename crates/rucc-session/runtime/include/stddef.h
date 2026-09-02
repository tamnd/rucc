/* stddef.h, the types that are the target's answers rather than the library's.
 *
 * Every one of these is decided by the ABI, so the compiler is the only thing that knows
 * them and this header is the compiler's on every implementation.
 *
 * The `__need_` protocol is the part that is not obvious. glibc does not include this header
 * whole. `<stdio.h>` writes `#define __need_size_t` and `#define __need_NULL` and then
 * includes it, because it wants those two names and must not put `offsetof` in the user's
 * way. Each request is answered on its own and clears itself, and the header is left
 * unguarded for that case so that the next request gets through. Only the whole form sets
 * `_STDDEF_H`.
 *
 * Each typedef is also guarded by the names the platform libraries use for the same job, and
 * this header sets all of them. That is deliberate: a second typedef of the same type is
 * legal C11 but is an error under `-std=c99 -pedantic-errors`, and musl's
 * `<bits/alltypes.h>` will write one unless `__DEFINED_size_t` is already set. */

/* The whole form, which is any inclusion that did not ask for one piece. */
#if !defined(__need_ptrdiff_t) && !defined(__need_size_t) && !defined(__need_rsize_t) \
    && !defined(__need_wchar_t) && !defined(__need_wint_t) && !defined(__need_NULL) \
    && !defined(__need_max_align_t) && !defined(__need_nullptr_t)
#define __need_ptrdiff_t
#define __need_size_t
#define __need_wchar_t
#define __need_NULL
#define __need_max_align_t
#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 202311L
#define __need_nullptr_t
#endif
#define __RUCC_STDDEF_WHOLE
#endif

#ifdef __need_ptrdiff_t
#undef __need_ptrdiff_t
#if !defined(_PTRDIFF_T) && !defined(_PTRDIFF_T_) && !defined(_PTRDIFF_T_DEFINED) \
    && !defined(_PTRDIFF_T_DECLARED) && !defined(__DEFINED_ptrdiff_t) \
    && !defined(__ptrdiff_t_defined)
#define _PTRDIFF_T
#define _PTRDIFF_T_
#define _PTRDIFF_T_DEFINED
#define _PTRDIFF_T_DECLARED
#define __DEFINED_ptrdiff_t
#define __ptrdiff_t_defined
typedef __PTRDIFF_TYPE__ ptrdiff_t;
#endif
#endif

#ifdef __need_size_t
#undef __need_size_t
#if !defined(_SIZE_T) && !defined(_SIZE_T_) && !defined(_SIZE_T_DEFINED) \
    && !defined(_SIZE_T_DEFINED_) && !defined(_SIZE_T_DECLARED) \
    && !defined(_BSD_SIZE_T_DEFINED_) && !defined(__DEFINED_size_t) \
    && !defined(__size_t_defined) && !defined(___int_size_t_h)
#define _SIZE_T
#define _SIZE_T_
#define _SIZE_T_DEFINED
#define _SIZE_T_DEFINED_
#define _SIZE_T_DECLARED
#define _BSD_SIZE_T_DEFINED_
#define __DEFINED_size_t
#define __size_t_defined
#define ___int_size_t_h
typedef __SIZE_TYPE__ size_t;
#endif
#endif

/* Annex K's `rsize_t`, which is `size_t` under a name that says the value is a count of
 * bytes that a bounds checked function will refuse when it is too large. Only handed out
 * when it is asked for by name, since nothing declares it otherwise. */
#ifdef __need_rsize_t
#undef __need_rsize_t
#if !defined(_RSIZE_T) && !defined(_RSIZE_T_DEFINED) && !defined(__DEFINED_rsize_t)
#define _RSIZE_T
#define _RSIZE_T_DEFINED
#define __DEFINED_rsize_t
typedef __SIZE_TYPE__ rsize_t;
#endif
#endif

#ifdef __need_wchar_t
#undef __need_wchar_t
/* C++ makes `wchar_t` a keyword, so the typedef would be a redeclaration of a type name
 * rather than a definition of one. This compiler does not compile C++ and the guard is here
 * so that a header shared with a C++ build does not have to care. */
#ifndef __cplusplus
#if !defined(_WCHAR_T) && !defined(_WCHAR_T_) && !defined(_WCHAR_T_DEFINED) \
    && !defined(_WCHAR_T_DEFINED_) && !defined(_WCHAR_T_DECLARED) \
    && !defined(_BSD_WCHAR_T_DEFINED_) && !defined(__DEFINED_wchar_t) \
    && !defined(__wchar_t_defined) && !defined(_WCHAR_T_H)
#define _WCHAR_T
#define _WCHAR_T_
#define _WCHAR_T_DEFINED
#define _WCHAR_T_DEFINED_
#define _WCHAR_T_DECLARED
#define _BSD_WCHAR_T_DEFINED_
#define __DEFINED_wchar_t
#define __wchar_t_defined
typedef __WCHAR_TYPE__ wchar_t;
#endif
#endif
#endif

/* `wint_t` belongs to `<wchar.h>`, but glibc asks this header for it the same way it asks
 * for the others, so the request is answered here rather than being a second header. */
#ifdef __need_wint_t
#undef __need_wint_t
#if !defined(_WINT_T) && !defined(_WINT_T_DEFINED) && !defined(_WINT_T_DECLARED) \
    && !defined(__DEFINED_wint_t) && !defined(__wint_t_defined)
#define _WINT_T
#define _WINT_T_DEFINED
#define _WINT_T_DECLARED
#define __DEFINED_wint_t
#define __wint_t_defined
typedef __WINT_TYPE__ wint_t;
#endif
#endif

#ifdef __need_NULL
#undef __need_NULL
#undef NULL
#if defined(__cplusplus)
#define NULL __null
#else
#define NULL ((void *) 0)
#endif
#endif

/* The alignment no standard type needs more of. A union of the widest of each family is the
 * definition every implementation reaches for, and it is written as a union rather than as
 * an `_Alignas` on a character so that its size is a multiple of its alignment. */
#ifdef __need_max_align_t
#undef __need_max_align_t
#if !defined(__RUCC_MAX_ALIGN_T) && !defined(_GCC_MAX_ALIGN_T) \
    && !defined(__CLANG_MAX_ALIGN_T_DEFINED) && !defined(__DEFINED_max_align_t)
#define __RUCC_MAX_ALIGN_T
#define _GCC_MAX_ALIGN_T
#define __CLANG_MAX_ALIGN_T_DEFINED
#define __DEFINED_max_align_t
typedef struct {
    long long __rucc_max_align_ll;
    long double __rucc_max_align_ld;
} max_align_t;
#endif
#endif

/* C23's `nullptr_t`, which is the type of `nullptr` and has exactly one value. */
#ifdef __need_nullptr_t
#undef __need_nullptr_t
#if !defined(__RUCC_NULLPTR_T) && !defined(_GCC_NULLPTR_T) && !defined(_NULLPTR_T_DECLARED)
#define __RUCC_NULLPTR_T
#define _GCC_NULLPTR_T
#define _NULLPTR_T_DECLARED
typedef __typeof__(nullptr) nullptr_t;
#endif
#endif

#ifdef __RUCC_STDDEF_WHOLE
#undef __RUCC_STDDEF_WHOLE
#ifndef _STDDEF_H
#define _STDDEF_H
#define _STDDEF_H_
#define __STDDEF_H
#define _ANSI_STDDEF_H

#define offsetof(type, member) __builtin_offsetof(type, member)

#endif
#endif
