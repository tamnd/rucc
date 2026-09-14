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

// The address of a weak symbol is null when nothing defined it, which is a question the linker
// answers rather than one the folder can, so this branch stays.
extern int maybe_there __attribute__((weak));

int present(void) {
  if (&maybe_there) {
    return 1;
  }
  return 0;
}
