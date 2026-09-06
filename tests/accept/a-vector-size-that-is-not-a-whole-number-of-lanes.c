/* reject: all */
/* message: vector size not an integral multiple of component size */
/* GNU's `vector_size` is given a size in bytes and the lanes have to fit in it exactly. Three
   bytes of `int` is not two lanes and not one, and gcc turns it down rather than rounding it,
   which is the right answer: a program that asked for a size the type cannot have got the
   arithmetic wrong somewhere above. */

typedef int __attribute__((vector_size(3))) bad;

bad f(bad a) { return a; }
