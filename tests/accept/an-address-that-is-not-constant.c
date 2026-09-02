/* reject: all */
/* message: initializer element is not constant */
/* A static object is initialized before the program runs, and the address of an automatic
   one does not exist until the block is entered. */

int f(void) {
  int x = 0;
  static int *p = &x;
  return *p;
}
