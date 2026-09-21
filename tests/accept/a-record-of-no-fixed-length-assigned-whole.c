/* accept: all */
/* One whole record with a member the program measures copied onto another, which is a copy of a
   length the program works out rather than of a number. */

int f(int n) {
  struct S {
    int a[n];
    int t;
  } x, y;
  x.t = 1;
  y = x;
  return y.t;
}
