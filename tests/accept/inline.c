/* accept: c99 c11 c17 c23 gnu */
/* reject: c89 */
/* The one that goes the other way from `restrict`: `inline` is a C99 keyword and gcc has it in
   gnu89 as an extension, so only strict C89 is without it. */

inline int twice(int a) {
  return a + a;
}
