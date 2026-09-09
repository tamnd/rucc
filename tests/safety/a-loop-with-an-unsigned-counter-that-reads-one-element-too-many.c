/* row: S1 */
/* refuse: J1 */
/* The same loop with a length one too large, which is the caller getting it wrong rather than the
   loop. The check the pass puts in front of the loop covers every byte the loop is going to read,
   so at anything above -O0 this is refused before the first read rather than on the last one, and
   the report is the same either way. Reading one element too many is what an off by one in a
   length actually looks like.

   Sixteen bytes and not eight, because the runtime hands out storage in granules and eight rounds
   up to sixteen, so an eight byte request with a read one past the end is a read the runtime has
   every right to allow. Sixteen is a granule already, which is what makes one past the end
   outside it. */
void *malloc(unsigned long size);
void free(void *p);

int total(unsigned char *a, unsigned n) {
    unsigned char *p = a;
    int sum = 0;
    unsigned i;
    for (i = 0; i < n; i++) {
        sum += *p;
        p++;
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
