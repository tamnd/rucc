/* reject: all */
/* message: wrong number of arguments specified for 'error' attribute */
/* The attribute takes one message, and gcc refuses a declaration that gives it none or two. */

void f(void) __attribute__((error("a", "b")));
