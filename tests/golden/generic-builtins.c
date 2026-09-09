// The builtins whose type comes from the call. Nothing declares these and no table holds a
// prototype for them, because there is no one prototype to hold: the same name answers with an
// int in one line and a long in the next, and what decides is the argument.

int counter;
long total;
_Atomic int shared;

long widths(void) {
  int narrow = __atomic_load_n(&counter, 0);
  long wide = __atomic_load_n(&total, 0);
  int through_atomic = __atomic_load_n(&shared, 0);
  return narrow + wide + through_atomic;
}

int reads_and_writes(void) {
  int expected = 0;
  __atomic_store_n(&counter, 1, 0);
  __atomic_load(&counter, &expected, 0);
  int fetched = __atomic_fetch_add(&counter, 1, 0);
  int swapped = __atomic_compare_exchange_n(&counter, &expected, 7, 0, 0, 0);
  __atomic_thread_fence(5);
  return fetched + swapped;
}

// The read modify writes, whose two spellings per operation differ in whether they answer the
// value before or the value after, and whose object may be a pointer. gcc adds the operand to a
// pointer object as it stands rather than scaling it by the pointee, so this moves the cursor
// four bytes and not four elements.
int *cursor;

long updates(void) {
  int taken = __sync_lock_test_and_set(&counter, 1);
  int put = __atomic_exchange_n(&counter, 2, 5);
  int after = __atomic_sub_fetch(&counter, 1, 5);
  int *moved = __atomic_fetch_add(&cursor, 4, 5);
  return taken + put + after + (moved != 0);
}

// The bitwise four, which this machine has no single instruction for and which become a loop
// around the compare and exchange further down. Nothing about that shows here, since what a name
// asks for is the same question whatever the machine can do about it. The nand is the and with
// every bit of the answer flipped, which is what gcc has meant by the name since 4.4.
int bits(int v) {
  int held = __atomic_fetch_and(&counter, v, 5);
  int flipped = __atomic_nand_fetch(&counter, v, 5);
  int set = __sync_fetch_and_or(&counter, v);
  int toggled = __sync_xor_and_fetch(&counter, v);
  return held + flipped + set + toggled;
}

// The three that pass a value through a second pointer rather than taking or answering one, which
// is the shape the family has for an object too big to come back in a register.
void through_pointers(int *p, int *v, int *r) {
  __atomic_load(p, r, 5);
  __atomic_store(p, v, 5);
  __atomic_exchange(p, v, r, 5);
}

// The flag, whose object is one byte whatever the pointer points at, and whose set value is one
// the implementation picks rather than one the program hands over. So both of these carry a
// constant the source never wrote, and the pointer here is an int one to show the width does not
// come from it.
int flag(int *p) {
  int held = __atomic_test_and_set(p, 5);
  __atomic_clear(p, 5);
  return held;
}

int overflow(int a, long b) {
  long product;
  int wrapped = __builtin_mul_overflow(a, b, &product);
  return wrapped + (int)product;
}

// The one in this family that is answered rather than called. A parameter is not a constant
// the front end can see and a literal is, so the two calls are a zero and a one and neither of
// them reaches the tree as a call.
int known(int a) { return __builtin_constant_p(a) + __builtin_constant_p(7); }

// The older family, whose trailing arguments are the variables the barrier is promised to
// protect and so are read as the tail of a variadic call.
long older(void) {
  long fetched = __sync_fetch_and_add(&total, 2);
  int swapped = __sync_bool_compare_and_swap(&counter, 1, 2, &total);
  __sync_lock_release(&counter);
  return fetched + swapped;
}
