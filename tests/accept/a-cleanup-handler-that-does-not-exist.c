/* reject: all */
/* message: does not name anything */
/* The argument is a name rather than a string, so it is looked up where the declaration is and a
   name nothing declared is a call that could not be built. gcc refuses it in the same place. */

void f(void) {
  int held __attribute__((cleanup(nowhere))) = 0;
  (void)held;
}
