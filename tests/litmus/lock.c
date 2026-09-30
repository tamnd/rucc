/*
 * PostgreSQL's spinlock, and the two other locks made of the same pair of builtins, each
 * protecting data that is read and written with plain loads and stores.
 *
 * S_LOCK is __sync_lock_test_and_set, which gcc documents as an acquire barrier only, and S_UNLOCK
 * is __sync_lock_release, which is a release. Between them the critical section has to stay where
 * it is: none of its loads may be answered before the lock is taken, and none of its stores may
 * become visible after it is given back. The data is a counter and a pair of words that are always
 * written equal. A load that moved out of the section reads a counter somebody else is about to
 * change, and an increment is lost. A store that moved out lets the next holder see one word of the
 * pair updated and not the other.
 *
 * Every thread takes the lock a fixed number of times, so the counter has exactly one right value
 * at the end. The spin is the one PostgreSQL does: test the lock with a plain load first, and wait
 * with an `isb` between tries.
 */

#include "litmus.h"
#include "pg_atomics.h"

/* Each on a line of its own, so the lock and the data do not share one by accident. */
static struct {
	slock_t lock;
	char pad0[124];
	long counter;
	long left, right;
	long torn;
	char pad1[96];
} spin __attribute__((aligned(128)));

static struct {
	pg_atomic_flag flag;
	char pad0[124];
	long counter;
	long left, right;
	long torn;
} flagged __attribute__((aligned(128)));

static struct {
	unsigned char byte;
	char pad0[127];
	long counter;
	long left, right;
	long torn;
} cleared __attribute__((aligned(128)));

static void section(long *counter, long *left, long *right, long *torn)
{
	if (*left != *right)
		(*torn)++;
	(*counter)++;
	(*left)++;
	(*right)++;
}

static void spinlock_body(int thread, long each)
{
	long n;
	(void)thread;
	for (n = 0; n < each; n++) {
		S_LOCK(&spin.lock);
		section(&spin.counter, &spin.left, &spin.right, &spin.torn);
		S_UNLOCK(&spin.lock);
	}
}

static void flag_body(int thread, long each)
{
	long n;
	(void)thread;
	for (n = 0; n < each; n++) {
		while (!pg_atomic_test_set_flag(&flagged.flag)) {
			while (!pg_atomic_unlocked_test_flag(&flagged.flag))
				spin_delay();
		}
		section(&flagged.counter, &flagged.left, &flagged.right, &flagged.torn);
		pg_atomic_clear_flag(&flagged.flag);
	}
}

static void test_and_set_body(int thread, long each)
{
	long n;
	(void)thread;
	for (n = 0; n < each; n++) {
		while (__atomic_test_and_set(&cleared.byte, __ATOMIC_ACQUIRE))
			spin_delay();
		section(&cleared.counter, &cleared.left, &cleared.right, &cleared.torn);
		__atomic_clear(&cleared.byte, __ATOMIC_RELEASE);
	}
}

static long judged(long counter, long left, long right, long torn, long want, char *why,
		   size_t size)
{
	if (counter == want && left == want && right == want && torn == 0)
		return 0;
	snprintf(why, size, "counter %ld, pair %ld and %ld, seen torn %ld times, wanted %ld",
		 counter, left, right, torn, want);
	return 1;
}

static long spinlock_check(int threads, long each, char *why, size_t size)
{
	return judged(spin.counter, spin.left, spin.right, spin.torn, threads * each, why, size);
}

static long flag_check(int threads, long each, char *why, size_t size)
{
	return judged(flagged.counter, flagged.left, flagged.right, flagged.torn, threads * each,
		      why, size);
}

static long test_and_set_check(int threads, long each, char *why, size_t size)
{
	return judged(cleared.counter, cleared.left, cleared.right, cleared.torn, threads * each,
		      why, size);
}

int main(void)
{
	struct hammer tests[] = {
		{"lock S_LOCK, S_UNLOCK", 4, 1000000, spinlock_body, spinlock_check},
		{"lock pg_atomic_test_set_flag", 4, 1000000, flag_body, flag_check},
		{"lock __atomic_test_and_set", 4, 1000000, test_and_set_body, test_and_set_check},
	};
	size_t t;

	litmus_setup();
	S_INIT_LOCK(&spin.lock);
	pg_atomic_clear_flag(&flagged.flag);
	for (t = 0; t < sizeof tests / sizeof tests[0]; t++)
		hammer_run(&tests[t]);
	return litmus_failed;
}
