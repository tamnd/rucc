/* iso646.h, the eleven spellings for the punctuation.
 *
 * They exist because a keyboard that cannot type a brace cannot type an ampersand either.
 * C23 says the header is obsolescent and every one of these still has to work. */

#ifndef __RUCC_ISO646_H
#define __RUCC_ISO646_H

#define and &&
#define and_eq &=
#define bitand &
#define bitor |
#define compl ~
#define not !
#define not_eq !=
#define or ||
#define or_eq |=
#define xor ^
#define xor_eq ^=

#endif
