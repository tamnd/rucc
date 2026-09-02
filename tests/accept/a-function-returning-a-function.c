/* reject: all */
/* message: 'g' declared as function returning a function */
/* A function can return a pointer to a function and cannot return a function, which is only
   reachable through a typedef because the declarator grammar has no way to write it. */

typedef int fn(void);

fn g(void);
