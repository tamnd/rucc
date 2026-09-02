/* stdarg.h, the variable argument list.
 *
 * A list is whatever the target says it is, which `__builtin_va_list` names, and the four
 * operators are the four the compiler already has. There is nothing here that a library
 * could provide, which is why this header is the compiler's own on every implementation.
 *
 * glibc includes this header with `__need___va_list` set, from `<stdio.h>` and from anything
 * else that declares a `vprintf`, because it wants the type without the four names in the
 * user's way. That request is answered and nothing else is. */

#ifndef __RUCC_VA_LIST_TYPE
#define __RUCC_VA_LIST_TYPE
#ifndef __GNUC_VA_LIST
#define __GNUC_VA_LIST
typedef __builtin_va_list __gnuc_va_list;
#endif
#endif

#ifdef __need___va_list
#undef __need___va_list
#else

#ifndef __RUCC_STDARG_H
#define __RUCC_STDARG_H

/* The names other headers set when they have already made the typedef. `_VA_LIST_DEFINED`
 * is glibc's, `_VA_LIST` and `__va_list__` are the BSD and Darwin spellings, and
 * `__DEFINED_va_list` is musl's, whose `<bits/alltypes.h>` would otherwise write a second
 * typedef of a type that is already this one. */
#if !defined(_VA_LIST) && !defined(_VA_LIST_DEFINED) && !defined(_VA_LIST_T_H) \
    && !defined(__va_list__) && !defined(__DEFINED_va_list)
#define _VA_LIST
#define _VA_LIST_DEFINED
#define _VA_LIST_T_H
#define __va_list__
#define __DEFINED_va_list
typedef __gnuc_va_list va_list;
#endif

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 202311L
/* C23 made the second argument optional and stopped evaluating it. The builtin took it as
 * an unevaluated operand long before that, so there is nothing to pass on. */
#define va_start(ap, ...) __builtin_va_start(ap, 0)
#else
#define va_start(ap, last) __builtin_va_start(ap, last)
#endif

#define va_arg(ap, type) __builtin_va_arg(ap, type)
#define va_end(ap) __builtin_va_end(ap)

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 199901L
#define va_copy(dst, src) __builtin_va_copy(dst, src)
#endif

/* The GNU spelling, which is available in every dialect because a program that asks for
 * `-std=c89` and calls `__va_copy` is asking for the extension by name. */
#define __va_copy(dst, src) __builtin_va_copy(dst, src)

#endif /* __RUCC_STDARG_H */
#endif /* __need___va_list */
