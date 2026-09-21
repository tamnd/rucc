/* The main a fuzz harness does not have, so that a corpus can be replayed without libFuzzer. */
/* A harness is one function, LLVMFuzzerTestOneInput, and sometimes an LLVMFuzzerInitialize beside
   it. libFuzzer supplies everything else: the main, the mutation loop, the coverage instrumentation
   and the process that dies when an input kills it. A replay wants none of that. It wants each
   input in the corpus handed to the harness once, in an order two runs agree on, with a report
   attributed to the input that produced it.

   The forking is the part worth explaining. The monitor runs in the continue posture, so a report
   does not stop the program, but an input that really does corrupt the library can still take the
   process down, and a replay of forty thousand inputs that stops at the four hundredth is a replay
   that found one thing and hid the rest. So each input runs in a child of its own. The parent
   prints the input's name, forks, waits, and prints how the child ended, and whatever the child
   wrote lands between those two lines because the parent waits before printing anything else.

   It also means the harness starts each input from a clean address space, which is what libFuzzer
   does not do and what makes a replay comparable: an input's reports are about that input rather
   than about whatever the previous forty left lying in the allocator.

   The cost is one fork per input. That is a few hundred microseconds against a decompression, and
   it buys a replay that finishes.

   The other thing libFuzzer supplies and a replay still needs is a clock. A corpus is full of
   inputs that ask a library to produce far more than they contain, which is the point of a
   compression corpus, and under instrumentation a decode that took a second natively can take a
   great deal longer. libFuzzer's answer is -timeout, and this is the same answer: the child sets an
   alarm before it starts and the parent tells a death by that alarm apart from any other, because
   an input that ran out of time is a thing to say and not a thing to fail on. */
#include <dirent.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* What the harness provides, under the name libFuzzer calls it by, so that a harness written here
   and a harness taken from upstream are the same file. There is no LLVMFuzzerInitialize here: it is
   the hook for a target that has to set something up before the first input, none of the targets
   this stands in for has one, and a harness that needs one can do it on its first call. */
int LLVMFuzzerTestOneInput(const unsigned char *data, unsigned long size);

/* Bigger than any corpus entry any of these projects has, and small enough that a file over it is a
   file somebody pointed this at by mistake rather than an input. */
#define LIMIT (16 * 1024 * 1024)

/* How long one input gets, in seconds. libFuzzer's own default is twenty five and that is against
   an uninstrumented build, so this is a minute and it is a backstop rather than a budget: a harness
   that lets a single input run this long has been asked to do something unbounded and should say so
   itself, which is what the output cap in the brotli harness is. */
#define PATIENCE 60

/* Read one file whole, or say nothing came back. */
static unsigned char *slurp(const char *path, unsigned long *size) {
    FILE *f = fopen(path, "rb");
    unsigned char *room;
    long end;

    if (f == NULL) {
        return NULL;
    }
    if (fseek(f, 0, SEEK_END) != 0) {
        fclose(f);
        return NULL;
    }
    end = ftell(f);
    if (end < 0 || end > LIMIT) {
        fclose(f);
        return NULL;
    }
    rewind(f);
    room = malloc((unsigned long)end + 1);
    if (room == NULL) {
        fclose(f);
        return NULL;
    }
    *size = fread(room, 1, (unsigned long)end, f);
    fclose(f);
    return room;
}

/* Hand one input to the harness in a child, and say how the child ended.
   The child's exit is its own, so a harness that returns is a zero and a harness that was killed is
   the signal, which is what the caller prints. */
static int once(const char *path) {
    unsigned long size = 0;
    unsigned char *data = slurp(path, &size);
    pid_t child;
    int status = 0;

    if (data == NULL) {
        return -1;
    }
    fflush(NULL);
    child = fork();
    if (child == 0) {
        alarm(PATIENCE);
        LLVMFuzzerTestOneInput(data, size);
        fflush(NULL);
        _exit(0);
    }
    free(data);
    if (child < 0) {
        return -1;
    }
    if (waitpid(child, &status, 0) != child) {
        return -1;
    }
    return status;
}

/* Every name in the directory, sorted, so that two runs of the same corpus walk it the same way.
   readdir's order is the file system's and is not an order. */
static int before(const void *a, const void *b) {
    return strcmp(*(const char *const *)a, *(const char *const *)b);
}

static char **names(const char *dir, int *count) {
    DIR *open = opendir(dir);
    struct dirent *entry;
    char **all = NULL;
    int held = 0;

    *count = 0;
    if (open == NULL) {
        return NULL;
    }
    while ((entry = readdir(open)) != NULL) {
        if (entry->d_name[0] == '.') {
            continue;
        }
        if (*count == held) {
            held = held == 0 ? 64 : held * 2;
            all = realloc(all, (unsigned long)held * sizeof(*all));
            if (all == NULL) {
                closedir(open);
                *count = 0;
                return NULL;
            }
        }
        all[*count] = malloc(strlen(entry->d_name) + 1);
        if (all[*count] == NULL) {
            break;
        }
        strcpy(all[*count], entry->d_name);
        (*count)++;
    }
    closedir(open);
    if (all != NULL) {
        qsort(all, (unsigned long)*count, sizeof(*all), before);
    }
    return all;
}

int main(int argc, char **argv) {
    char **all;
    int count = 0;
    int i;
    int ran = 0;

    if (argc < 2) {
        fprintf(stderr, "replay: give me a directory of inputs\n");
        return 2;
    }
    all = names(argv[1], &count);
    if (all == NULL) {
        fprintf(stderr, "replay: nothing to read in %s\n", argv[1]);
        return 2;
    }
    for (i = 0; i < count; i++) {
        char path[4096];
        int status;

        if (snprintf(path, sizeof path, "%s/%s", argv[1], all[i]) >= (int)sizeof path) {
            continue;
        }
        printf("<<<input %s>>>\n", all[i]);
        fflush(stdout);
        status = once(path);
        fflush(NULL);
        if (status < 0) {
            printf("<<<ended unread>>>\n");
        } else if (WIFSIGNALED(status) && WTERMSIG(status) == SIGALRM) {
            printf("<<<ended timeout>>>\n");
        } else if (WIFSIGNALED(status)) {
            printf("<<<ended signal %d>>>\n", WTERMSIG(status));
        } else {
            printf("<<<ended %d>>>\n", WEXITSTATUS(status));
        }
        ran++;
    }
    printf("<<<replayed %d>>>\n", ran);
    return 0;
}
