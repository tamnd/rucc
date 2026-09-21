/* reject: all */
/* message: storage size of 'sb' isn't constant */
/* The record is one a function may declare, and an object of it still has to be one the frame
   makes room for: a static object is laid out once, before anything has read `k`. */

int k;

int f(void) {
  static struct B {
    int a[k];
  } sb;
  return (int)sizeof sb;
}
