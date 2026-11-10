/* accept: gnu */
/* reject: iso */
/* message: initializer element is not constant */
/* gcc reads a member or an element of a `const` object as the value its initializer put there,
   in the initializer of an object with static storage, as it reads a whole `const` object. It
   reads a scalar of any type there, and an automatic object in the same block too. libgomp fills
   `gomp_global_icv` from `gomp_default_icv_values` this way. The strict dialects keep 6.6 as it
   is written. */

struct icv { int threads; unsigned dynamic : 1; double wait; const char *name; int m[2][2]; };
union word { int i; float f; };

const struct icv icv = { 4, 1, 0.5, "dyn", { { 1, 2 }, { 3, 4 } } };
const int counts[3] = { 7, 8, 9 };
const char *const names[] = { "zero", "one" };
const union word word = { 6 };

int threads = icv.threads;
unsigned dynamic = icv.dynamic;
double wait = icv.wait;
const char *name = icv.name;
int corner = icv.m[1][0];
int second = counts[1];
const char *one = names[1];
int six = word.i;

int counted(void)
{
  const struct icv local = { 5 };
  static int s = local.threads;
  return s;
}
