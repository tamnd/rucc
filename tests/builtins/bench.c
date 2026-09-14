/* The four block routines, timed against whatever is linked beside this file.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8, which says the word at a time paths want a
 * benchmark to hold them to rather than an opinion. This is that benchmark. It is the same shape
 * as tests/builtins/differential.c next door, compiled twice by the system compiler, once linked
 * against the archive rucc wrote out of runtime/builtins and once against nothing extra so that
 * the four names resolve in the C library. Neither program knows which side it got.
 *
 * The C library is the denominator rather than a target. glibc's memcpy is hand written vector
 * assembly chosen at load time from what the machine reports, and no portable C is going to reach
 * it. What a ratio against it is good for is being a number that means the same thing on two
 * machines, which an absolute microsecond count is not, and being the thing that says whether a
 * rewrite of these routines moved anything.
 *
 * Every case is timed several times and every timing is over a fixed number of bytes rather than a
 * fixed number of calls, so a row for eight bytes and a row for sixty four kilobytes are two
 * measurements of the same amount of work and the small row is mostly call overhead on purpose.
 *
 * The two shapes each size is run at are the whole reason the file has a shape table. A word at a
 * time loop can only run when the source and the destination reach a word boundary together, so
 * `aligned` is the case it runs in and `skewed` is the case it does not, and a benchmark that only
 * had the first would report an improvement that half the calls in a program never see.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* How many timings are taken, and how many are thrown away first.
 *
 * Ten and three, which is what spec/16-performance.md section 16.2 asks of every benchmark here
 * and what xtask/src/bench.rs already does. Both are arguments, and the task passes one and one,
 * because it runs this program and the other one alternately rather than one after the other: a
 * machine that gets slower halfway through a run should slow both sides of every ratio instead of
 * one side of all of them. The ceiling is there because the timings live in an array on the stack.
 */
#define RUNS 10
#define WARMUPS 3
#define MOST 64

/* The bytes one timing moves, whatever the size of the call it moves them in.
 *
 * Two megabytes is large enough that the clock is not the thing being measured and small enough
 * that the whole run takes a couple of seconds. It is a multiple of every size below, so a row is
 * an exact count of bytes rather than a rounded one.
 */
#define PER_RUN (2u * 1024u * 1024u)

/* The buffers. Aligned hard so that `aligned` means aligned rather than means whatever the linker
 * happened to do, and with room above the largest size for the offsets the shapes ask for.
 */
#define BIGGEST 65536u
#define ROOM (BIGGEST + 128u)

static _Alignas(64) unsigned char left[ROOM];
static _Alignas(64) unsigned char right[ROOM];

/* Where the answers go, so that nothing here is a loop whose result is unused. The calls survive
 * anyway, since -fno-builtin leaves the compiler knowing nothing about these four names, but a
 * benchmark that relies on that and does not say so is one flag away from timing an empty loop.
 */
static unsigned long long sink;

/* Which routine a case runs. `memmove-down` is the overlapping case, where the destination sits
 * above the source and the copy has to run backwards, which is the one path in these four that no
 * other routine has.
 */
enum Which { COPY, MOVE, MOVE_DOWN, SET, CMP };

static const char *const ROUTINE[] = { "memcpy", "memmove", "memmove-down", "memset", "memcmp" };

/* The lengths. Eight is one word on a 64-bit machine and is where the overhead of the call is the
 * measurement, sixty four is a small structure, a kilobyte is a buffer, and sixty four kilobytes
 * is the largest that still fits in a cache near the core, which keeps the row about the routine
 * rather than about the memory system.
 */
static const size_t SIZE[] = { 8, 64, 1024, BIGGEST };

/* Where the two ends of a case sit.
 *
 * `from` and `to` are offsets into the source and destination buffers, and `gap` is how far above
 * the source the destination sits in the overlapping case, which needs both ends in one buffer.
 * The aligned shape puts every pointer on a word boundary. The skewed shape puts the two ends
 * seven bytes apart in the ordinary case and one byte apart in the overlapping one, and either is
 * enough to keep a word at a time loop from running at all.
 */
struct Shape {
    const char *name;
    size_t from;
    size_t to;
    size_t gap;
};

static const struct Shape SHAPE[] = {
    { "aligned", 0, 0, 64 },
    { "skewed", 1, 8, 57 },
};

#define COUNT(a) (sizeof(a) / sizeof((a)[0]))

/* The clock, as a count of nanoseconds. Monotonic rather than the wall clock, because a benchmark
 * that reads the wall clock reports whatever the machine did about a leap second.
 */
