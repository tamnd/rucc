/* accept: all */
/* GNU's vector, which is several lanes of one arithmetic type that every operator works on at
   once. The attribute is taken in every dialect, the same as gcc takes it, because a header
   that declares one is compiled under whatever the project asked for. */

typedef int __attribute__((vector_size(16))) v4si;

int f(int n) {
  v4si a = { 1, 2, 3, 4 };
  v4si b = a * a + n;
  b -= a;
  return (-b)[0] + (~b)[1];
}
