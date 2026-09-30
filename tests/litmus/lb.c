/*
 * Load buffering, with a barrier between each thread's load and its store.
 *
 *     thread 0          thread 1
 *     r0 = x            r1 = y
 *     barrier           barrier
 *     y = 1             x = 1
 *
 * Both loads reading the other thread's store is forbidden, since each store would then have had
 * to become visible before the load in front of it was answered. AArch64 allows it without a
 * barrier, although few parts ever show it. Every barrier and ordered access here keeps an earlier
 * load in front of a later store: a full fence, the read barrier, whose `dmb ishld` orders loads
 * against everything after them, the write barrier, which is a full `dmb ish` under gcc, an
 * acquiring load, and a releasing store.
 *
 * The reason PostgreSQL needs this one is the reader of a structure that is then given back: the
 * reads of the fields must be done before the store that lets somebody else reuse it.
 */

#include "litmus.h"
#include "pg_atomics.h"

static volatile int x[LITMUS_ROUND], y[LITMUS_ROUND];
static int r0[LITMUS_ROUND], r1[LITMUS_ROUND];

enum {
	NONE,
	PG_MEMORY_BARRIER,
	PG_READ_BARRIER,
	PG_WRITE_BARRIER,
	ACQUIRE_LOAD,
	RELEASE_STORE,
	SYNC_FETCH_AND_ADD,
	S_UNLOCK,
};

static int variant;

static void reset(void)
{
	int held = variant == S_UNLOCK ? 0xff : 0;
	memset((void *)x, held, sizeof x);
	memset((void *)y, held, sizeof y);
	memset(r0, -1, sizeof r0);
	memset(r1, -1, sizeof r1);
}

static void body(int t, long i)
{
	volatile int *mine = t ? &x[i] : &y[i];
	volatile int *theirs = t ? &y[i] : &x[i];
	int *seen = t ? &r1[i] : &r0[i];

	switch (variant) {
	case NONE:
		*seen = *theirs;
		*mine = 1;
		break;
	case PG_MEMORY_BARRIER:
		*seen = *theirs;
		pg_memory_barrier();
		*mine = 1;
		break;
	case PG_READ_BARRIER:
		*seen = *theirs;
		pg_read_barrier();
		*mine = 1;
		break;
	case PG_WRITE_BARRIER:
		*seen = *theirs;
		pg_write_barrier();
		*mine = 1;
		break;
	case ACQUIRE_LOAD:
		*seen = __atomic_load_n(theirs, __ATOMIC_ACQUIRE);
		*mine = 1;
		break;
	case RELEASE_STORE:
		*seen = *theirs;
		__atomic_store_n(mine, 1, __ATOMIC_RELEASE);
		break;
	case SYNC_FETCH_AND_ADD:
		*seen = *theirs;
		__sync_fetch_and_add(mine, 1);
		break;
	case S_UNLOCK:
		/*
		 * Both start held, and giving one back is the store. It is a release, so what was
		 * read before it stays in front of it.
		 */
		*seen = *theirs == 0;
		S_UNLOCK(mine);
		break;
	}
}

static long both_one(void)
{
	long n = 0, i;
	for (i = 0; i < LITMUS_ROUND; i++)
		n += r0[i] == 1 && r1[i] == 1;
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
		{PG_MEMORY_BARRIER, "lb pg_memory_barrier"},
		{PG_READ_BARRIER, "lb pg_read_barrier"},
		{PG_WRITE_BARRIER, "lb pg_write_barrier"},
		{ACQUIRE_LOAD, "lb acquire load"},
		{RELEASE_STORE, "lb release store"},
		{SYNC_FETCH_AND_ADD, "lb __sync_fetch_and_add"},
		{S_UNLOCK, "lb S_UNLOCK"},
	};
	struct litmus test = {"lb no barrier", 2, reset, body, never, both_one};
	size_t c;

	litmus_setup();
	variant = NONE;
	litmus_run(&test);
	test.forbidden = both_one;
	test.weak = NULL;
	for (c = 0; c < sizeof cases / sizeof cases[0]; c++) {
		variant = cases[c].variant;
		test.name = cases[c].name;
		litmus_run(&test);
	}
	return litmus_failed;
}
