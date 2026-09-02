/* reject: all */
/* message: case label not within a switch statement */
/* A case label is a destination and there is no switch here to jump from. */

void f(void) {
  case 1:;
}
