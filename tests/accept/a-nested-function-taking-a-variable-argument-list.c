/* reject: all */
/* message: a nested function cannot take a variable argument list */
/* The static chain goes after every argument, where a function reading `...` cannot find it. */

int f(void) {
  int g(int n, ...) { return n; }
  return g(1, 2);
}
