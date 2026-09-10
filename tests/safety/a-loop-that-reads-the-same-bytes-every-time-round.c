/* row: S1 */
/* allow */
/* An address that does not move, which is what a loop over a fixed set of slots looks like and is
   the shape SQLite's chacha block function is built out of. The check in front of the loop covers
   one access rather than a swept range, because every iteration was asking about the same bytes.
   Nothing here is unusual, and the reason it is a case worth writing down is that the pass used to
   report it as somebody else's to answer and nobody else was answering it. */
void *malloc(unsigned long size);
void free(void *p);

int total(unsigned char *a, int rounds) {
    int sum = 0;
    int i;
    for (i = 0; i < rounds; i++) {
        sum += a[4];
        sum += a[8];
    }
    return sum;
}

int main(void) {
    unsigned char *bytes = malloc(16);
    unsigned i;
    for (i = 0; i < 16; i++) {
        bytes[i] = 3;
    }
    int sum = total(bytes, 5);
    free(bytes);
    return sum == 30 ? 0 : 1;
}
