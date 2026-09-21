/* accept: all */
/* An empty initializer is the only initializer C allows on an object whose length the program
   computes, and 6.7.11p11 says one with automatic storage duration holds what a static object
   would, which is zero in every element and in the padding. It is the only way the language
   offers to ask for a zeroed one of these. */

int f(int n) {
  char b[n] = {};
  struct S {
    int a[n];
    int t;
  } s = {};
  return b[0] + s.a[0] + s.t;
}
