/* accept: all */
/* warns: dereferencing 'void *' pointer */
/* Any integer constant expression with the value zero is a null pointer constant once it is cast
   to `void *`, and not only a literal zero. The kernel's `__is_constexpr` is built on that: the
   conditional below has type `int *` when `x` is a constant and `void *` when it is not, and
   gcc's `sizeof (void)` of one is where the warning comes from. A `const` object is not a
   constant here in any dialect, which is gcc's answer too. */

#define is_constexpr(x) (sizeof(int) == sizeof(*(8 ? ((void *)((long)(x) * 0l)) : (int *)8)))

int variable;
const int constant = 3;
enum { four = 4 };

typedef char a_literal[is_constexpr(4) ? 1 : -1];
typedef char an_enumerator[is_constexpr(four + 1) ? 1 : -1];
typedef char a_size[is_constexpr(sizeof(long)) ? 1 : -1];
typedef char a_variable[is_constexpr(variable) ? -1 : 1];
typedef char a_const_object[is_constexpr(constant) ? -1 : 1];

int *through_a_cast = (void *)(1 - 1);
