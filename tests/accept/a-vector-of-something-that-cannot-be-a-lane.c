/* reject: all */
/* message: invalid vector type for attribute */
/* A lane here is one of the arithmetic types. `bool` is not one, because a vector of them is a
   mask and is a different thing from a vector of one-byte integers, and gcc refuses that one
   too. A pointer is not one either, which is where this is narrower than gcc: gcc builds a
   vector of pointers and this compiler does not have one yet. */

typedef char *pointer;
typedef pointer __attribute__((vector_size(16))) bad;

bad f(bad a) { return a; }
