/* accept: all */
/* warns: 'error' attribute ignored */
/* Only a call can be reported, so gcc drops the attribute from an object with a warning rather
   than refusing the declaration. */

int x __attribute__((error("x")));
