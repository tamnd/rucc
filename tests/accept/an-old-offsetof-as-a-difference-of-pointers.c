/* accept: all */
/* Before offsetof was in every header, some code wrote it as the difference of two pointers.
   FreeBSD's ng_ksocket.c still does, in a static initializer. Both pointers fold to numbers, so
   gcc folds the difference to the offset of the member and takes it as a constant. */

#define OFFSETOF(s, e) ((char *)&((s *)0)->e - (char *)((s *)0))

struct sockopt {
  int level;
  int name;
  char value[8];
};

static const int offset = OFFSETOF(struct sockopt, value);
static const int name = OFFSETOF(struct sockopt, name);

int main(void) { return offset - 8 + name - 4; }
