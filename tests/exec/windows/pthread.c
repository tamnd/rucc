/* flags: -pthread */
/* gcc flags: -static */
/* POSIX threads out of the sysroot's winpthreads, which -pthread links as -lpthread. Four threads add
   to one counter under a mutex, hand a value back through pthread_join, and wait on a condition
   variable until the main thread lets them finish. pthread_once runs its function once whatever the
   number of callers, and a key's value is per thread. The library is static, so the program must
   not import libwinpthread-1.dll, which no Windows has. An msys2 gcc links that DLL unless told
   -static. */
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <windows.h>

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t ready = PTHREAD_COND_INITIALIZER;
static pthread_once_t once = PTHREAD_ONCE_INIT;
static pthread_key_t key;
static long counter;
static int waiting;
static int go;
static int inits;

static void init(void) {
    inits++;
}

static void *work(void *arg) {
    long id = (long)(INT_PTR)arg;
    pthread_once(&once, init);
    pthread_setspecific(key, (void *)(INT_PTR)(id * 100));
    for (int i = 0; i < 100000; i++) {
        pthread_mutex_lock(&lock);
        counter++;
        pthread_mutex_unlock(&lock);
    }
    pthread_mutex_lock(&lock);
    waiting++;
    pthread_cond_broadcast(&ready);
    while (!go)
        pthread_cond_wait(&ready, &lock);
    pthread_mutex_unlock(&lock);
    return (void *)(INT_PTR)((long)(INT_PTR)pthread_getspecific(key) + id);
}

static int imports_winpthread(void) {
    unsigned char *base = (unsigned char *)GetModuleHandleA(NULL);
    IMAGE_NT_HEADERS *nt = (IMAGE_NT_HEADERS *)(base + ((IMAGE_DOS_HEADER *)base)->e_lfanew);
    IMAGE_DATA_DIRECTORY dir = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT];
    IMAGE_IMPORT_DESCRIPTOR *d = (IMAGE_IMPORT_DESCRIPTOR *)(base + dir.VirtualAddress);
    for (; d->Name; d++)
        if (!_strnicmp((const char *)(base + d->Name), "libwinpthread", 13))
            return 1;
    return 0;
}

int main(void) {
    pthread_t threads[4];
    pthread_key_create(&key, NULL);
    pthread_setspecific(key, (void *)7);
    for (long i = 0; i < 4; i++)
        if (pthread_create(&threads[i], NULL, work, (void *)(INT_PTR)i) != 0) {
            printf("pthread_create failed\n");
            return 1;
        }
    pthread_mutex_lock(&lock);
    while (waiting < 4)
        pthread_cond_wait(&ready, &lock);
    printf("waiting %d counter %ld\n", waiting, counter);
    go = 1;
    pthread_cond_broadcast(&ready);
    pthread_mutex_unlock(&lock);
    for (int i = 0; i < 4; i++) {
        void *result;
        pthread_join(threads[i], &result);
        printf("thread %d returned %ld\n", i, (long)(INT_PTR)result);
    }
    printf("once ran %d time\n", inits);
    printf("main key %ld\n", (long)(INT_PTR)pthread_getspecific(key));
    printf("equal self %d\n", pthread_equal(pthread_self(), pthread_self()) != 0);
    printf("imports libwinpthread %s\n", imports_winpthread() ? "yes" : "no");
    return 0;
}
