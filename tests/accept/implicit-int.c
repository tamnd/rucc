/* accept: c89 gnu89 */
/* reject: c99 c11 c17 c23 gnu99 gnu11 gnu17 gnu23 */
/* A declaration with no type used to mean `int`. Removed in C99, and an error in gcc rather
   than a warning since gcc 14. Under C89 it is the language and gcc says nothing about it, which
   is why there is no `warns` line here. */

static counted;
