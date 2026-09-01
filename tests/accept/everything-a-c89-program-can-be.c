/* accept: all */
/* The dialects are meant to differ only where the language does. A program written in the
   common subset has to compile under every one of them, and this is that program. */

struct point {
  int x;
  int y;
};

union either {
  int as_int;
  char as_bytes[4];
};

enum colour { red, green, blue };

typedef struct point point;

static int total;

int distance(point a, point b) {
  int dx = a.x - b.x;
  int dy = a.y - b.y;
  return dx * dx + dy * dy;
}

int classify(enum colour c, union either e) {
  int result;
  switch (c) {
    case red:
      result = e.as_int;
      break;
    case green:
      result = e.as_bytes[0];
      break;
    default:
      result = 0;
      break;
  }
  total += result;
  return result;
}

int count(const char *s) {
  int n = 0;
  while (*s != 0) {
    n++;
    s++;
  }
  return n;
}
