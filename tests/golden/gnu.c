// The GNU extensions the walk builds: the statement expression, which is a block in the middle
// of an expression whose value is what its last statement computed, `__builtin_va_arg`, which
// is one argument off a variable argument list, and the address of a label with the jump to one
// that goes with it.

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

// One argument off a variable argument list, which stays an intrinsic because what it becomes
// is the target's answer. The list is a pointer here rather than a `va_list`, since `va_list`
// is a typedef of `__builtin_va_list` and there are no builtin declarations yet.
int an_argument(void *ap) { return __builtin_va_arg(ap, int); }

// Two of them on one list are two arguments, and each moves the list on.
double two_arguments(void *ap) {
  return __builtin_va_arg(ap, double) + (double)__builtin_va_arg(ap, int);
}

// The address of a label, and a jump to an address, which is what a threaded interpreter
// dispatches with. Where the jump arrives is not known here, so every label the function takes
// the address of is one of its targets and the values live across it are passed on every edge.
int dispatch(int n) {
  void *table[2];
  table[0] = &&odd;
  table[1] = &&even;
  int total = 0;
  goto *table[n & 1];
odd:
  total = n;
  goto *table[1];
even:
  return total + 1;
}
