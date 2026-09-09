/* stdatomic.h, the C11 atomic names.
 *
 * Everything here is spelled in terms of the `__atomic_` builtins, which is how gcc writes this
 * header too. The standard calls the operations generic functions and none of them can be one:
 * the type of the object decides the width of the access, so a function would need a prototype
 * per type and there is no way to write that. So they are macros and nothing else. gcc declares
 * a function beside each macro, so that a program parenthesising the name to stop the expansion
 * still has something to call, and those calls go to libatomic. There is no libatomic here, so a
 * declaration would promise a function that nothing can ever provide and this header declares
 * none: it adds no name to a translation unit that includes it.
 *
 * Two of these are not what gcc's header says, and both are written out where they appear. The
 * signal fence is a thread fence, which is stronger than it has to be. `kill_dependency` is its
 * argument, which is what it means anywhere that does not track a consume dependency.
 *
 * The arithmetic on an atomic pointer object does not scale by the pointee here, so
 * `atomic_fetch_add(&p, 1)` on an `int * _Atomic` moves the pointer one byte and not four. That
 * is what gcc's header does as well, because these expand to the same builtins and the builtins
 * take a number of bytes. A plain `p += 1` on the same object does scale, since that is the
 * language and not a builtin. */

#ifndef __RUCC_STDATOMIC_H
#define __RUCC_STDATOMIC_H

typedef enum {
  memory_order_relaxed = __ATOMIC_RELAXED,
  memory_order_consume = __ATOMIC_CONSUME,
  memory_order_acquire = __ATOMIC_ACQUIRE,
  memory_order_release = __ATOMIC_RELEASE,
  memory_order_acq_rel = __ATOMIC_ACQ_REL,
  memory_order_seq_cst = __ATOMIC_SEQ_CST
} memory_order;

typedef _Atomic _Bool atomic_bool;
typedef _Atomic char atomic_char;
typedef _Atomic signed char atomic_schar;
typedef _Atomic unsigned char atomic_uchar;
typedef _Atomic short atomic_short;
typedef _Atomic unsigned short atomic_ushort;
typedef _Atomic int atomic_int;
typedef _Atomic unsigned int atomic_uint;
typedef _Atomic long atomic_long;
typedef _Atomic unsigned long atomic_ulong;
typedef _Atomic long long atomic_llong;
typedef _Atomic unsigned long long atomic_ullong;

#ifdef __CHAR8_TYPE__
typedef _Atomic __CHAR8_TYPE__ atomic_char8_t;
#endif
typedef _Atomic __CHAR16_TYPE__ atomic_char16_t;
typedef _Atomic __CHAR32_TYPE__ atomic_char32_t;
typedef _Atomic __WCHAR_TYPE__ atomic_wchar_t;

typedef _Atomic __INT_LEAST8_TYPE__ atomic_int_least8_t;
typedef _Atomic __UINT_LEAST8_TYPE__ atomic_uint_least8_t;
typedef _Atomic __INT_LEAST16_TYPE__ atomic_int_least16_t;
typedef _Atomic __UINT_LEAST16_TYPE__ atomic_uint_least16_t;
typedef _Atomic __INT_LEAST32_TYPE__ atomic_int_least32_t;
typedef _Atomic __UINT_LEAST32_TYPE__ atomic_uint_least32_t;
typedef _Atomic __INT_LEAST64_TYPE__ atomic_int_least64_t;
typedef _Atomic __UINT_LEAST64_TYPE__ atomic_uint_least64_t;

typedef _Atomic __INT_FAST8_TYPE__ atomic_int_fast8_t;
typedef _Atomic __UINT_FAST8_TYPE__ atomic_uint_fast8_t;
typedef _Atomic __INT_FAST16_TYPE__ atomic_int_fast16_t;
typedef _Atomic __UINT_FAST16_TYPE__ atomic_uint_fast16_t;
typedef _Atomic __INT_FAST32_TYPE__ atomic_int_fast32_t;
typedef _Atomic __UINT_FAST32_TYPE__ atomic_uint_fast32_t;
typedef _Atomic __INT_FAST64_TYPE__ atomic_int_fast64_t;
typedef _Atomic __UINT_FAST64_TYPE__ atomic_uint_fast64_t;

