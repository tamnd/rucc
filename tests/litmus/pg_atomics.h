/*
 * PostgreSQL's barriers, spinlock and atomics as they are built with gcc on AArch64 Linux, cut
 * down to what the litmus tests use.
 *
 * Adapted from src/include/port/atomics.h, src/include/port/atomics/generic-gcc.h,
 * src/include/port/atomics/generic.h and src/include/storage/s_lock.h in PostgreSQL 18. The
 * builtin each operation reduces to is the one upstream picks when configure has found
 * HAVE_GCC__ATOMIC_INT32_CAS, HAVE_GCC__ATOMIC_INT64_CAS, HAVE_GCC__SYNC_INT32_CAS,
 * HAVE_GCC__SYNC_INT64_CAS and HAVE_GCC__SYNC_INT32_TAS, which gcc gives it on that platform. The
 * layers of _impl names and the fallbacks for other compilers are left out, so each function here
 * is the body upstream's reaches in the end.
 *
 * Portions Copyright (c) 1996-2025, PostgreSQL Global Development Group
 * Portions Copyright (c) 1994, The Regents of the University of California
 *
 * Permission to use, copy, modify, and distribute this software and its documentation for any
 * purpose, without fee, and without a written agreement is hereby granted, provided that the above
 * copyright notice and this paragraph and the following two paragraphs appear in all copies.
 *
 * IN NO EVENT SHALL THE UNIVERSITY OF CALIFORNIA BE LIABLE TO ANY PARTY FOR DIRECT, INDIRECT,
 * SPECIAL, INCIDENTAL, OR CONSEQUENTIAL DAMAGES, INCLUDING LOST PROFITS, ARISING OUT OF THE USE OF
 * THIS SOFTWARE AND ITS DOCUMENTATION, EVEN IF THE UNIVERSITY OF CALIFORNIA HAS BEEN ADVISED OF THE
 * POSSIBILITY OF SUCH DAMAGE.
 *
 * THE UNIVERSITY OF CALIFORNIA SPECIFICALLY DISCLAIMS ANY WARRANTIES, INCLUDING, BUT NOT LIMITED
 * TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE. THE SOFTWARE
 * PROVIDED HEREUNDER IS ON AN "AS IS" BASIS, AND THE UNIVERSITY OF CALIFORNIA HAS NO OBLIGATIONS TO
 * PROVIDE MAINTENANCE, SUPPORT, UPDATES, ENHANCEMENTS, OR MODIFICATIONS.
 */

#ifndef PG_ATOMICS_H
#define PG_ATOMICS_H

#include <stdbool.h>
#include <stdint.h>

typedef uint32_t uint32;
typedef int32_t int32;
typedef uint64_t uint64;
typedef int64_t int64;

/* generic-gcc.h */
#define pg_compiler_barrier() __asm__ __volatile__("" ::: "memory")
#define pg_memory_barrier() __atomic_thread_fence(__ATOMIC_SEQ_CST)
#define pg_read_barrier()                                                                          \
	do {                                                                                       \
		pg_compiler_barrier();                                                             \
		__atomic_thread_fence(__ATOMIC_ACQUIRE);                                           \
	} while (0)
#define pg_write_barrier()                                                                         \
	do {                                                                                       \
		pg_compiler_barrier();                                                             \
		__atomic_thread_fence(__ATOMIC_RELEASE);                                           \
	} while (0)

/* s_lock.h, the __arm__ || __aarch64__ section under HAVE_GCC__SYNC_INT32_TAS. */
typedef int slock_t;

static __inline__ int tas(volatile slock_t *lock)
{
	return __sync_lock_test_and_set(lock, 1);
}

#define TAS(lock) tas(lock)
#define TAS_SPIN(lock) (*(lock) ? 1 : TAS(lock))
#define S_UNLOCK(lock) __sync_lock_release(lock)
#define S_INIT_LOCK(lock) S_UNLOCK(lock)

static __inline__ void spin_delay(void)
{
#if defined(__aarch64__)
	__asm__ __volatile__(" isb;				\n");
#endif
}

/* s_lock() in s_lock.c without the stuck lock timeout, which is what S_LOCK falls into. */
static inline void s_lock(volatile slock_t *lock)
{
	while (TAS_SPIN(lock))
		spin_delay();
}

#define S_LOCK(lock) (TAS(lock) ? (s_lock(lock), 0) : 0)

/* generic-gcc.h: the flag. */
typedef struct pg_atomic_flag {
	volatile int value;
} pg_atomic_flag;

static inline bool pg_atomic_test_set_flag(volatile pg_atomic_flag *ptr)
{
	/* NB: only an acquire barrier, not a full one */
	/* some platform only support a 1 here */
	return __sync_lock_test_and_set(&ptr->value, 1) == 0;
}

static inline bool pg_atomic_unlocked_test_flag(volatile pg_atomic_flag *ptr)
{
	return ptr->value == 0;
}

static inline void pg_atomic_clear_flag(volatile pg_atomic_flag *ptr)
{
	__sync_lock_release(&ptr->value);
}

