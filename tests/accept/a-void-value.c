/* reject: all */
/* message: void value not ignored as it ought to be */
/* A call to a function returning void is a statement and not a value, so there is nothing
   for the initializer to read. */

void g(void);

int f(void) {
  int x = g();
  return x;
}