typedef _Atomic __INTPTR_TYPE__ atomic_intptr_t;
typedef _Atomic __UINTPTR_TYPE__ atomic_uintptr_t;
typedef _Atomic __SIZE_TYPE__ atomic_size_t;
typedef _Atomic __PTRDIFF_TYPE__ atomic_ptrdiff_t;
typedef _Atomic __INTMAX_TYPE__ atomic_intmax_t;
typedef _Atomic __UINTMAX_TYPE__ atomic_uintmax_t;

/* C17 deprecated this one and C23 took it out, so it is defined only where a program can still
 * be written against it. It never did anything: an atomic object is initialized from a value
 * like any other object. */
#if !(defined __STDC_VERSION__ && __STDC_VERSION__ > 201710L)
#define ATOMIC_VAR_INIT(value) (value)
#endif

/* Initialization is not an atomic operation, and 7.17.2.2 says so: an object being initialized
 * is not one another thread can be reading. Relaxed rather than nothing at all because the store
 * still has to be the width of the object, which is what makes it a store and not two. */
#define atomic_init(object, value) atomic_store_explicit(object, value, __ATOMIC_RELAXED)

#define kill_dependency(value) (value)

#define atomic_thread_fence(order) __atomic_thread_fence(order)

/* A signal fence keeps the compiler from moving accesses across it and asks nothing of the
 * machine, and a thread fence asks the machine as well. So this is stronger than the standard
 * requires, which costs an instruction and cannot give a wrong answer. It stays this way until
 * the IR can hold a barrier that binds one thread only. */
#define atomic_signal_fence(order) __atomic_thread_fence(order)

/* Every atomic type this compiler accepts is one the machine reaches in a single instruction,
 * because the ones it does not are refused where they are written. So the answer is yes, and the
 * object is still named so that a program written with a side effect in there behaves. */
#define atomic_is_lock_free(object) ((void)(object), 1)

#define ATOMIC_BOOL_LOCK_FREE __GCC_ATOMIC_BOOL_LOCK_FREE
#define ATOMIC_CHAR_LOCK_FREE __GCC_ATOMIC_CHAR_LOCK_FREE
#ifdef __GCC_ATOMIC_CHAR8_T_LOCK_FREE
#define ATOMIC_CHAR8_T_LOCK_FREE __GCC_ATOMIC_CHAR8_T_LOCK_FREE
#endif
#define ATOMIC_CHAR16_T_LOCK_FREE __GCC_ATOMIC_CHAR16_T_LOCK_FREE
#define ATOMIC_CHAR32_T_LOCK_FREE __GCC_ATOMIC_CHAR32_T_LOCK_FREE
#define ATOMIC_WCHAR_T_LOCK_FREE __GCC_ATOMIC_WCHAR_T_LOCK_FREE
#define ATOMIC_SHORT_LOCK_FREE __GCC_ATOMIC_SHORT_LOCK_FREE
#define ATOMIC_INT_LOCK_FREE __GCC_ATOMIC_INT_LOCK_FREE
#define ATOMIC_LONG_LOCK_FREE __GCC_ATOMIC_LONG_LOCK_FREE
#define ATOMIC_LLONG_LOCK_FREE __GCC_ATOMIC_LLONG_LOCK_FREE
#define ATOMIC_POINTER_LOCK_FREE __GCC_ATOMIC_POINTER_LOCK_FREE

/* The `_n` builtins, which take and answer a value, rather than the ones that pass it through a
 * second pointer. gcc's header uses the second pointer because it has to work for an object of
 * any size and the wide ones come back through memory. Every atomic object here is one a
 * register holds, so the value form says the same thing without a temporary to write it into. */
