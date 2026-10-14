/* One 16-byte object that this half and a GCC shared library built with -latomic both update with
 * a compare and swap, from two threads. Each update adds one to both halves of the object, so a
 * lost update shows as a count below the total, and a torn one as two halves that differ. The
 * library is opened with dlopen, so it does not bind to the routines of this program, and the
 * process has both the routines this program was linked with and libatomic. They only agree if
 * they are the same routines. */
#include <dlfcn.h>
#include <pthread.h>
#include <stdio.h>

typedef unsigned __int128 u128;

#define ROUNDS 1000000L

static _Alignas(16) u128 shared;
static void (*theirs)(u128 *, long);

static void *mine(void *arg)
{
    (void)arg;
    for (long i = 0; i < ROUNDS; i++) {
        u128 old = __atomic_load_n(&shared, __ATOMIC_RELAXED);
        while (!__atomic_compare_exchange_n(&shared, &old, old + (((u128)1 << 64) | 1), 0,
                                            __ATOMIC_SEQ_CST, __ATOMIC_RELAXED))
            ;
    }
    return 0;
}

static void *other(void *arg)
{
    (void)arg;
    theirs(&shared, ROUNDS);
    return 0;
}

int main(int argc, char **argv)
{
    if (argc < 2)
        return 2;
    void *lib = dlopen(argv[1], RTLD_NOW);
    if (!lib) {
        printf("dlopen: %s\n", dlerror());
        return 1;
    }
    theirs = (void (*)(u128 *, long))dlsym(lib, "bump");
    if (!theirs) {
        printf("dlsym: %s\n", dlerror());
        return 1;
    }
    pthread_t a, b;
    pthread_create(&a, 0, mine, 0);
    pthread_create(&b, 0, other, 0);
    pthread_join(a, 0);
    pthread_join(b, 0);
    printf("low %lu\n", (unsigned long)shared);
    printf("high %lu\n", (unsigned long)(shared >> 64));
    return 0;
}
