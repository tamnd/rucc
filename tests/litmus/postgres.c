/*
 * Two of PostgreSQL's own patterns built on its atomics: the lightweight lock's state word, and
 * the latch's lost wakeup.
 *
 * An LWLock is one 32 bit word. Taking it in exclusive mode adds LW_VAL_EXCLUSIVE with a compare
 * and exchange when no one holds it, taking it in shared mode adds one when no one holds it
 * exclusively, and giving it back subtracts again with pg_atomic_sub_fetch_u32(). This is
 * LWLockAttemptLock() and the start of LWLockRelease() from src/backend/storage/lmgr/lwlock.c,
 * with the wait queue replaced by spinning, since the queue is not what is being tested. Two
 * threads take it exclusively and change a counter and a pair of words that are always equal, two
 * take it shared and check the pair is equal. The data is plain loads and stores, so the lock
 * itself has to keep them inside.
 *
 * The latch is SetLatch() against ResetLatch() and the check that follows it in a wait loop, from
 * src/backend/storage/ipc/latch.c and waiteventset.c. The waiter clears is_set, has a full barrier,
 * and looks for work. The setter makes work, has a full barrier, and looks at is_set to decide
 * whether the latch needs setting at all. If the setter saw is_set still true and the waiter saw
 * no work, the wakeup is lost and the waiter sleeps with work waiting. That is store buffering.
 * The setter's barrier is pg_memory_barrier() the way SetLatch() has it, and then, as elsewhere in
 * PostgreSQL where the change and the barrier are one call, pg_atomic_fetch_or_u32() and
 * pg_atomic_fetch_add_u32() on the work word, whose comments promise full barrier semantics.
 *
 * Portions Copyright (c) 1996-2025, PostgreSQL Global Development Group
 * Portions Copyright (c) 1994, The Regents of the University of California
 *
 * The PostgreSQL license notice is in pg_atomics.h.
 */

#include "litmus.h"
#include "pg_atomics.h"

/* lwlock.c, with MAX_BACKENDS at (1 << 18) - 1 as in PostgreSQL 18. */
#define MAX_BACKENDS ((1u << 18) - 1)
#define LW_VAL_EXCLUSIVE (MAX_BACKENDS + 1)
#define LW_VAL_SHARED 1
#define LW_LOCK_MASK (MAX_BACKENDS | LW_VAL_EXCLUSIVE)

typedef enum LWLockMode { LW_EXCLUSIVE, LW_SHARED } LWLockMode;

static struct {
	pg_atomic_uint32 state;
	char pad0[124];
	long counter;
	long left, right;
	char pad1[104];
	long torn[LITMUS_THREADS];
} lwlock __attribute__((aligned(128)));

/* Returns true when the lock is held by somebody else and the caller has to wait. */
static bool LWLockAttemptLock(LWLockMode mode)
{
	uint32 old_state = pg_atomic_read_u32(&lwlock.state);

	while (true) {
		uint32 desired_state = old_state;
		bool lock_free;

		if (mode == LW_EXCLUSIVE) {
			lock_free = (old_state & LW_LOCK_MASK) == 0;
			if (lock_free)
				desired_state += LW_VAL_EXCLUSIVE;
		} else {
			lock_free = (old_state & LW_VAL_EXCLUSIVE) == 0;
			if (lock_free)
				desired_state += LW_VAL_SHARED;
		}
		if (pg_atomic_compare_exchange_u32(&lwlock.state, &old_state, desired_state))
			return !lock_free;
	}
}

static void LWLockAcquire(LWLockMode mode)
{
	while (LWLockAttemptLock(mode))
		spin_delay();
}

static void LWLockRelease(LWLockMode mode)
{
	if (mode == LW_EXCLUSIVE)
		(void)pg_atomic_sub_fetch_u32(&lwlock.state, LW_VAL_EXCLUSIVE);
	else
		(void)pg_atomic_sub_fetch_u32(&lwlock.state, LW_VAL_SHARED);
}

