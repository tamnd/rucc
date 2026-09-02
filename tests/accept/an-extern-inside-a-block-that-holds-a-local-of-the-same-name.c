/* accept: all */
/* C 6.2.2p4 gives `extern` the linkage of a visible prior declaration only where that
   declaration has a linkage of its own. The local in the block outside has none, so the inner
   declaration names the object at file scope and the two are not in contradiction. The same
   pair written in one block is, and gcc refuses that one in the same words this does. */

int v = 3;

int f(void)
{
  int v = 4;
  (void) v;
  {
    extern int v;
    return v;
  }
}

int main(void) { return f() - 3; }
