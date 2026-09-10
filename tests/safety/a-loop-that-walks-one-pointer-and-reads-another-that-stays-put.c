/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Two accesses in one loop, one that moves and one that does not, which is what the loop splitter
   works out two different limits for. One loop in five that it takes on SQLite has this shape. */
static void fill(int *out, const int *base, int n) {
    int i;
    for (i = 0; i < n; i++) {
        out[i] = *base + i;
    }
}
int main(void) {
    int *p = malloc(16 * sizeof(int));
    int base = 7;
    int last;
    fill(p, &base, 16);
    last = p[15];
    free(p);
    return last - 22;
}
