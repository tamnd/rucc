/* stdbool.h, three macros and a claim.
 *
 * C23 made `bool`, `true` and `false` keywords and this header a formality, and it says the
 * header is obsolescent. It is still here because the amount of C that includes it is not
 * going down, and because a program that includes it and then writes `#ifdef bool` is
 * entitled to an answer. */

#ifndef __RUCC_STDBOOL_H
#define __RUCC_STDBOOL_H

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 202311L
/* The three names are keywords. Defining the macros anyway would be legal, since each
 * expands to itself, but `#undef bool` in a later header would then take the keyword away. */
#define __bool_true_false_are_defined 1
#else
#define bool _Bool
#define true 1
#define false 0
#define __bool_true_false_are_defined 1
#endif

#endif
