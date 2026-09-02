/* reject: all */
/* message: subscripted value is neither array nor pointer nor vector */
/* Nor is there anything to index. The vector in the wording is gcc's, since its vector
   extension takes a subscript as well. */

int f(void) {
  int x = 0;
  return x[0];
}
