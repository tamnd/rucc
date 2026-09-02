/* accept: all */
/* The builtins whose type comes from the call. `__atomic_load_n` answers with an `int` here
   and with a `long` two lines down, so there is no one prototype to declare it as and each
   call works its own out from what it was handed. */

int counter;
long total;

int use(void)
{
  int old = __atomic_load_n(&counter, 0);
  long sum = __atomic_load_n(&total, 0);
  __atomic_store_n(&counter, old + 1, 0);
  int before = __atomic_fetch_add(&counter, 1, 0);
  int expected = old;
  int swapped = __atomic_compare_exchange_n(&counter, &expected, 7, 0, 0, 0);
  __atomic_thread_fence(5);

  long product;
  int overflowed = __builtin_mul_overflow(old, sum, &product);
  int known = __builtin_constant_p(old + 1);

  /* The older family, which takes the variables the barrier protects after the arguments it
     uses and so has to be read as variadic. */
  long fetched = __sync_fetch_and_add(&total, 2);
  int exchanged = __sync_bool_compare_and_swap(&counter, 1, 2, &total);
  __sync_lock_release(&counter);

  return old + (int)sum + before + swapped + (int)product + overflowed + known
         + (int)fetched + exchanged;
}
