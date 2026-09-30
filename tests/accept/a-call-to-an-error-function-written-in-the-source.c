/* accept: all */
/* A call to a function carrying `error` is reported once the optimizer is done with it and not
   where it is written, since the kernel calls one under a condition only inlining settles. So
   checking the source says nothing about it. */

extern void bad(void) __attribute__((error("bad")));
void f(int x) { if (x) bad(); }
