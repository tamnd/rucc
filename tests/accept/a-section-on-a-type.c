/* reject: all */
/* message: section attribute not allowed for 'word' */
/* A type is not something the linker places, so gcc refuses a section on a typedef. */

typedef int word __attribute__((section(".mine")));