#define atomic_store_explicit(object, desired, order) \
  __atomic_store_n(object, desired, order)
#define atomic_store(object, desired) \
  atomic_store_explicit(object, desired, __ATOMIC_SEQ_CST)

#define atomic_load_explicit(object, order) __atomic_load_n(object, order)
#define atomic_load(object) atomic_load_explicit(object, __ATOMIC_SEQ_CST)

#define atomic_exchange_explicit(object, desired, order) \
  __atomic_exchange_n(object, desired, order)
#define atomic_exchange(object, desired) \
  atomic_exchange_explicit(object, desired, __ATOMIC_SEQ_CST)

#define atomic_compare_exchange_strong_explicit(object, expected, desired, success, failure) \
  __atomic_compare_exchange_n(object, expected, desired, 0, success, failure)
#define atomic_compare_exchange_strong(object, expected, desired)                 \
  atomic_compare_exchange_strong_explicit(object, expected, desired,              \
                                          __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)

#define atomic_compare_exchange_weak_explicit(object, expected, desired, success, failure) \
  __atomic_compare_exchange_n(object, expected, desired, 1, success, failure)
#define atomic_compare_exchange_weak(object, expected, desired)                 \
  atomic_compare_exchange_weak_explicit(object, expected, desired,              \
                                        __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)

#define atomic_fetch_add_explicit(object, operand, order) \
  __atomic_fetch_add(object, operand, order)
#define atomic_fetch_add(object, operand) \
  atomic_fetch_add_explicit(object, operand, __ATOMIC_SEQ_CST)

#define atomic_fetch_sub_explicit(object, operand, order) \
  __atomic_fetch_sub(object, operand, order)
#define atomic_fetch_sub(object, operand) \
  atomic_fetch_sub_explicit(object, operand, __ATOMIC_SEQ_CST)

#define atomic_fetch_and_explicit(object, operand, order) \
  __atomic_fetch_and(object, operand, order)
#define atomic_fetch_and(object, operand) \
  atomic_fetch_and_explicit(object, operand, __ATOMIC_SEQ_CST)

#define atomic_fetch_or_explicit(object, operand, order) \
  __atomic_fetch_or(object, operand, order)
#define atomic_fetch_or(object, operand) \
  atomic_fetch_or_explicit(object, operand, __ATOMIC_SEQ_CST)

#define atomic_fetch_xor_explicit(object, operand, order) \
  __atomic_fetch_xor(object, operand, order)
#define atomic_fetch_xor(object, operand) \
  atomic_fetch_xor_explicit(object, operand, __ATOMIC_SEQ_CST)

/* One byte, whatever the type says, which is what the two builtins below reach. gcc wraps that
 * byte in a structure so that nothing but the four names here can touch it. A structure with the
 * qualifier on it is an object this compiler has no instruction for, so the byte is named
 * directly, and the standard already says a program may do nothing else with one of these.
 *
 * An unsigned char rather than a `_Bool` because the two builtins work on a byte and neither of
 * them ever puts anything but a zero or a one in it, so nothing can tell the difference, and
 * because a `_Bool` object in memory is one this compiler cannot generate code for yet. That is
 * `tamnd/rucc#352` and it is what `atomic_bool` above is waiting on as well. */
typedef _Atomic unsigned char atomic_flag;
#define ATOMIC_FLAG_INIT { 0 }

#define atomic_flag_test_and_set(object) __atomic_test_and_set(object, __ATOMIC_SEQ_CST)
#define atomic_flag_test_and_set_explicit(object, order) __atomic_test_and_set(object, order)

#define atomic_flag_clear(object) __atomic_clear(object, __ATOMIC_SEQ_CST)
#define atomic_flag_clear_explicit(object, order) __atomic_clear(object, order)

#if defined __STDC_VERSION__ && __STDC_VERSION__ > 201710L
#define __STDC_VERSION_STDATOMIC_H__ 202311L
#endif

#endif
