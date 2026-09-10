/* row: S1 */
/* allow */
/* The loop shape a program written with lengths rather than with counts has, counted by an index as
   wide as the arithmetic that turns it into a byte count. The width of the type bounds nothing at
   that width, so what bounds the count is the guard in front of the loop, and the pass asks the
   ranges for it at the preheader rather than where the length came from. Take the guard away and
   the loop keeps its check, which is the honest answer for a length nothing bounds.

   Sixty four rather than sixteen so the guard is not the same number as the allocation, since a
   guard that happened to be exactly the length would leave it unclear which of the two the bound
   came from. */
void *malloc(unsigned long size);
void free(void *p);

int total(unsigned char *a, unsigned long n) {
    int sum = 0;
    unsigned long i;
    if (n > 64) {
        return 0;
    }
    for (i = 0; i < n; i++) {
        sum += a[i];
    }
    return sum;
}

int main(void) {
    unsigned char *bytes = malloc(16);
    unsigned i;
    for (i = 0; i < 16; i++) {
        bytes[i] = 3;
    }
    int sum = total(bytes, 16);
    free(bytes);
    return sum == 48 ? 0 : 1;
}
