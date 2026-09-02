/* reject: all */
/* message: continue statement not within a loop */
/* A `continue` needs a loop and not a switch, which is the one way it differs from `break`,
   and the wording says so. */

void f(void) {
  continue;
}
