/* reject: all */
/* message: initializer element is not constant */
/* gcc reads a member of a `const` object in a static initializer only where the initializer of
   the object names it. A member it left at zero is not read. */

struct pair { int a; int b; };

const struct pair pair = { 1 };
int b = pair.b;
