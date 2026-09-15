/* reject: all */
/* message: takes one argument */
/* The handler is called with the address of the object and with nothing else, so one parameter
   that is a pointer is the only shape there is anything to call. */

void handler(int held, int again);

void f(void) {
  int held __attribute__((cleanup(handler))) = 0;
  (void)held;
}
