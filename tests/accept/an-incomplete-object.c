/* reject: all */
/* message: storage size of 'object' isn't known */
/* A struct that was declared and never defined has no size, so nothing can be an object of
   it, only a pointer to one. */

struct s;

struct s object;
