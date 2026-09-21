/* accept: all */
/* The address of a member reached through a null pointer, cast to an integer, which is how
   offsetof is spelled by everything that predates __builtin_offsetof and by every header that
   still has to work on a compiler without it. tcc's own tcc.h is one of those, and this is what
   its table of option names is built out of. There is no object under the address, so the whole
   of it is a number this compiler already knows, which is what makes it a constant and therefore
   an initializer for an object that exists before the program runs. gcc folds all of these. */

#define OFFSETOF(type, field) ((unsigned long) &((type *)0)->field)

struct S {
  int a;
  char b;
  long c;
  int d[4];
  struct {
    int x;
  } n;
};

typedef struct {
  unsigned short off;
  const char *name;
} Entry;

static const Entry table[] = {
  { OFFSETOF(struct S, a), "a" },   { OFFSETOF(struct S, b), "b" },
  { OFFSETOF(struct S, c), "c" },   { OFFSETOF(struct S, d[2]), "d2" },
  { OFFSETOF(struct S, n.x), "nx" }, { 0, 0 },
};

/* A narrow type too. There is no relocation here for half of it to be lost, which is the whole
   difference between this and the address of a real object, so gcc takes this and refuses
   `int n = (int)&some_object;`. */
static const int narrow = (int) OFFSETOF(struct S, c);

/* The distance between two of them, which is the other half of how a warning table is built:
   tcc writes `offsetof(TCCState, sw) - offsetof(TCCState, warn_none)`. */
static const unsigned long between = OFFSETOF(struct S, c) - OFFSETOF(struct S, b);

/* And the pointer itself, without the cast, which is an address constant whose value is known
   here rather than at the link. */
static long *const nowhere = &((struct S *)0)->c;

int main(void) {
  return (int) (table[2].off + (unsigned) narrow + (unsigned) between + (nowhere != 0));
}
