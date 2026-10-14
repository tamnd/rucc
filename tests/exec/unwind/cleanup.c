/* flags: -fexceptions -pthread */
/* A cleanup variable in this half runs when a C++ exception or a forced unwind from pthread_exit
 * goes through its frame, and on a plain return. The inner one runs first. */
#include <stdio.h>

void deep(int k);

static void note(int *p)
{
    printf("cleanup %d\n", *p);
    fflush(stdout);
}

__attribute__((noinline)) int layer(int k)
{
    int a __attribute__((cleanup(note))) = k;
    deep(k);
    return a;
}

__attribute__((noinline)) int outer(int k)
{
    int b __attribute__((cleanup(note))) = k + 100;
    return layer(k) + 1;
}
