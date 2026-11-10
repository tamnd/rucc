/* reject: all */
/* message: initializer element is not constant */
/* gcc reads a part of an object in a static initializer only where the whole object is `const`.
   A `const` member of an object that is not is not read. */

struct fixed { const int n; int m; };

struct fixed fixed = { 3, 4 };
int n = fixed.n;
