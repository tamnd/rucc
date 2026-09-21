/* accept: all */
/* `__const__` is `const`, the way `__volatile__` is `volatile` and `__signed__` is `signed`. All
   of these live in the reserved namespace, so gcc has them on in every dialect including
   `-std=c89`, and a header that has to compile under any `-std=` is written with them rather than
   with the plain words. tcc's own test file uses this one on a function it wants folded, and a
   compiler without the spelling does not merely miss the qualifier: it reads the word as the name
   of an object and then reads the type behind it as an old style definition's parameters, so the
   error comes out at the end of some later function. */

static __const__ unsigned int swab32(unsigned int x)
{
  return ((x & 0xffu) << 24) | ((x & 0xff00u) << 8) | ((x >> 8) & 0xff00u) | (x >> 24);
}

/* On an object, where it is the qualifier and nothing else. */
static __const__ int table[4] = { 1, 2, 3, 4 };
__const__ char *const name = "a";

/* On a parameter, through a pointer, and in a cast, which are the three places a header puts it. */
static int first(__const__ int *p)
{
  return *p;
}

/* Beside the other two spellings of the same word, since a file may use whichever it likes. */
static const int a = 1;
static __const int b = 2;
static __const__ int c = 3;

/* And beside the rest of the family, all of which have both endings. */
static __volatile__ int spun;
static __signed__ int negative = -1;
static __inline__ int one(void) { return 1; }

int main(void)
{
  __const__ int *p = &table[2];
  return (int) (swab32(0x01020304u) != 0x04030201u) + first(p) - 3 + a + b + c - 6 + one() - 1
         + (int) negative + 1 + spun + (name[0] != 'a');
}
