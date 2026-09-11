/* row: S4 */
/* flags: -fsafety-subobject */
/* refuse: J1 */
/* says: judgement J1 */
/* What `-fsafety-subobject` costs, written as a program rather than as a sentence. C 6.5 says a
   store to allocated storage sets the effective type of that storage, so filling a buffer as int
   and then filling the same buffer as double is something a program is allowed to do. Under the
   flag the second fill disagrees with what the first one recorded and is refused. That is a false
   positive by the letter of the standard, and it is the reason the flag is off by default and is a
   decision the build makes rather than one this compiler makes for it. A pool allocator that hands
   the same block back for a different type is the shape this shows up in outside a test. */
void *malloc(unsigned long size);
void free(void *p);

int main(void) {
    void *storage = malloc(64);
    int *counts = storage;
    double *weights = storage;
    counts[0] = 1;
    weights[0] = 1.5;
    free(storage);
    return 0;
}
