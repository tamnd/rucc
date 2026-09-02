/* reject: all */
/* message: redefinition of 'x' */
/* Two declarations in a block that both give a value are a redefinition. The other pairing
   is next door in `a-redeclaration-with-no-linkage.c`, and the two are worth keeping apart
   because gcc says something different about each. */

void f(void) {
  int x = 1;
  int x = 2;
  (void)x;
}
