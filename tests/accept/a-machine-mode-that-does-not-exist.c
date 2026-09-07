/* reject: all */
/* message: unknown machine mode */
/* A mode names a shape the machine has, so a name that is not one of them cannot be guessed at:
   every guess is a width, and a width the program did not ask for is the whole bug this
   attribute exists to prevent. gcc refuses it in the same words. */

typedef unsigned int __attribute__((mode(ZZ))) bad;

bad f(bad a) { return a; }
