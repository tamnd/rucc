/* reject: all */
/* message: initializer element is not constant */
/* gcc reads a member of a `const` union in a static initializer only as the member its
   initializer wrote. The other member is not read. */

union word { int i; float f; };

const union word word = { 6 };
float f = word.f;
