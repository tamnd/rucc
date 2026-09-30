/* accept: all */
/* C23 6.7.7.1 lets an attribute follow a function declarator's parameter list, where it
   appertains to the function type, and a definition may write one there as well as a
   declaration. `unsequenced` and `reproducible` are the two C23 brought in for that place, and
   both are promises a compiler is free to ignore. gcc 16 takes every line here, and answers
   202311 for both names. The dialects before C23 have neither the syntax nor the names, so for
   them the file is empty. */

#if __STDC_VERSION__ >= 202311L

#if __has_c_attribute(unsequenced) != 202311L || __has_c_attribute(reproducible) != 202311L
#error "the two function type attributes are answered with the wrong value"
#endif

static int square(int x) [[unsequenced]] { return x * x; }

int twice(int x) [[reproducible]];
int twice(int x) [[reproducible]] { return 2 * x; }

int (*pick)(int) [[unsequenced]];

int both(int x) [[reproducible]] [[unsequenced]] { return square(x) + twice(x); }

#else
typedef int not_empty;
#endif
