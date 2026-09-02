/* accept: all */
/* A function declared `static` and defined with nothing in front of it, which is how a great
   deal of real code is written and which this compiler used to call a contradiction. C 6.2.2p5
   gives a function declared with no storage class the linkage `extern` would have given it, and
   `extern` takes what the declaration before it had, so the definition is a second declaration
   of the same static function. The same pair written on an object is a contradiction, and it is
   in `a-non-static-object-after-a-static-one.c` next door. */

static int f(void);

int f(void) { return 1; }

int main(void) { return f() - 1; }
