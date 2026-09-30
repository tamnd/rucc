/* accept: all */
/* warns: ignoring attribute 'section ("b")' because it conflicts with previous 'section ("a")' */
/* The first declaration to name a section is the one the rest of the file was read against, so
   gcc keeps it and warns about the second. */

extern int x __attribute__((section("a")));
__attribute__((section("b"))) int x = 1;
