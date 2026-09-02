/* reject: all */
/* message: too many arguments to function 'g'; expected 2, have 3 */
/* The same message from the other side, which a variadic function would not get. */

int g(int a, int b);

int f(void) {
  return g(1, 2, 3);
}
