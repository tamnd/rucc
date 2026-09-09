/* row: S1 */
/* allow */
/* The same loop as the pointer walk beside this one, written the way most C is written, with a
   subscript instead of a pointer moved by hand. The two look the same in C and are not the same in
   the IR: the address here is a zero extension of the counter multiplied by the element size, so
   getting one check in front of the loop needs that extension widened, and an unsigned counter
   carries no promise that it will not wrap. The loop's own test is the promise. It keeps the
   counter under the limit, so the counter never reaches the top of its type, so the widening
   describes the same numbers the narrow sequence does. */
void *malloc(unsigned long size);
void free(void *p);

int total(unsigned char *a, unsigned n) {
    int sum = 0;
    unsigned i;
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
