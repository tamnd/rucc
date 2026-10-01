/* reject: all */
/* message: label 'out' used but not defined */
/* Without `__label__` the label belongs to the function that defines it, so the `goto` in the
   nested function names a label of its own, and that one is never defined. gcc says so in these
   words. */

int f(void) {
  void leave(void) { goto out; }
  leave();
  return 1;
out:
  return 0;
}
