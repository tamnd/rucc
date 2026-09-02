/* reject: all */
/* message: called object 'x' is not a function or function pointer */
/* The message names what was called, which is what makes it useful when the call is deep in
   an expression. */

void f(void) {
  int x = 0;
  x();
}
