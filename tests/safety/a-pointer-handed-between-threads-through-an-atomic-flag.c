/* row: 9.5 the edge an atomic carries */
/* flags: -fsafety-races=pointer */
/* allow */
/* The publication pattern, which is the reason the atomics had to become edges. One thread fills a
   record, stores the pointer to it where the other thread can see it, and then sets a flag with
   release ordering. The other thread reads that flag with acquire ordering and only then reads the
   pointer. It is a correct program and the atomic pair is the whole of what orders it: there is no
   lock anywhere in it, and the reader deliberately reads before it joins so that the join is not
   what carries the ordering. Without the two markers the compiler now puts around an atomic, the
   epoch plane would hold a stamp from the writer that the reader has not got past, and a race would
   be reported against a program doing nothing wrong. This is the one place in the safety pass where
   instrumentation nobody wrote costs a false report rather than a missed one, so the case that has
   to pass is the quiet one. `=pointer` because the read of the published pointer is where the
   question gets put, and that is the mode that asks at a read. */
void *malloc(unsigned long size);
int pthread_create(void **thread, void *attr, void *(*start)(void *), void *arg);
int pthread_join(void *thread, void **value);

struct handoff {
    int *value;
    int flag;
};

static struct handoff *shared;

static void *writer(void *arg) {
    int *made = malloc(sizeof(int));
    *made = 42;
    shared->value = made;
    __atomic_store_n(&shared->flag, 1, __ATOMIC_RELEASE);
    return arg;
}

int main(void) {
    void *thread;
    int *seen;
    long spins = 0;

    shared = malloc(sizeof(struct handoff));
    shared->value = 0;
    shared->flag = 0;

    if (pthread_create(&thread, 0, writer, 0) != 0) {
        /* Nothing to say about ordering on a machine that would not give us a second thread. */
        return 0;
    }
    /* Bounded, so that a run where the flag never arrives ends in a failure rather than in a
       test suite that hangs. Two threads and a writer that does almost nothing get here at once. */
    while (__atomic_load_n(&shared->flag, __ATOMIC_ACQUIRE) == 0) {
        if (++spins > 100000000L) {
            return 1;
        }
    }
    seen = shared->value;
    pthread_join(thread, 0);
    return *seen == 42 ? 0 : 1;
}
