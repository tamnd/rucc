/* The GCC half of mixed.c, a shared library linked with -latomic. */
typedef unsigned __int128 u128;

void bump(u128 *p, long n)
{
    for (long i = 0; i < n; i++) {
        u128 old = __atomic_load_n(p, __ATOMIC_RELAXED);
        while (!__atomic_compare_exchange_n(p, &old, old + (((u128)1 << 64) | 1), 0,
                                            __ATOMIC_SEQ_CST, __ATOMIC_RELAXED))
            ;
    }
}
