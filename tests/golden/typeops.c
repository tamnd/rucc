// The operators that ask a question about a type rather than compute with a value. All but
// `sizeof` a variable length array are answered here and never reach the IR.

struct point { int x; int y; };

typedef typeof(1 + 1) same_as_int;
typedef typeof_unqual(const int) plain_int;

unsigned long constant_sizes(void) {
  struct point p = { 0, 0 };
  return sizeof(int) + sizeof(struct point) + sizeof p + sizeof "hi"
       + _Alignof(double) + alignof(struct point);
}

unsigned long a_size_that_has_to_be_computed(int n) {
  int vla[n];
  return sizeof vla;
}

int chosen(void) {
  return _Generic(1.0, double: 1, float: 2, default: 0)
       + _Generic((char)1, char: 10, default: 0);
}

int casts(double d, struct point *p) {
  same_as_int a = (int)d;
  plain_int b = (short)a;
  void *erased = (void *)p;
  return a + b + (erased == 0);
}
