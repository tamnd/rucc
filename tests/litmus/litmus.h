/*
 * The harness every litmus test here runs under.
 *
 * A litmus test is a few threads, each doing two or three memory accesses, and a list of outcomes
 * the memory model forbids. One run of it says almost nothing, because the interleavings that show
 * a missing barrier are rare, so each test is run a few million times and every outcome is looked
 * at afterwards.
 *
 * The runs are grouped into rounds. A round starts one thread per role, and those threads go
 * through the round's iterations together, meeting at a barrier before each one so that the
 * accesses of one iteration are made as close to the same moment as the machine allows. Every
 * iteration has locations of its own, an element of a set of arrays, so nothing has to be reset
 * between iterations and nothing one iteration left behind is read by the next. What each thread
 * saw goes into a slot of its own, and the outcomes are counted by the main thread once the round
 * is over, where the counting cannot disturb what it is counting.
 *
 * Rounds are run until the test has done LITMUS_ITERATIONS iterations or LITMUS_SECONDS seconds
 * have gone by, whichever comes first. Both can be set from the environment.
 *
 * A test also counts the outcomes that are allowed but only reachable through reordering, where it
 * has any. Those are never a failure. They are printed so that a run can be seen to have reached
 * the interleavings that matter at all: a store buffering test whose unfenced control never once
 * sees both loads read zero is a test that would not have noticed a missing barrier either.
 */

#ifndef LITMUS_H
#define LITMUS_H

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* Iterations in one round, and so the length of every array a test keeps per iteration. */
#define LITMUS_ROUND 16384

/* The most threads any test starts at once. */
#define LITMUS_THREADS 8

struct litmus {
	/* What the test is called in the report. */
	const char *name;
	/* How many threads, each running body with its own index. */
	int threads;
	/* Called by the main thread before each round, to put every location back. */
	void (*reset)(void);
	/* One thread's part of one iteration. */
	void (*body)(int thread, long i);
	/* Called by the main thread after each round: how many iterations did what is forbidden. */
	long (*forbidden)(void);
	/* The same for what is allowed and only reachable by reordering, or null. */
	long (*weak)(void);
};

static long litmus_max_iterations = 2000000;
static double litmus_max_seconds = 1.0;
static int litmus_failed;

/* The barrier the threads of a round meet at before each iteration. */
static long litmus_arrived;

struct litmus_thread {
	const struct litmus *test;
	int index;
};

static inline double litmus_now(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static inline void *litmus_thread_main(void *arg)
{
	struct litmus_thread *self = arg;
	const struct litmus *test = self->test;
	long threads = test->threads;
	long i;

	for (i = 0; i < LITMUS_ROUND; i++) {
		long want = (i + 1) * threads;
		__atomic_fetch_add(&litmus_arrived, 1, __ATOMIC_SEQ_CST);
		while (__atomic_load_n(&litmus_arrived, __ATOMIC_ACQUIRE) < want)
			;
		test->body(self->index, i);
	}
	return NULL;
}

/* Reads LITMUS_ITERATIONS and LITMUS_SECONDS, once, before the first test. */
static inline void litmus_setup(void)
{
	const char *iterations = getenv("LITMUS_ITERATIONS");
	const char *seconds = getenv("LITMUS_SECONDS");
	if (iterations && *iterations)
		litmus_max_iterations = strtol(iterations, NULL, 10);
	if (seconds && *seconds)
		litmus_max_seconds = strtod(seconds, NULL);
}

/* Runs one test to its limit, prints a line about it, and remembers whether it failed. */
static inline void litmus_run(const struct litmus *test)
{
	pthread_t tids[LITMUS_THREADS];
	struct litmus_thread args[LITMUS_THREADS];
	double start = litmus_now();
	double elapsed = 0;
	long done = 0, forbidden = 0, weak = 0;
	int t;

	if (test->threads > LITMUS_THREADS) {
		printf("%-40s  asks for %d threads, the harness has room for %d\n", test->name,
		       test->threads, LITMUS_THREADS);
		litmus_failed = 1;
		return;
	}
	while (done < litmus_max_iterations && elapsed < litmus_max_seconds) {
		test->reset();
		litmus_arrived = 0;
		for (t = 0; t < test->threads; t++) {
			args[t].test = test;
			args[t].index = t;
			if (pthread_create(&tids[t], NULL, litmus_thread_main, &args[t]) != 0) {
				printf("%-40s  could not start a thread\n", test->name);
				litmus_failed = 1;
				return;
			}
		}
		for (t = 0; t < test->threads; t++)
			pthread_join(tids[t], NULL);
		forbidden += test->forbidden();
		if (test->weak)
			weak += test->weak();
		done += LITMUS_ROUND;
		elapsed = litmus_now() - start;
	}
	printf("%-40s  %8ld iterations in %.2fs, ", test->name, done, elapsed);
	if (test->weak)
		printf("%ld reordered, ", weak);
	if (forbidden) {
		printf("%ld FORBIDDEN\n", forbidden);
		litmus_failed = 1;
	} else {
		printf("none forbidden\n");
	}
	fflush(stdout);
}

/*
 * The counting tests, which are not litmus shapes but many threads hammering one location, share
 * the thread starting and the report.
 */
struct hammer {
	const char *name;
	int threads;
	long each;
	void (*body)(int thread, long each);
	/* How many things were wrong afterwards, and a line saying what, when anything was. */
	long (*check)(int threads, long each, char *why, size_t size);
};

struct hammer_thread {
	const struct hammer *test;
	int index;
};

/* Where the threads of a counting test wait for each other, so all of them start at once. */
static long hammer_ready;

static inline void *hammer_thread_main(void *arg)
{
	struct hammer_thread *self = arg;
	__atomic_fetch_add(&hammer_ready, 1, __ATOMIC_SEQ_CST);
	while (__atomic_load_n(&hammer_ready, __ATOMIC_ACQUIRE) < self->test->threads)
		;
	self->test->body(self->index, self->test->each);
	return NULL;
}

static inline void hammer_run(const struct hammer *test)
{
	pthread_t tids[LITMUS_THREADS];
	struct hammer_thread args[LITMUS_THREADS];
	char why[200] = "";
	double start = litmus_now();
	long wrong;
	int t;

	hammer_ready = 0;
	for (t = 0; t < test->threads; t++) {
		args[t].test = test;
		args[t].index = t;
		if (pthread_create(&tids[t], NULL, hammer_thread_main, &args[t]) != 0) {
			printf("%-40s  could not start a thread\n", test->name);
			litmus_failed = 1;
			return;
		}
	}
	for (t = 0; t < test->threads; t++)
		pthread_join(tids[t], NULL);
	wrong = test->check(test->threads, test->each, why, sizeof why);
	printf("%-40s  %d threads x %ld in %.2fs, ", test->name, test->threads, test->each,
	       litmus_now() - start);
	if (wrong) {
		printf("WRONG: %s\n", why);
		litmus_failed = 1;
	} else {
		printf("ok\n");
	}
	fflush(stdout);
}

#endif
