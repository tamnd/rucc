/* row: T4 */
/* refuse: J1 */
/* says: in memcpy, over its src argument */
void *memcpy(void *dst, const void *src, unsigned long n);
/* The same frame, read by a copy of a length the program works out rather than a walk. One
   judgement covers the whole range, and the witness is part of it. A length known where the call
   is would make the copy loads and stores, which are checked the ordinary way. */
__attribute__((noinline)) static int *fill(int n) {
    int local[8];
    int i;
    for (i = 0; i < 8; i++)
        local[i] = n + i;
    return local;
}

int main(int argc, char **argv) {
    int out[8];
    int *p = fill(argc);
    (void)argv;
    memcpy(out, p, sizeof out - (argc - 1) * sizeof *out);
    return out[3];
}
