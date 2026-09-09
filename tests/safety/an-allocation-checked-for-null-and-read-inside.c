/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The shape the optimizer reads an extent off, which is a request for a fixed number of bytes and
   a test of the answer before anything goes through it. Every access here is inside what was asked
   for, so the program runs and the run has to agree with that. */
int main(void) {
    int *p = malloc(4 * sizeof(int));
    int sum;
    if (p == 0) {
        return 0;
    }
    p[0] = 1;
    p[3] = 2;
    sum = p[0] + p[3];
    free(p);
    return sum - 3;
}
