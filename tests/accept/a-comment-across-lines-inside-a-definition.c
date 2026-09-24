/* accept: all */
/* A block comment is one space, so a newline inside it does not end the definition it
   sits in. GCC reads F as `((x) + 1)`, and the newline used to cut the body short. */

#define F(x) \
  ((x) + \
/* a comment that runs
onto a second line */ \
  1)

#define G 1 /* and one
that is not continued */ + 2

typedef char f_adds_one[F(1) == 2 ? 1 : -1];
typedef char g_adds_two[G == 3 ? 1 : -1];
