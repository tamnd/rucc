/* accept: all */
/* gcc folds `i && 0` and `i * 0` to nought when `i` only reads, and `__builtin_constant_p`
   says one for them. With an increment in the way it says nought. tcc's test prints these. */

int check(int i)
{
  typedef char anded[__builtin_constant_p(i && 0) ? 1 : -1];
  typedef char ored[__builtin_constant_p(i || 1) ? 1 : -1];
  typedef char times[__builtin_constant_p(i * 0) ? 1 : -1];
  typedef char chosen[__builtin_constant_p(i && 0 ? i : 34) ? 1 : -1];
  typedef char moved[__builtin_constant_p(i++ && 0) ? -1 : 1];
  typedef char read[__builtin_constant_p(i + 1) ? -1 : 1];
  return sizeof(anded) + sizeof(ored) + sizeof(times) + sizeof(chosen) + sizeof(moved)
         + sizeof(read);
}
