/* flags: -fexceptions -fnon-call-exceptions -pthread */
/* A fault in the scope of a cleanup handler, turned into an unwind by a signal handler that calls
 * pthread_exit. Under -fnon-call-exceptions the load and the division are covered the way a call
 * is, so both handlers run on the way out of each thread, the inner one first, as they do with
 * gcc. Without the flag only the outer one would run, since the fault is not in a call. */
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>

static void done(int *p) { printf("cleanup %d\n", *p); fflush(stdout); }
static void outer(int *p) { printf("outer %d\n", *p); fflush(stdout); }

static void on_fault(int sig) { (void)sig; pthread_exit(0); }

int *volatile hole;
volatile int zero;

static int work(int k) {
    int a __attribute__((cleanup(done))) = k;
    if (k == 1)
        return *hole;
    return 10 / zero;
}

static void *run(void *arg) {
    int b __attribute__((cleanup(outer))) = (int)(long)arg;
    return (void *)(long)work((int)(long)arg);
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_fault;
    sa.sa_flags = SA_NODEFER;
    sigaction(SIGSEGV, &sa, 0);
    sigaction(SIGFPE, &sa, 0);
    for (long k = 1; k <= 2; k++) {
        pthread_t t;
        pthread_create(&t, 0, run, (void *)k);
        pthread_join(t, 0);
    }
    puts("end");
    return 0;
}
