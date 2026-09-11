/* row: 9.5 the edge a fence carries */
/* flags: -fsafety-races=pointer */
/* allow */
/* The same handoff as the atomic one next to this, written the way a fence is usually written. The
   flag is set and read with relaxed ordering, which says the word does not tear and says nothing
   at all about what happened either side of it, and the ordering comes from the two fences: a
   release fence after the record is filled and an acquire fence once the flag has been seen. It is
   a correct program and those two fences are the whole of what orders it. A fence has no object in
   it to key an edge on, so the pair of markers the compiler puts around one takes no operands and
   the runtime holds one clock for every fence in the program rather than a table. That orders more
   pairs of threads than this program has, which is the safe direction: a thread put further ahead
   reports fewer races, never a race that is not there. Without the markers the reader would find
   the writer's stamp on the published pointer with nothing to have got past it, and a race would be
   reported against a program doing nothing wrong. */
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
    __atomic_thread_fence(__ATOMIC_RELEASE);
    __atomic_store_n(&shared->flag, 1, __ATOMIC_RELAXED);
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
    /* Bounded, so that a run where the flag never arrives ends in a failure rather than in a test
       suite that hangs. Two threads and a writer that does almost nothing get here at once. */
    while (__atomic_load_n(&shared->flag, __ATOMIC_RELAXED) == 0) {
        if (++spins > 100000000L) {
            return 1;
        }
    }
    __atomic_thread_fence(__ATOMIC_ACQUIRE);
    /* Before the join, so that the join is not what carries the ordering. */
    seen = shared->value;
    pthread_join(thread, 0);
    return *seen == 42 ? 0 : 1;
}