/* generic-gcc.h and generic.h: 32 bits. */
typedef struct pg_atomic_uint32 {
	volatile uint32 value;
} pg_atomic_uint32;

static inline void pg_atomic_init_u32(volatile pg_atomic_uint32 *ptr, uint32 val)
{
	ptr->value = val;
}

static inline uint32 pg_atomic_read_u32(volatile pg_atomic_uint32 *ptr)
{
	return ptr->value;
}

static inline void pg_atomic_write_u32(volatile pg_atomic_uint32 *ptr, uint32 val)
{
	ptr->value = val;
}

static inline bool pg_atomic_compare_exchange_u32(volatile pg_atomic_uint32 *ptr,
						  uint32 *expected, uint32 newval)
{
	/* FIXME: we can probably use a lower consistency model */
	return __atomic_compare_exchange_n(&ptr->value, expected, newval, false, __ATOMIC_SEQ_CST,
					   __ATOMIC_SEQ_CST);
}

static inline uint32 pg_atomic_exchange_u32(volatile pg_atomic_uint32 *ptr, uint32 newval)
{
	return __atomic_exchange_n(&ptr->value, newval, __ATOMIC_SEQ_CST);
}

static inline uint32 pg_atomic_fetch_add_u32(volatile pg_atomic_uint32 *ptr, int32 add_)
{
	return __sync_fetch_and_add(&ptr->value, add_);
}

static inline uint32 pg_atomic_fetch_sub_u32(volatile pg_atomic_uint32 *ptr, int32 sub_)
{
	return __sync_fetch_and_sub(&ptr->value, sub_);
}

static inline uint32 pg_atomic_fetch_and_u32(volatile pg_atomic_uint32 *ptr, uint32 and_)
{
	return __sync_fetch_and_and(&ptr->value, and_);
}

static inline uint32 pg_atomic_fetch_or_u32(volatile pg_atomic_uint32 *ptr, uint32 or_)
{
	return __sync_fetch_and_or(&ptr->value, or_);
}

static inline uint32 pg_atomic_add_fetch_u32(volatile pg_atomic_uint32 *ptr, int32 add_)
{
	return pg_atomic_fetch_add_u32(ptr, add_) + add_;
}

static inline uint32 pg_atomic_sub_fetch_u32(volatile pg_atomic_uint32 *ptr, int32 sub_)
{
	return pg_atomic_fetch_sub_u32(ptr, sub_) - sub_;
}

static inline uint32 pg_atomic_read_membarrier_u32(volatile pg_atomic_uint32 *ptr)
{
	return pg_atomic_fetch_add_u32(ptr, 0);
}

static inline void pg_atomic_write_membarrier_u32(volatile pg_atomic_uint32 *ptr, uint32 val)
{
	(void)pg_atomic_exchange_u32(ptr, val);
}

/* generic-gcc.h and generic.h: 64 bits, with PG_HAVE_8BYTE_SINGLE_COPY_ATOMICITY from arch-arm.h. */
typedef struct pg_atomic_uint64 {
	volatile uint64 value __attribute__((aligned(8)));
} pg_atomic_uint64;

static inline void pg_atomic_init_u64(volatile pg_atomic_uint64 *ptr, uint64 val)
{
	ptr->value = val;
}

static inline uint64 pg_atomic_read_u64(volatile pg_atomic_uint64 *ptr)
{
	return ptr->value;
}

static inline bool pg_atomic_compare_exchange_u64(volatile pg_atomic_uint64 *ptr,
						  uint64 *expected, uint64 newval)
{
	return __atomic_compare_exchange_n(&ptr->value, expected, newval, false, __ATOMIC_SEQ_CST,
					   __ATOMIC_SEQ_CST);
}

static inline uint64 pg_atomic_exchange_u64(volatile pg_atomic_uint64 *ptr, uint64 newval)
{
	return __atomic_exchange_n(&ptr->value, newval, __ATOMIC_SEQ_CST);
}

static inline uint64 pg_atomic_fetch_add_u64(volatile pg_atomic_uint64 *ptr, int64 add_)
{
	return __sync_fetch_and_add(&ptr->value, add_);
}

static inline uint64 pg_atomic_fetch_sub_u64(volatile pg_atomic_uint64 *ptr, int64 sub_)
{
	return __sync_fetch_and_sub(&ptr->value, sub_);
}

static inline uint64 pg_atomic_fetch_and_u64(volatile pg_atomic_uint64 *ptr, uint64 and_)
{
	return __sync_fetch_and_and(&ptr->value, and_);
}

static inline uint64 pg_atomic_fetch_or_u64(volatile pg_atomic_uint64 *ptr, uint64 or_)
{
	return __sync_fetch_and_or(&ptr->value, or_);
}

static inline uint64 pg_atomic_read_membarrier_u64(volatile pg_atomic_uint64 *ptr)
{
	return pg_atomic_fetch_add_u64(ptr, 0);
}

static inline void pg_atomic_write_membarrier_u64(volatile pg_atomic_uint64 *ptr, uint64 val)
{
	(void)pg_atomic_exchange_u64(ptr, val);
}

#endif
