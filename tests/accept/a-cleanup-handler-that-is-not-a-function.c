/* reject: all */
/* message: is not a function */
/* An object is not something a call can go to, and a pointer to a function written here is not
   either: what is named is the handler itself and not something holding it. */

int handler;

void f(void) {
  int held __attribute__((cleanup(handler))) = 0;
  (void)held;
}
