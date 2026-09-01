/* accept: c89 gnu89 */
/* reject: c99 c11 c17 c23 gnu99 gnu11 gnu17 gnu23 */
/* gap: #84 c89 gnu89 */
/* A declaration with no type used to mean `int`. Removed in C99, and an error in gcc rather
   than a warning since gcc 14. */

static counted;