static double nanos(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (double)now.tv_sec * 1000000000.0 + (double)now.tv_nsec;
}

/* Fills both buffers with the same bytes, and then makes the two ends of a comparison equal.
 *
 * Run before every timing rather than once, so that each timing is over the same bytes and the
 * spread between them is the machine rather than the data. It matters for two of the five cases:
 * the overlapping move rewrites its own source, and a comparison against bytes that differ early
 * returns early and would time a few bytes rather than the length in the row.
 *
 * The filling is a byte loop written here rather than a call to memset or memcpy, since those are
 * the routines under test and a harness that prepares its input with them is timing them twice.
 */
static void prepare(int which, unsigned char *out, const unsigned char *in, size_t size) {
    unsigned long long state = 0x2545F4914F6CDD1DULL;
    size_t i;
    for (i = 0; i < ROOM; i += 1) {
        state = state * 6364136223846793005ULL + 1442695040888963407ULL;
        left[i] = (unsigned char)(state >> 33);
        right[i] = left[i];
    }
    if (which == CMP) {
        for (i = 0; i < size; i += 1) {
            out[i] = in[i];
        }
    }
}

/* One timing's worth of calls.
 *
 * The switch is outside the loop rather than inside it, because at eight bytes a switch in the
 * loop is a measurable part of what the row reports and the row is supposed to be about the call.
 */
static void batch(int which, unsigned char *out, const unsigned char *in, size_t size,
                  size_t iters) {
    size_t i;
    switch (which) {
    case COPY:
        for (i = 0; i < iters; i += 1) {
            memcpy(out, in, size);
        }
        break;
    case MOVE:
    case MOVE_DOWN:
        for (i = 0; i < iters; i += 1) {
            memmove(out, in, size);
        }
        break;
    case SET:
        for (i = 0; i < iters; i += 1) {
            memset(out, (int)i, size);
        }
        break;
    case CMP:
        for (i = 0; i < iters; i += 1) {
            sink += (unsigned long long)memcmp(out, in, size);
        }
        break;
    default:
        break;
    }
}

int main(int argc, char **argv) {
    int runs = RUNS;
    int warmups = WARMUPS;
    double taken[MOST];
    unsigned long long checksum = 0;
    size_t w;

    if (argc > 1) {
        runs = atoi(argv[1]);
        if (runs < 1 || runs > MOST) {
            fprintf(stderr, "runs must be between 1 and %d\n", MOST);
            return 1;
        }
    }
    if (argc > 2) {
        warmups = atoi(argv[2]);
        if (warmups < 0) {
            fprintf(stderr, "warmups cannot be negative\n");
            return 1;
        }
    }

    for (w = 0; w < COUNT(ROUTINE); w += 1) {
        size_t s;
        for (s = 0; s < COUNT(SIZE); s += 1) {
            size_t h;
            for (h = 0; h < COUNT(SHAPE); h += 1) {
                const struct Shape *shape = &SHAPE[h];
                size_t size = SIZE[s];
                size_t iters = PER_RUN / size;
                unsigned char *out;
                const unsigned char *in;
                int r;

                if ((int)w == MOVE_DOWN) {
                    in = left + shape->from;
                    out = left + shape->from + shape->gap;
                } else {
                    in = left + shape->from;
                    out = right + shape->to;
                }

                for (r = 0; r < warmups; r += 1) {
                    prepare((int)w, out, in, size);
                    batch((int)w, out, in, size, iters);
                }
                for (r = 0; r < runs; r += 1) {
                    double started;
                    prepare((int)w, out, in, size);
                    started = nanos();
                    batch((int)w, out, in, size, iters);
                    taken[r] = nanos() - started;
                    checksum = checksum * 31 + out[0];
                    checksum = checksum * 31 + out[size - 1];
                }

                printf("case %s %lu %s %llu", ROUTINE[w], (unsigned long)size, shape->name,
                       (unsigned long long)iters * (unsigned long long)size);
                for (r = 0; r < runs; r += 1) {
                    printf(" %.1f", taken[r]);
                }
                printf("\n");
            }
        }
    }

    /* Printed so that the two sides can be held against each other on something other than their
     * speed. It is not the correctness check, which is the differential next door over twenty
     * seven thousand cases, but a routine that copies the wrong bytes fast should not be able to
     * post a number here and have nobody notice.
     */
    printf("checksum %llu\n", checksum);
    printf("sink %llu\n", sink);
    return 0;
}
