/* accept: all */
/* C says both arms of a conditional are void or neither is, and gcc says the whole thing is
   void the moment either arm is, warning only under -Wpedantic. The permissive rule is the one
   real code is written against: a statement expression that ends in a goto has type void, and
   an arm like that is how the dead code tests write a branch that must not be generated. */

extern int puts(const char *);

int f(int c)
{
	c ? puts("yes") : (void)0;
	c ? (void)0 : puts("no");
	return c;
}
