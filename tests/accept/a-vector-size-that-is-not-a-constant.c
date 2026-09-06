/* reject: all */
/* message: is not an integer constant */
/* The size is part of the type, so it is fixed where it is written and cannot be worked out
   while the program runs. A variable length array is the one thing C has that is sized like
   that, and a vector is not one. */

int n;

typedef int __attribute__((vector_size(n * 4))) bad;

bad f(bad a) { return a; }
