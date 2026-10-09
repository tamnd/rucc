/* row: Y6 */
/* allow */
void *alloca(unsigned long size);
/* A variable length array declared each time round a loop and an alloca in a function called from
   it, each written in full before it is read. The bytes are begun unwritten each time they are
   taken, so what was written last time round must not count, and what the stores write now must. */
static int __attribute__((noinline)) summed(int n) {
    int *taken = alloca(n * sizeof *taken);
    int total = 0;
    int i;
    for (i = 0; i < n; i++) {
        taken[i] = i;
    }
    for (i = 0; i < n; i++) {
        total += taken[i];
    }
    return total;
}

int main(int argc, char **argv) {
    int n = argc + 31;
    int total = 0;
    int round;
    (void)argv;
    for (round = 0; round < 4; round++) {
        int row[n];
        int i;
        for (i = 0; i < n; i++) {
            row[i] = round + i;
        }
        total += row[n - 1] + summed(n);
    }
    return total == 4 * (31 + 496) + 6 ? 0 : 1;
}
