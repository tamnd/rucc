// A condition that folds to a constant picks its arm in the front end, so the arm that cannot run
// leaves nothing behind for the linker to go looking for, at every level and not just the ones
// that asked for optimization.

void only_on_32_bit(void);

int width(void) {
  if (sizeof(void *) == 4) {
    only_on_32_bit();
    return 32;
  }
  return 64;
}

int never(void) {
  if (0) {
    only_on_32_bit();
  }
  if (1) {
    return 1;
  } else {
    only_on_32_bit();
  }
  return 0;
}

int floating(void) {
  if (0.0) {
    only_on_32_bit();
  }
  if (1.5) {
    return 1;
  }
  return 0;
}

// The label is inside an arm that cannot be reached from the top and a `goto` outside it does
// reach it, so the arm is still built from the label down and joins what follows the `if`.
int jumped_into(int n) {
  if (0) {
    only_on_32_bit();
  inside:
    n++;
  }
  if (n == 1) goto inside;
  return n;
}

// The folder assumes no object is at zero, and the symbol a program asks this about is the one
// that can be: a weak symbol is at zero when nothing defined it. So a condition that went looking
// for an address keeps its branch and the question is left to run time.
extern int maybe_there __attribute__((weak));

int present(void) {
  if (&maybe_there) {
    return 1;
  }
  return 0;
}

// The shape real code writes is not a constant at all. libtommath asks whether a platform routine
// was compiled in with a macro that is nought where it was not, and the variable on the other side
// of the `&&` cannot change that answer, so the left still runs and the arm goes away with the
// call in it.
int has_it(int err) {
  if (err && 0) {
    only_on_32_bit();
  }
  if (err || 1) {
    return 1;
  }
  only_on_32_bit();
  return 0;
}

// The side that decides can be the left one, and then the other side does not run at all.
int skipped(int (*f)(void)) {
  if (0 && f()) {
    only_on_32_bit();
  }
  return 0;
}

int negated(int err) {
  if (!(err && 0)) {
    return 1;
  }
  only_on_32_bit();
  return 0;
}
