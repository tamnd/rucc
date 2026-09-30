/* stdckdint.h, checked integer arithmetic from C23.
 *
 * Each macro stores the mathematically correct result of the operation, wrapped to the type of
 * `*r`, and says whether it had to wrap. The overflow builtins already do exactly that for any
 * integer types, so the macros are those builtins with their arguments in the order the standard
 * writes them, and the answer converted to `bool` because the builtins answer with an int.
 *
 * glibc does not ship this header, but a C library that does is found after it, the way gcc's
 * own copy does it. */

#ifndef __RUCC_STDCKDINT_H
#define __RUCC_STDCKDINT_H

#define __STDC_VERSION_STDCKDINT_H__ 202311L

#define ckd_add(r, a, b) ((_Bool)__builtin_add_overflow((a), (b), (r)))
#define ckd_sub(r, a, b) ((_Bool)__builtin_sub_overflow((a), (b), (r)))
#define ckd_mul(r, a, b) ((_Bool)__builtin_mul_overflow((a), (b), (r)))

#if !defined(_LIBC_STDCKDINT_H) && __has_include_next(<stdckdint.h>)
#include_next <stdckdint.h>
#endif

#endif
