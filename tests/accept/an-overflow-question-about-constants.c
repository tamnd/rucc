/* accept: all */
/* gcc folds `__builtin_add_overflow_p` and its two siblings when both operands are constants, and
   gnulib's intprops asserts about them at compile time. The third operand only names the type the
   answer has to fit in. */

typedef char add_over[__builtin_add_overflow_p(2147483647, 1, (int)0) ? 1 : -1];
typedef char add_fits[__builtin_add_overflow_p(2147483646, 1, (int)0) ? -1 : 1];
typedef char sub_under[__builtin_sub_overflow_p(0u, 1u, (unsigned)0) ? 1 : -1];
typedef char sub_wide[__builtin_sub_overflow_p(0u, 1u, (long)0) ? -1 : 1];
typedef char mul_char[__builtin_mul_overflow_p(16, 16, (signed char)0) ? 1 : -1];
typedef char mul_long[__builtin_mul_overflow_p(4294967296LL, 4294967296LL, (long long)0) ? 1 : -1];
typedef char neg_unsigned[__builtin_mul_overflow_p(-1, 1, (unsigned long)0) ? 1 : -1];

int sizes(void)
{
  return sizeof(add_over) + sizeof(add_fits) + sizeof(sub_under) + sizeof(sub_wide)
         + sizeof(mul_char) + sizeof(mul_long) + sizeof(neg_unsigned);
}
