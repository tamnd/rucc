/*
 * Counters that several threads change at once through one read modify write each, at 32 and 64
 * bits, and bit masks where every thread owns one bit.
 *
 * Each of these is a single location, so no ordering between locations is involved, only
 * atomicity: a compare and exchange that let another write in between its load and its store, or
 * an exclusive store that did not fail when it should have, loses an increment, and the count at
 * the end is short. The compare and exchange loops are the ones PostgreSQL writes around
 * pg_atomic_compare_exchange_u32() and _u64(), where a failure leaves the value it found in the
 * expected one and the loop goes round with it. The fetch and add and the others are the __sync
 * builtins PostgreSQL's pg_atomic_fetch_add_u32() and its neighbours are made of, and the C11 ones
 * beside them.
 *
 * In the masks every thread sets its own bit and clears it again. Nobody else touches that bit, so
 * the value each fetch answers must show it clear before the set and set before the clear, while
 * the other threads are changing the rest of the word under it.
 */

#include "litmus.h"
#include "pg_atomics.h"

#define THREADS 4
#define EACH 1000000

static pg_atomic_uint32 u32 __attribute__((aligned(128)));
static pg_atomic_uint64 u64 __attribute__((aligned(128)));
static volatile unsigned short u16 __attribute__((aligned(128)));
static volatile unsigned char u8 __attribute__((aligned(128)));
static long wrong_bits[THREADS];
static uint64 taken[THREADS];

enum {
	CAS_32,
	CAS_64,
	SYNC_VAL_CAS_32,
	SYNC_VAL_CAS_64,
	SYNC_BOOL_CAS_32,
	SYNC_BOOL_CAS_64,
	SYNC_FETCH_AND_ADD_32,
	SYNC_FETCH_AND_ADD_64,
	SYNC_FETCH_AND_ADD_16,
	SYNC_FETCH_AND_ADD_8,
	SYNC_FETCH_AND_SUB_32,
	SYNC_SUB_AND_FETCH_64,
	ATOMIC_FETCH_ADD_32,
	ATOMIC_FETCH_ADD_64,
	ATOMIC_ADD_FETCH_RELAXED_64,
	PG_ATOMIC_ADD_FETCH_U32,
	BITS_32,
	BITS_64,
	EXCHANGE_64,
};

static int variant;

static void body(int thread, long each)
{
	long n;
	uint32 old32;
	uint64 old64, bit64;
	uint32 bit32 = 1u << thread;

	bit64 = (uint64)1 << (32 + thread * 7);
	for (n = 0; n < each; n++) {
		switch (variant) {
		case CAS_32:
			old32 = pg_atomic_read_u32(&u32);
			while (!pg_atomic_compare_exchange_u32(&u32, &old32, old32 + 1))
				;
			break;
		case CAS_64:
			old64 = pg_atomic_read_u64(&u64);
			while (!pg_atomic_compare_exchange_u64(&u64, &old64, old64 + 1))
				;
			break;
		case SYNC_VAL_CAS_32:
			old32 = u32.value;
			for (;;) {
				uint32 seen = __sync_val_compare_and_swap(&u32.value, old32, old32 + 1);
				if (seen == old32)
					break;
				old32 = seen;
			}
			break;
		case SYNC_VAL_CAS_64:
			old64 = u64.value;
			for (;;) {
				uint64 seen = __sync_val_compare_and_swap(&u64.value, old64, old64 + 1);
				if (seen == old64)
					break;
				old64 = seen;
			}
			break;
		case SYNC_BOOL_CAS_32:
			do
				old32 = u32.value;
			while (!__sync_bool_compare_and_swap(&u32.value, old32, old32 + 1));
			break;
		case SYNC_BOOL_CAS_64:
			do
				old64 = u64.value;
			while (!__sync_bool_compare_and_swap(&u64.value, old64, old64 + 1));
			break;
		case SYNC_FETCH_AND_ADD_32:
			__sync_fetch_and_add(&u32.value, 1);
			break;
		case SYNC_FETCH_AND_ADD_64:
			__sync_fetch_and_add(&u64.value, (uint64)1 << 33 | 1);
			break;
		case SYNC_FETCH_AND_ADD_16:
			__sync_fetch_and_add(&u16, 1);
			break;
		case SYNC_FETCH_AND_ADD_8:
			__sync_fetch_and_add(&u8, 1);
			break;
		case SYNC_FETCH_AND_SUB_32:
			pg_atomic_fetch_sub_u32(&u32, 1);
			break;
		case SYNC_SUB_AND_FETCH_64:
			__sync_sub_and_fetch(&u64.value, 3);
			break;
		case ATOMIC_FETCH_ADD_32:
			__atomic_fetch_add(&u32.value, 1, __ATOMIC_SEQ_CST);
			break;
		case ATOMIC_FETCH_ADD_64:
			__atomic_fetch_add(&u64.value, 1, __ATOMIC_SEQ_CST);
			break;
		case ATOMIC_ADD_FETCH_RELAXED_64:
			__atomic_add_fetch(&u64.value, 1, __ATOMIC_RELAXED);
			break;
		case PG_ATOMIC_ADD_FETCH_U32:
			pg_atomic_add_fetch_u32(&u32, 2);
			break;
		case BITS_32:
			old32 = pg_atomic_fetch_or_u32(&u32, bit32);
			if (old32 & bit32)
				wrong_bits[thread]++;
			old32 = pg_atomic_fetch_and_u32(&u32, ~bit32);
			if (!(old32 & bit32))
				wrong_bits[thread]++;
			break;
		case BITS_64:
			old64 = pg_atomic_fetch_or_u64(&u64, bit64);
			if (old64 & bit64)
				wrong_bits[thread]++;
			old64 = pg_atomic_fetch_and_u64(&u64, ~bit64);
			if (!(old64 & bit64))
				wrong_bits[thread]++;
			break;
		case EXCHANGE_64:
			/* Every value put in comes out exactly once, here or at the end. */
			taken[thread] += pg_atomic_exchange_u64(&u64, (uint64)thread << 40 | (uint64)n);
			break;
		}
	}
}

