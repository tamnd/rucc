/* reject: all */
/* message: redeclaration of 'x' with no linkage */
/* Neither of these defines a value, so what is wrong is that a name with no linkage was
   declared twice rather than that an object was defined twice. */

void f(void) {
  int x;
  int x;
  (void)x;
}
