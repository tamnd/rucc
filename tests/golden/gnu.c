// GNU's statement expression, which is a block in the middle of an expression whose value is
// what its last statement computed. The statements are walked where the expression is, and the
// last one is evaluated rather than discarded, which is the whole difference between one of
// these and an ordinary block.

int use(int);

struct pair {
  int a, b;
};

// The ordinary use, which is what every macro that needs a temporary is written with.
int a_temporary(int x) {
  return ({
    int t = use(x);
    t * t;
  });
}

// Control flow inside one, which joins before the value is taken.
int control_flow(int c) {
  return ({
    if (c) {
      use(1);
    }
    c ? 2 : 3;
  });
}

// A block with no value, which is `void` and is a statement with brackets round it.
void no_value(int x) {
  ({ use(x); });
  ({});
}

// The value is an object rather than a number, so what the last statement named is the object
// and not a copy of it.
int an_object(int c) {
  return ({
    struct pair p = { c, c + 1 };
    p;
  }).b;
}

// The declarations in one are its own, and an array whose length is not a constant is given
// back where the block ends and after its value has been taken out of it.
int a_scope_of_its_own(int n) {
  return ({
    int a[n];
    a[0] = n;
    a[0];
  });
}

// Control that never reaches the end of one, which is what a macro that always jumps expands
// to. The value is never taken and everything after it is unreachable.
int never_finishes(int x) {
  return ({
    return x;
    0;
  });
}
