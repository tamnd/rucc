/* reject: all */
/* message: 'weakref' attribute is not supported */
/* `weakref` makes a static name another spelling of a symbol that may be missing at link time.
   Dropped, the name is a static function with no definition, and a call to it is a call to
   nothing, so it is refused rather than dropped. */

static int maybe(void) __attribute__((weakref("real_maybe")));
int ask(void) { return maybe ? maybe() : 0; }
