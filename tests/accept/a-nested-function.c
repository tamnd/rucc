/* accept: all */
/* GNU's nested function, which reaches the variables of the function it is written in and is
   called by name or through its address. The design is in `spec/13-gnu-compat.md` section
   13.3. */

int f(int n) {
  int total = 0;
  int add(int k) { total += k * n; return total; }
  int (*p)(int) = add;
  add(1);
  return p(2);
}
