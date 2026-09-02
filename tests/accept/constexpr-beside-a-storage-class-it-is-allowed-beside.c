/* accept: c23 gnu23 */
/* reject: c89 c99 c11 c17 gnu89 gnu99 gnu11 gnu17 */
/* C23 6.7.1 says at most one storage class specifier, and then names the pairs that are the
   exception. `constexpr` is in all three of them: it may be written with `auto`, with
   `register` and with `static`, in either order, and gcc takes every one of these. Before C23
   the keyword is an ordinary identifier and every line here is two declarators with no type. */

static constexpr int a = 1;
constexpr static int b = 2;

int f(void)
{
  register constexpr int c = 3;
  constexpr auto d = 4;
  return a + b + c + (int) d;
}
