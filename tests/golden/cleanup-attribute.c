// `__attribute__((cleanup(f)))` on a local asks for `f(&object)` on every way out of the block the
// object was declared in, which is the closing brace and every `return`, `break`, `continue` and
// `goto` that leaves it. Two objects in one block are given back in the reverse of the order they
// were declared, so a program that takes a lock and then takes what the lock protects lets go of
// them the other way round.

void release(void **held);
int decide(void);
void note(int n);

// The closing brace, and the reverse order: `second` goes first.
void at_the_end(void) {
  void *first __attribute__((cleanup(release))) = 0;
  void *second __attribute__((cleanup(release))) = 0;
  note(1);
}

// A `return` leaves every block the function is inside at once, and the value is taken out of the
// object before the handler is given it.
int on_return(void) {
  void *held __attribute__((cleanup(release))) = 0;
  if (decide()) {
    return 1;
  }
  return 0;
}

// An inner block gives its object back where it ends, and the outer one keeps going with its own.
void nested(void) {
  void *outer __attribute__((cleanup(release))) = 0;
  {
    void *inner __attribute__((cleanup(release))) = 0;
    note(2);
  }
  note(3);
}

// A `break` and a `continue` both leave the body of the loop, which is the block the object is
// declared in, so the handler runs once for every time round it.
void in_a_loop(void) {
  while (decide()) {
    void *each __attribute__((cleanup(release))) = 0;
    if (decide()) {
      continue;
    }
    if (decide()) {
      break;
    }
    note(4);
  }
}

// A `goto` out of the block runs what the block owes on the way, which is the shape every library
// that writes one error label at the bottom of a function is built on. The armoured spelling is
// the one a header writes, for the reason every spelling in a header is armoured.
int jumped_out(void) {
  void *held __attribute__((__cleanup__(release))) = 0;
  {
    void *inner __attribute__((cleanup(release))) = 0;
    if (decide()) {
      goto done;
    }
    note(5);
  }
done:
  return 0;
}
