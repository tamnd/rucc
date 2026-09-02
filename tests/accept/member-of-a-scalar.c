/* reject: all */
/* message: request for member 'a' in something not a structure or union */
/* There are no members on an `int`. */

int f(void) {
  int x = 0;
  return x.a;
}
