/* row: Y2 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The allowed side of the strict aliasing row, and the case that costs the most to get wrong: a
   whole eight byte granule stored through one type and read back through the same one. The plane
   keeps one slot per granule and the top bit of a slot means the bytes disagree, so a type numbered
   above that bit is stored here and read back as something else entirely. */
int main(void) {
    double *d = malloc(8 * sizeof(double));
    long *l = malloc(8 * sizeof(long));
    int i;
    double total = 0;
    long sum = 0;
    for (i = 0; i < 8; i++) {
        d[i] = i / 2.0;
        l[i] = i;
    }
    for (i = 0; i < 8; i++) {
        total += d[i];
        sum += l[i];
    }
    free(l);
    free(d);
    return total > 13.0 && sum == 28 ? 0 : 1;
}
