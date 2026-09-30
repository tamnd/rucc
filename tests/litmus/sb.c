/*
 * Store buffering, with each barrier PostgreSQL uses as a full one.
 *
 *     thread 0          thread 1
 *     x = 1             y = 1
 *     full barrier      full barrier
 *     r0 = y            r1 = x
 *
 * Without the barriers both threads may read zero, because each store can still be sitting in its
 * own core's store buffer when the other core's load is answered, and AArch64 lets a load pass an
 * earlier store to a different address. A full barrier forbids it. That is the shape of every
 * place PostgreSQL publishes a flag and then checks one the other side publishes, such as SetLatch
 * and WaitLatch, which is the lost wakeup.
 *
 * The barrier is written each way PostgreSQL writes one. pg_memory_barrier() and
 * __sync_synchronize() are fences. The __sync read modify writes are documented by gcc as full
 * barriers, and PostgreSQL's pg_atomic_fetch_add_u32() and its neighbours promise the same to
 * their callers, so each of them is used here as the store and the barrier in one, followed by a
 * plain load. The C11 ones promise less: a sequentially consistent operation is only ordered
 * against other sequentially consistent operations, so their load is a sequentially consistent
 * one as well.
 *
 * The first test has no barrier at all. Reading zero twice is allowed there, and how often it
 * happens is printed to show the harness gets the two stores and the two loads close enough
 * together for a missing barrier to be seen.
 */

#include "litmus.h"
#include "pg_atomics.h"

static volatile int x32[LITMUS_ROUND], y32[LITMUS_ROUND];
static volatile uint64 x64[LITMUS_ROUND], y64[LITMUS_ROUND];
static int r0[LITMUS_ROUND], r1[LITMUS_ROUND];

enum {
	NONE,
	PG_MEMORY_BARRIER,
	SYNC_SYNCHRONIZE,
	SYNC_FETCH_AND_ADD_32,
	SYNC_FETCH_AND_ADD_64,
	SYNC_FETCH_AND_SUB_32,
	SYNC_FETCH_AND_OR_32,
	SYNC_FETCH_AND_OR_64,
	SYNC_FETCH_AND_AND_32,
	SYNC_ADD_AND_FETCH_32,
	SYNC_VAL_COMPARE_AND_SWAP_32,
	SYNC_VAL_COMPARE_AND_SWAP_64,
	SYNC_BOOL_COMPARE_AND_SWAP_32,
	PG_ATOMIC_FETCH_ADD_U32,
	PG_ATOMIC_FETCH_OR_U64,
	SEQ_CST_STORE_LOAD_32,
	SEQ_CST_STORE_LOAD_64,
	SEQ_CST_EXCHANGE_LOAD_32,
	SEQ_CST_COMPARE_EXCHANGE_LOAD_32,
	SEQ_CST_FETCH_ADD_LOAD_64,
};

static int variant;

static void reset(void)
{
	memset((void *)x32, 0, sizeof x32);
	memset((void *)y32, 0, sizeof y32);
	memset((void *)x64, 0, sizeof x64);
	memset((void *)y64, 0, sizeof y64);
	memset(r0, -1, sizeof r0);
	memset(r1, -1, sizeof r1);
}

