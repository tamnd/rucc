// The GNU extensions the walk builds: the statement expression, which is a block in the middle
// of an expression whose value is what its last statement computed, `__builtin_va_arg`, which
// is one argument off a variable argument list, the address of a label with the jump to one
// that goes with it, inline assembly, and `__builtin_unreachable`.

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
// is the target's answer. A `va_list` parameter is an array of one on this target, so it has
// already been adjusted to the address of the list by the time it is read.
int an_argument(__builtin_va_list ap) { return __builtin_va_arg(ap, int); }

// Two of them on one list are two arguments, and each moves the list on.
double two_arguments(__builtin_va_list ap) {
  return __builtin_va_arg(ap, double) + (double)__builtin_va_arg(ap, int);
}

// The rest of the family, over a list of the function's own. The list is an object here rather
// than a parameter, so what the three of them are handed is its address, which is what it
// decays to on a target whose `va_list` is an array.
int the_whole_family(int n, ...) {
  __builtin_va_list ap, copy;
  __builtin_va_start(ap, n);
  __builtin_va_copy(copy, ap);
  int first = __builtin_va_arg(ap, int);
  int again = __builtin_va_arg(copy, int);
  __builtin_va_end(ap);
  __builtin_va_end(copy);
  return first + again;
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

// Assembly with no operands, which is implicitly `volatile` because there is no result to say
// it was needed and dropping it would drop the only thing it did.
void barrier(void) { __asm__("mfence" ::: "memory"); }

// One output and one input, numbered in the order they are written, so `%0` is the output and
// `%1` is the input whatever the constraints say.
int add_one(int x) {
  int r;
  __asm__("addl $1, %1" : "=r"(r) : "r"(x));
  return r;
}

// An operand that is read and written, which is one operand and not two, so it is an argument
// as well as a result and the object is written back where the assembly finishes.
int twice(int x) {
  __asm__("addl %0, %0" : "+r"(x));
  return x;
}

// A memory operand, which travels as the address of the object rather than as its value, so
// the object needs somewhere to live and the walk gives it a slot.
int in_memory(int x) {
  int slot = x;
  __asm__("incl %0" : "+m"(slot));
  return slot;
}

// Named operands, which are the same operands with the numbers written out for the reader, and
// are resolved back to numbers before the template reaches the assembler.
int named(int x) {
  int r;
  __asm__("movl %[in], %[out]" : [out] "=r"(r) : [in] "r"(x));
  return r;
}

// An `asm goto`, which is a terminator: control either falls through to the statement after it
// or arrives at one of the labels. The output is written where control falls through, so the
// edge to the label carries the value the object had before the assembly ran.
int jumps(int x) {
  int r = 7;
  __asm__ goto("cbnz %0, %l1" : "=r"(r) : "r"(x)::away);
  return r;
away:
  return r;
}

// `__FUNCTION__` and `__PRETTY_FUNCTION__`, which say in C what `__func__` says. They are three
// objects and not one, so the comparison below is false at compile time in gcc as well.
int three_names(void) {
  return __func__ == __FUNCTION__ || __func__ == __PRETTY_FUNCTION__;
}

// `__builtin_unreachable`, which is the program promising control does not get here. It is a
// node with nothing under it and it lowers to a hint no instruction is written for, so what
// this function is in the end is the `if` and the terminator the walk puts on a body that can
// run off the bottom. That terminator is here whether or not the promise is written down, which
// is why the promise costs nothing to honour.
int promised(int x) {
  if (x) {
    return 1;
  }
  __builtin_unreachable();
}

// The statement after one is still built. The promise being false is undefined behaviour and
// deleting what follows is an optimization rather than the meaning, so nothing here does it.
int promised_early(int x) {
  __builtin_unreachable();
  return x;
}
