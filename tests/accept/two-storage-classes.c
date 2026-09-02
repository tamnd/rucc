/* reject: all */
/* message: multiple storage classes in declaration specifiers */
/* A declaration says where the object lives once. The message does not name either keyword,
   which is gcc's wording, and the span is on the second one, which is the one to delete. */

static extern int x;
