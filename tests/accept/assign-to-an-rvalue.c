/* reject: all */
/* message: lvalue required as left operand of assignment */
/* A constant names no object, so there is nowhere to put the value. */

void f(void) {
  1 = 2;
}
