/* reject: all */
/* message: label 'out' is in the enclosing function, not this nested one */
/* A `goto` from a nested function to a label of the function around it, which GNU allows when
   the label is declared with `__label__` and this compiler does not do: it has to unwind the
   frames in between. The label is defined after the nested function, which is the case the
   check has to wait for. */

int f(void) {
  __label__ out;
  void leave(void) { goto out; }
  leave();
  return 1;
out:
  return 0;
}
