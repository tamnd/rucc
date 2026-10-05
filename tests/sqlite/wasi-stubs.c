/* The three functions of the SQLite shell that start a process, for tests/sqlite/wasm.sh. WASI has
   no processes, so wasi-libc does not define them. Each one fails, which the shell reports as it
   reports a failed command on any other system. The workload calls none of them. */
#include <stdio.h>

int system(const char *command) {
    (void)command;
    return -1;
}

FILE *popen(const char *command, const char *mode) {
    (void)command;
    (void)mode;
    return 0;
}

int pclose(FILE *stream) {
    (void)stream;
    return -1;
}
