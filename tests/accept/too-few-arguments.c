/* reject: all */
/* message: too few arguments to function 'g'; expected 2, have 1 */
/* The counts are in the message because the prototype is usually not on the screen. */

int g(int a, int b);

int f(void) {
  return g(1);
}
