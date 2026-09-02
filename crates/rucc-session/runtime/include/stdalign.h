/* stdalign.h, the two alignment names.
 *
 * C11 spelled the operators with an underscore and put the plain names here. C23 made the
 * plain names keywords and this header a formality, which is the same course `<stdbool.h>`
 * took and for the same reason. */

#ifndef __RUCC_STDALIGN_H
#define __RUCC_STDALIGN_H

#if !defined(__STDC_VERSION__) || __STDC_VERSION__ < 202311L
#define alignas _Alignas
#define alignof _Alignof
#endif

#define __alignas_is_defined 1
#define __alignof_is_defined 1

#endif