static void body(int t, long i)
{
	volatile int *mine = t ? &y32[i] : &x32[i];
	volatile int *theirs = t ? &x32[i] : &y32[i];
	volatile uint64 *mine64 = t ? &y64[i] : &x64[i];
	volatile uint64 *theirs64 = t ? &x64[i] : &y64[i];
	int *seen = t ? &r1[i] : &r0[i];
	int expected;

	switch (variant) {
	case NONE:
		*mine = 1;
		*seen = *theirs;
		break;
	case PG_MEMORY_BARRIER:
		*mine = 1;
		pg_memory_barrier();
		*seen = *theirs;
		break;
	case SYNC_SYNCHRONIZE:
		*mine = 1;
		__sync_synchronize();
		*seen = *theirs;
		break;
	case SYNC_FETCH_AND_ADD_32:
		__sync_fetch_and_add(mine, 1);
		*seen = *theirs;
		break;
	case SYNC_FETCH_AND_ADD_64:
		__sync_fetch_and_add(mine64, 1);
		*seen = (int)*theirs64;
		break;
	case SYNC_FETCH_AND_SUB_32:
		__sync_fetch_and_sub(mine, -1);
		*seen = *theirs;
		break;
	case SYNC_FETCH_AND_OR_32:
		__sync_fetch_and_or(mine, 1);
		*seen = *theirs;
		break;
	case SYNC_FETCH_AND_OR_64:
		__sync_fetch_and_or(mine64, 1);
		*seen = (int)*theirs64;
		break;
	case SYNC_FETCH_AND_AND_32:
		/* An and needs a bit to keep, so the location gets two first. The and is the barrier. */
		*mine = 3;
		__sync_fetch_and_and(mine, 1);
		*seen = *theirs & 1;
		break;
	case SYNC_ADD_AND_FETCH_32:
		__sync_add_and_fetch(mine, 1);
		*seen = *theirs;
		break;
	case SYNC_VAL_COMPARE_AND_SWAP_32:
		__sync_val_compare_and_swap(mine, 0, 1);
		*seen = *theirs;
		break;
	case SYNC_VAL_COMPARE_AND_SWAP_64:
		__sync_val_compare_and_swap(mine64, 0, 1);
		*seen = (int)*theirs64;
		break;
	case SYNC_BOOL_COMPARE_AND_SWAP_32:
		__sync_bool_compare_and_swap(mine, 0, 1);
		*seen = *theirs;
		break;
	case PG_ATOMIC_FETCH_ADD_U32:
		pg_atomic_fetch_add_u32((volatile pg_atomic_uint32 *)mine, 1);
		*seen = (int)pg_atomic_read_u32((volatile pg_atomic_uint32 *)theirs);
		break;
	case PG_ATOMIC_FETCH_OR_U64:
		pg_atomic_fetch_or_u64((volatile pg_atomic_uint64 *)mine64, 1);
		*seen = (int)pg_atomic_read_u64((volatile pg_atomic_uint64 *)theirs64);
		break;
	case SEQ_CST_STORE_LOAD_32:
		__atomic_store_n(mine, 1, __ATOMIC_SEQ_CST);
		*seen = __atomic_load_n(theirs, __ATOMIC_SEQ_CST);
		break;
	case SEQ_CST_STORE_LOAD_64:
		__atomic_store_n(mine64, 1, __ATOMIC_SEQ_CST);
		*seen = (int)__atomic_load_n(theirs64, __ATOMIC_SEQ_CST);
		break;
	case SEQ_CST_EXCHANGE_LOAD_32:
		__atomic_exchange_n(mine, 1, __ATOMIC_SEQ_CST);
		*seen = __atomic_load_n(theirs, __ATOMIC_SEQ_CST);
		break;
	case SEQ_CST_COMPARE_EXCHANGE_LOAD_32:
		expected = 0;
		__atomic_compare_exchange_n(mine, &expected, 1, 0, __ATOMIC_SEQ_CST,
					    __ATOMIC_SEQ_CST);
		*seen = __atomic_load_n(theirs, __ATOMIC_SEQ_CST);
		break;
	case SEQ_CST_FETCH_ADD_LOAD_64:
		__atomic_fetch_add(mine64, 1, __ATOMIC_SEQ_CST);
		*seen = (int)__atomic_load_n(theirs64, __ATOMIC_SEQ_CST);
		break;
	}
}

static long both_zero(void)
{
	long n = 0, i;
	for (i = 0; i < LITMUS_ROUND; i++)
		n += r0[i] == 0 && r1[i] == 0;
	return n;
}

static long forbidden(void)
{
	return both_zero();
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
		{PG_MEMORY_BARRIER, "sb pg_memory_barrier"},
		{SYNC_SYNCHRONIZE, "sb __sync_synchronize"},
		{SYNC_FETCH_AND_ADD_32, "sb __sync_fetch_and_add 32"},
		{SYNC_FETCH_AND_ADD_64, "sb __sync_fetch_and_add 64"},
		{SYNC_FETCH_AND_SUB_32, "sb __sync_fetch_and_sub 32"},
		{SYNC_FETCH_AND_OR_32, "sb __sync_fetch_and_or 32"},
		{SYNC_FETCH_AND_OR_64, "sb __sync_fetch_and_or 64"},
		{SYNC_FETCH_AND_AND_32, "sb __sync_fetch_and_and 32"},
		{SYNC_ADD_AND_FETCH_32, "sb __sync_add_and_fetch 32"},
		{SYNC_VAL_COMPARE_AND_SWAP_32, "sb __sync_val_compare_and_swap 32"},
		{SYNC_VAL_COMPARE_AND_SWAP_64, "sb __sync_val_compare_and_swap 64"},
		{SYNC_BOOL_COMPARE_AND_SWAP_32, "sb __sync_bool_compare_and_swap 32"},
		{PG_ATOMIC_FETCH_ADD_U32, "sb pg_atomic_fetch_add_u32"},
		{PG_ATOMIC_FETCH_OR_U64, "sb pg_atomic_fetch_or_u64"},
		{SEQ_CST_STORE_LOAD_32, "sb seq_cst store, load 32"},
		{SEQ_CST_STORE_LOAD_64, "sb seq_cst store, load 64"},
		{SEQ_CST_EXCHANGE_LOAD_32, "sb seq_cst exchange, load 32"},
		{SEQ_CST_COMPARE_EXCHANGE_LOAD_32, "sb seq_cst compare_exchange, load"},
		{SEQ_CST_FETCH_ADD_LOAD_64, "sb seq_cst fetch_add, load 64"},
	};
	struct litmus test = {"sb no barrier", 2, reset, body, never, both_zero};
	size_t c;

	litmus_setup();
	variant = NONE;
	litmus_run(&test);
	test.forbidden = forbidden;
	test.weak = NULL;
	for (c = 0; c < sizeof cases / sizeof cases[0]; c++) {
		variant = cases[c].variant;
		test.name = cases[c].name;
		litmus_run(&test);
	}
	return litmus_failed;
}
