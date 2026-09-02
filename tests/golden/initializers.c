// An initializer list is flattened to offsets, whatever shape the braces and designators
// came in, because that is what a static initializer and a store both need.

struct point { int x; int y; };
union either { int as_int; char as_bytes[4]; };

int scalar = 1 + 2 * 3;
int array[4] = { 1, 2, [3] = 4 };
int partly[4] = { 1 };
struct point origin = { .y = 5 };
struct point ordered = { 1, 2 };
struct point nested[2] = { { 1, 2 }, [1].x = 3 };
int square[2][2] = { { 1, 2 }, { 3, 4 } };
int flattened[2][2] = { 1, 2, 3, 4 };
union either overlapping = { .as_int = 7 };

// A designator names a place, and the places may be named in any order. Naming one of them
// twice is legal as well, and the last of the two is the one that stands.
struct point backwards = { .y = 2, .x = 1 };
struct point twice = { .x = 1, .y = 2, .x = 3 };
int sparse[4] = { [3] = 4, [1] = 2 };
int resumed[4] = { [2] = 3, 4 };

char counted[] = "hi";
char exact[3] = "hi";
const char *pointed_at = "hi";

double widened = 1;

int automatic(int a) {
  struct point local = { a, a + 1 };
  int list[2] = { a, a };
  struct point compound = (struct point){ 1, 2 };
  return local.x + list[0] + compound.y;
}
