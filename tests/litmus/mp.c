/*
 * Message passing, with PostgreSQL's write barrier on one side and its read barrier on the other.
 *
 *     thread 0          thread 1
 *     data = 1          r0 = flag
 *     write barrier     read barrier
 *     flag = 1          r1 = data
 *
 * Seeing the flag and then the old data is forbidden. Without the write barrier AArch64 may make
 * the two stores visible in the other order, and without the read barrier it may answer the second
 * load before the first. This is how PostgreSQL hands a filled in structure to another backend
 * through shared memory: the fields are plain stores, then pg_write_barrier(), then the store that
 * says it is ready, and the reader checks that, then pg_read_barrier(), then reads the fields.
 *
 * The same shape is run with the barrier spelled each other way the code base spells one, and
 * with an acquiring load and a releasing store in place of the two fences. The first run has no
 * barrier at all, where the forbidden outcome is allowed, and it is printed as a measure of how
 * hard the run tried.
 */

#include "litmus.h"
#include "pg_atomics.h"

static volatile int data32[LITMUS_ROUND], flag32[LITMUS_ROUND];
static volatile uint64 data64[LITMUS_ROUND];
static int r0[LITMUS_ROUND], r1[LITMUS_ROUND];
static slock_t unlocked[LITMUS_ROUND];

enum {
	NONE,
	PG_WRITE_READ_BARRIER,
	PG_WRITE_READ_BARRIER_64,
	PG_MEMORY_BARRIER,
	SYNC_SYNCHRONIZE,
	RELEASE_STORE_ACQUIRE_LOAD,
	RELEASE_FENCE_ACQUIRE_FENCE,
	S_UNLOCK_THEN_READ_BARRIER,
	SEQ_CST_STORE_LOAD,
	SYNC_FETCH_AND_ADD,
};

static int variant;

static void reset(void)
{
	memset((void *)data32, 0, sizeof data32);
	memset((void *)flag32, 0, sizeof flag32);
	memset((void *)data64, 0, sizeof data64);
	memset(r0, -1, sizeof r0);
	memset(r1, -1, sizeof r1);
	/* Held, so that giving it back is the flag going up. */
	memset((void *)unlocked, 0xff, sizeof unlocked);
}

static void writer(long i)
{
	switch (variant) {
	case NONE:
		data32[i] = 1;
		flag32[i] = 1;
		break;
	case PG_WRITE_READ_BARRIER:
		data32[i] = 1;
		pg_write_barrier();
		flag32[i] = 1;
		break;
	case PG_WRITE_READ_BARRIER_64:
		data64[i] = 0x100000001ull;
		pg_write_barrier();
		flag32[i] = 1;
		break;
	case PG_MEMORY_BARRIER:
		data32[i] = 1;
		pg_memory_barrier();
		flag32[i] = 1;
		break;
	case SYNC_SYNCHRONIZE:
		data32[i] = 1;
		__sync_synchronize();
		flag32[i] = 1;
		break;
	case RELEASE_STORE_ACQUIRE_LOAD:
		data32[i] = 1;
		__atomic_store_n(&flag32[i], 1, __ATOMIC_RELEASE);
		break;
	case RELEASE_FENCE_ACQUIRE_FENCE:
		data32[i] = 1;
		__atomic_thread_fence(__ATOMIC_RELEASE);
		flag32[i] = 1;
		break;
	case S_UNLOCK_THEN_READ_BARRIER:
		data32[i] = 1;
		S_UNLOCK(&unlocked[i]);
		break;
	case SEQ_CST_STORE_LOAD:
		data32[i] = 1;
		__atomic_store_n(&flag32[i], 1, __ATOMIC_SEQ_CST);
		break;
	case SYNC_FETCH_AND_ADD:
		data32[i] = 1;
		__sync_fetch_and_add(&flag32[i], 1);
		break;
	}
}

static void reader(long i)
{
	switch (variant) {
	case NONE:
		r0[i] = flag32[i];
		r1[i] = data32[i];
		break;
	case PG_WRITE_READ_BARRIER:
		r0[i] = flag32[i];
		pg_read_barrier();
		r1[i] = data32[i];
		break;
	case PG_WRITE_READ_BARRIER_64:
		r0[i] = flag32[i];
		pg_read_barrier();
		/* Both halves or neither: a torn read is as wrong as a stale one. */
		r1[i] = data64[i] == 0x100000001ull ? 1 : data64[i] == 0 ? 0 : 2;
		break;
	case PG_MEMORY_BARRIER:
		r0[i] = flag32[i];
		pg_memory_barrier();
		r1[i] = data32[i];
		break;
	case SYNC_SYNCHRONIZE:
		r0[i] = flag32[i];
		__sync_synchronize();
		r1[i] = data32[i];
		break;
	case RELEASE_STORE_ACQUIRE_LOAD:
		r0[i] = __atomic_load_n(&flag32[i], __ATOMIC_ACQUIRE);
		r1[i] = data32[i];
		break;
	case RELEASE_FENCE_ACQUIRE_FENCE:
		r0[i] = flag32[i];
		__atomic_thread_fence(__ATOMIC_ACQUIRE);
		r1[i] = data32[i];
		break;
	case S_UNLOCK_THEN_READ_BARRIER:
		r0[i] = unlocked[i] == 0;
		pg_read_barrier();
		r1[i] = data32[i];
		break;
	case SEQ_CST_STORE_LOAD:
		r0[i] = __atomic_load_n(&flag32[i], __ATOMIC_SEQ_CST);
		r1[i] = data32[i];
		break;
	case SYNC_FETCH_AND_ADD:
		r0[i] = (int)__sync_fetch_and_add(&flag32[i], 0);
		r1[i] = data32[i];
		break;
	}
}

static void body(int t, long i)
{
	if (t == 0)
		writer(i);
	else
		reader(i);
}

static long stale(void)
{
	long n = 0, i;
	for (i = 0; i < LITMUS_ROUND; i++)
		n += r0[i] == 1 && r1[i] != 1;
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
		{PG_WRITE_READ_BARRIER, "mp pg_write_barrier, pg_read_barrier"},
		{PG_WRITE_READ_BARRIER_64, "mp the same over 64 bits"},
		{PG_MEMORY_BARRIER, "mp pg_memory_barrier"},
		{SYNC_SYNCHRONIZE, "mp __sync_synchronize"},
		{RELEASE_STORE_ACQUIRE_LOAD, "mp release store, acquire load"},
		{RELEASE_FENCE_ACQUIRE_FENCE, "mp release fence, acquire fence"},
		{S_UNLOCK_THEN_READ_BARRIER, "mp S_UNLOCK, pg_read_barrier"},
		{SEQ_CST_STORE_LOAD, "mp seq_cst store, seq_cst load"},
		{SYNC_FETCH_AND_ADD, "mp __sync_fetch_and_add both sides"},
	};
	struct litmus test = {"mp no barrier", 2, reset, body, never, stale};
	size_t c;

	litmus_setup();
	variant = NONE;
	litmus_run(&test);
	test.forbidden = stale;
	test.weak = NULL;
	for (c = 0; c < sizeof cases / sizeof cases[0]; c++) {
		variant = cases[c].variant;
		test.name = cases[c].name;
		litmus_run(&test);
	}
	return litmus_failed;
}
