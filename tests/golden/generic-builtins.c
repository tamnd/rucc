// The builtins whose type comes from the call. Nothing declares these and no table holds a
// prototype for them, because there is no one prototype to hold: the same name answers with an
// int in one line and a long in the next, and what decides is the argument.
//
// There is no `.ir` beside this case. Every one of these is a builtin nothing lowers, and the
// walk refuses a call to one rather than writing down a call to a symbol no object file
// defines, so what this case pins is the typed tree it produces.

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