static void lwlock_body(int thread, long each)
{
	long n;

	for (n = 0; n < each; n++) {
		if (thread < 2) {
			LWLockAcquire(LW_EXCLUSIVE);
			if (lwlock.left != lwlock.right)
				lwlock.torn[thread]++;
			lwlock.counter++;
			lwlock.left++;
			lwlock.right++;
			LWLockRelease(LW_EXCLUSIVE);
		} else {
			LWLockAcquire(LW_SHARED);
			if (lwlock.left != lwlock.right)
				lwlock.torn[thread]++;
			LWLockRelease(LW_SHARED);
		}
	}
}

static long lwlock_check(int threads, long each, char *why, size_t size)
{
	long want = 2 * each, torn = 0;
	uint32 state = pg_atomic_read_u32(&lwlock.state);
	int t;

	for (t = 0; t < threads; t++)
		torn += lwlock.torn[t];
	if (lwlock.counter == want && lwlock.left == want && lwlock.right == want && torn == 0 &&
	    state == 0)
		return 0;
	snprintf(why, size, "counter %ld, pair %ld and %ld, seen torn %ld times, state %#x, wanted %ld",
		 lwlock.counter, lwlock.left, lwlock.right, torn, (unsigned)state, want);
	return 1;
}

static pg_atomic_uint32 work[LITMUS_ROUND];
static volatile bool is_set[LITMUS_ROUND];
static int setter_saw_set[LITMUS_ROUND], waiter_saw_work[LITMUS_ROUND];

enum {
	NONE,
	PG_MEMORY_BARRIER,
	PG_ATOMIC_FETCH_OR_U32,
	PG_ATOMIC_FETCH_ADD_U32,
};

static int variant;

static void latch_reset(void)
{
	memset((void *)work, 0, sizeof work);
	memset((void *)is_set, 1, sizeof is_set);
	memset(setter_saw_set, -1, sizeof setter_saw_set);
	memset(waiter_saw_work, -1, sizeof waiter_saw_work);
}

static void latch_body(int t, long i)
{
	if (t == 0) {
		/* ResetLatch(), then the caller's check for work. */
		is_set[i] = false;
		if (variant != NONE)
			pg_memory_barrier();
		waiter_saw_work[i] = pg_atomic_read_u32(&work[i]) != 0;
		return;
	}
	/* Make work, then SetLatch(). */
	switch (variant) {
	case NONE:
		pg_atomic_write_u32(&work[i], 1);
		break;
	case PG_MEMORY_BARRIER:
		pg_atomic_write_u32(&work[i], 1);
		pg_memory_barrier();
		break;
	case PG_ATOMIC_FETCH_OR_U32:
		(void)pg_atomic_fetch_or_u32(&work[i], 1);
		break;
	case PG_ATOMIC_FETCH_ADD_U32:
		(void)pg_atomic_fetch_add_u32(&work[i], 1);
		break;
	}
	setter_saw_set[i] = is_set[i];
}

static long lost_wakeups(void)
{
	long n = 0, i;
	for (i = 0; i < LITMUS_ROUND; i++)
		n += setter_saw_set[i] == 1 && waiter_saw_work[i] == 0;
	return n;
}

static long never(void)
{
	return 0;
}

int main(void)
{
	static const struct {
		int variant;
		const char *name;
	} cases[] = {
		{PG_MEMORY_BARRIER, "latch pg_memory_barrier"},
		{PG_ATOMIC_FETCH_OR_U32, "latch pg_atomic_fetch_or_u32"},
		{PG_ATOMIC_FETCH_ADD_U32, "latch pg_atomic_fetch_add_u32"},
	};
	struct hammer lock = {"lwlock 2 exclusive, 2 shared", 4, 1000000, lwlock_body,
			      lwlock_check};
	struct litmus latch = {"latch no barrier", 2, latch_reset, latch_body, never, lost_wakeups};
	size_t c;

	litmus_setup();
	pg_atomic_init_u32(&lwlock.state, 0);
	hammer_run(&lock);

	variant = NONE;
	litmus_run(&latch);
	latch.forbidden = lost_wakeups;
	latch.weak = NULL;
	for (c = 0; c < sizeof cases / sizeof cases[0]; c++) {
		variant = cases[c].variant;
		latch.name = cases[c].name;
		litmus_run(&latch);
	}
	return litmus_failed;
}
