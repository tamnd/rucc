/* accept: all */
/* `weakref` makes a static name another spelling of a symbol that may be missing at link time.
   Every reference through it is weak, so the test below reads as false when nothing defines
   `real_maybe`, and the older spelling with the target given to `alias` is the same thing. */

static int maybe(void) __attribute__((weakref("real_maybe")));
static int other(void) __attribute__((weakref, alias("real_other")));
int ask(void) { return (maybe ? maybe() : 0) + (other ? other() : 0); }
