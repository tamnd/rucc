/* accept: c99 c11 c17 c23 gnu99 gnu11 gnu17 gnu23 */
/* reject: c89 gnu89 */
/* `restrict` is a C99 keyword and is not one in C89, in either dialect. `__restrict__` is the
   spelling that works everywhere, which is why headers use it. */

void copy(int *restrict to, const int *restrict from) {
  *to = *from;
}
