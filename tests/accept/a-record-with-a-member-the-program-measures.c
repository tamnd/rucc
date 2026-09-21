/* accept: all */
/* A member whose length the program computes makes the record as a whole one whose size is
   worked out where the declaration is reached. gcc has taken this since it took variable length
   arrays, and only a function may declare one. */

int f(int n) {
  struct S {
    char c;
    int a[n];
    int b;
  } s;
  s.c = 1;
  s.b = 2;
  return (int)sizeof s + s.b;
}
