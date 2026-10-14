/* flags: -pthread */
/* pthread_cancel of a thread in a loop of this half, once at the pthread_testcancel in the loop and
 * once in asynchronous mode, where the cancel can stop the thread at any instruction of the loop.
 * glibc unwinds with _Unwind_ForcedUnwind from there to the cleanup handler, so each frame of this
 * half needs a correct .eh_frame entry, and in the second case a correct one at each instruction. */
#include <pthread.h>
#include <stdio.h>

void started(void);

static void note(void *p)
{
    printf("cleanup %s\n", (const char *)p);
    fflush(stdout);
}

void *deferred(void *arg)
{
    volatile long n = 0;
    (void)arg;
    pthread_cleanup_push(note, "deferred");
    started();
    for (;;) {
        n++;
        pthread_testcancel();
    }
    pthread_cleanup_pop(0);
    return 0;
}

void *async(void *arg)
{
    volatile long n = 0;
    int old;
    (void)arg;
    pthread_cleanup_push(note, "async");
    started();
    pthread_setcanceltype(PTHREAD_CANCEL_ASYNCHRONOUS, &old);
    for (;;)
        n = n * 3 + 1;
    pthread_cleanup_pop(0);
    return 0;
}
