/* accept: all */
/* A tentative definition may name a structure that is not complete yet, 6.9.2p2, if the type
   is complete at the end of the unit. gcc takes it with no warning. FreeBSD's vfs_bio.c declares
   two objects of a bit set type a few lines before the type. The object has the size of the
   complete type. */

struct set lo;
static struct set hi;

struct set {
  long bits[3];
};

typedef char lo_has_the_size_of_the_type[sizeof lo == sizeof(struct set) ? 1 : -1];

int main(void) {
  lo.bits[2] = 1;
  hi.bits[0] = 2;
  return (int)(lo.bits[2] + hi.bits[0]) - 3;
}
