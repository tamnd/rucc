/* accept: all */
/* warns: 'warning' attribute ignored */
/* gcc drops the attribute with a warning when what it was given is not a string, and the
   function is declared as if it had not been written. */

void f(void) __attribute__((warning(3)));
