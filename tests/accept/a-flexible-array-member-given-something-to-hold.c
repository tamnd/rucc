/* accept: all */
/* A structure whose last member is a flexible array, defined at file scope with an initializer
   for the array. `sizeof` answers without the array and the object has to hold what was written,
   so the object is larger than its type. gcc gives these the same sizes. The image used to be
   written at the size the type had, and the verifier caught twenty bytes going into four. */

struct a { int i; int j[]; } x = { 1, { 2, 0, 2, 3 } };

struct b { char c; char p[]; } y = { 'o', "wx" };

struct c { char c; char p[]; } z = { '9', { 'e', 'b' } };

int main(void)
{
  return (x.j[3] != 3) + (y.p[1] != 'x') + (z.p[1] != 'b');
}
