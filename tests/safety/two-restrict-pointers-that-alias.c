/* row: Y8 */
/* refuse: J1 */
/* gap: #431 */
void *malloc(unsigned long size);
void free(void *p);
/* The promise the caller made and broke. Nothing goes out of bounds and nothing is dead, so the
   only evidence is the annotation, and the optimizer will have believed it. `restrict` checking
   is on S5's list for that reason. */
void combine(int *restrict to, int *restrict from, int count) {
    int i;
    for (i = 0; i < count; i++) {
        to[i] = from[i] + 1;
    }
}

int main(void) {
    int *p = malloc(16 * sizeof(int));
    int i;
    for (i = 0; i < 16; i++) {
        p[i] = i;
    }
    combine(p, p + 1, 8);
    free(p);
    return 0;
}