static long check(int threads, long each, char *why, size_t size)
{
	uint64 total = (uint64)threads * (uint64)each;
	uint64 got = 0, want = 0;
	long bits = 0;
	int t;
	long n;

	switch (variant) {
	case CAS_32:
	case SYNC_VAL_CAS_32:
	case SYNC_BOOL_CAS_32:
	case SYNC_FETCH_AND_ADD_32:
	case ATOMIC_FETCH_ADD_32:
		got = u32.value;
		want = (uint32)total;
		break;
	case PG_ATOMIC_ADD_FETCH_U32:
		got = u32.value;
		want = (uint32)(2 * total);
		break;
	case SYNC_FETCH_AND_SUB_32:
		got = u32.value;
		want = (uint32)(0x80000000u - total);
		break;
	case CAS_64:
	case SYNC_VAL_CAS_64:
	case SYNC_BOOL_CAS_64:
	case ATOMIC_FETCH_ADD_64:
	case ATOMIC_ADD_FETCH_RELAXED_64:
		got = u64.value;
		want = total;
		break;
	case SYNC_FETCH_AND_ADD_64:
		got = u64.value;
		want = total * ((uint64)1 << 33 | 1);
		break;
	case SYNC_SUB_AND_FETCH_64:
		got = u64.value;
		want = 0 - 3 * total;
		break;
	case SYNC_FETCH_AND_ADD_16:
		got = u16;
		want = (unsigned short)total;
		break;
	case SYNC_FETCH_AND_ADD_8:
		got = u8;
		want = (unsigned char)total;
		break;
	case BITS_32:
	case BITS_64:
		for (t = 0; t < threads; t++)
			bits += wrong_bits[t];
		got = variant == BITS_32 ? u32.value : u64.value;
		if (bits || got) {
			snprintf(why, size, "a thread saw its own bit wrong %ld times, word left %#llx",
				 bits, (unsigned long long)got);
			return 1;
		}
		return 0;
	case EXCHANGE_64:
		got = u64.value;
		for (t = 0; t < threads; t++) {
			got += taken[t];
			for (n = 0; n < each; n++)
				want += (uint64)t << 40 | (uint64)n;
		}
		break;
	}
	if (got == want)
		return 0;
	snprintf(why, size, "ended at %#llx, wanted %#llx", (unsigned long long)got,
		 (unsigned long long)want);
	return 1;
}

int main(void)
{
	static const struct {
		int variant;
		const char *name;
	} cases[] = {
		{CAS_32, "counter pg_atomic_compare_exchange_u32"},
		{CAS_64, "counter pg_atomic_compare_exchange_u64"},
		{SYNC_VAL_CAS_32, "counter __sync_val_compare_and_swap 32"},
		{SYNC_VAL_CAS_64, "counter __sync_val_compare_and_swap 64"},
		{SYNC_BOOL_CAS_32, "counter __sync_bool_compare_and_swap 32"},
		{SYNC_BOOL_CAS_64, "counter __sync_bool_compare_and_swap 64"},
		{SYNC_FETCH_AND_ADD_32, "counter __sync_fetch_and_add 32"},
		{SYNC_FETCH_AND_ADD_64, "counter __sync_fetch_and_add 64"},
		{SYNC_FETCH_AND_ADD_16, "counter __sync_fetch_and_add 16"},
		{SYNC_FETCH_AND_ADD_8, "counter __sync_fetch_and_add 8"},
		{SYNC_FETCH_AND_SUB_32, "counter pg_atomic_fetch_sub_u32"},
		{SYNC_SUB_AND_FETCH_64, "counter __sync_sub_and_fetch 64"},
		{ATOMIC_FETCH_ADD_32, "counter __atomic_fetch_add seq_cst 32"},
		{ATOMIC_FETCH_ADD_64, "counter __atomic_fetch_add seq_cst 64"},
		{ATOMIC_ADD_FETCH_RELAXED_64, "counter __atomic_add_fetch relaxed 64"},
		{PG_ATOMIC_ADD_FETCH_U32, "counter pg_atomic_add_fetch_u32"},
		{BITS_32, "bits pg_atomic_fetch_or/and_u32"},
		{BITS_64, "bits pg_atomic_fetch_or/and_u64"},
		{EXCHANGE_64, "exchange pg_atomic_exchange_u64"},
	};
	struct hammer test = {"", THREADS, EACH, body, check};
	size_t c;

	litmus_setup();
	for (c = 0; c < sizeof cases / sizeof cases[0]; c++) {
		variant = cases[c].variant;
		pg_atomic_init_u32(&u32, variant == SYNC_FETCH_AND_SUB_32 ? 0x80000000u : 0);
		pg_atomic_init_u64(&u64, 0);
		u16 = 0;
		u8 = 0;
		memset(wrong_bits, 0, sizeof wrong_bits);
		memset(taken, 0, sizeof taken);
		test.name = cases[c].name;
		hammer_run(&test);
	}
	return litmus_failed;
}
