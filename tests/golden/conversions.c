// Every implicit conversion the language performs is a node of its own in the typed tree,
// so that nothing downstream has to know the conversion rules a second time.

int values[4];

int taking(int, long);

long promotions(char c, short s, unsigned char uc, _Bool b) {
  return c + s + uc + b;
}

double usual_arithmetic(int i, unsigned u, long l, float f, double d) {
  return i + u + l + f + d;
}

int decay(void) {
  int *p = values;
  int (*fn)(int, long) = taking;
  return p[0] + fn(1, 2);
}

int to_bool(int *p, double d) {
  if (p && d) {
    return !p;
  }
  return p ? 1 : 0;
}

const void *to_pointer(int *p) {
  return p;
}

void narrowing(char *out, int i) {
  *out = (char)i;
}
