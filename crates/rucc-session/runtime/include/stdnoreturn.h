/* stdnoreturn.h, one macro.
 *
 * C23 deprecated it in favour of the `[[noreturn]]` attribute, and defining the macro under
 * C23 would break a program that writes the attribute, since `noreturn` inside the brackets
 * would expand. So the macro is not defined there. */

#ifndef __RUCC_STDNORETURN_H
#define __RUCC_STDNORETURN_H

#if !defined(__STDC_VERSION__) || __STDC_VERSION__ < 202311L
#define noreturn _Noreturn
#endif

#endif
