/* row: S1 */
/* refuse: J1 */
/* The same loop with a length one too large. The check the pass puts in front of the loop covers
   every byte the loop is going to read, so above -O0 this is refused before the first read rather
   than on the last one, and at -O0 it is refused on the last one. Either way it is refused, which
   is the whole point of taking the check out rather than dropping it.

   Sixteen bytes and not eight, because the runtime hands out storage in granules and eight rounds
   up to sixteen, so an eight byte request with a read one past the end is a read the runtime has
   every right to allow. */
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
    int sum = total(bytes, 17);
    free(bytes);
    return sum == 48 ? 0 : 1;
}
