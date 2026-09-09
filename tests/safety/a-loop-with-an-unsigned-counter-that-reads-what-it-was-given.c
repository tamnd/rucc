/* row: S1 */
/* allow */
/* The loop shape the hoist pass is really for, counted by an unsigned index. How many times it
   runs is worked out from the limit its own test compared, and that test read the limit as
   unsigned, so the byte count in front of the loop is built by zero extending it. Sign extending
   it instead would make a large limit negative, clamp it to nothing, and leave a check over one
   byte in front of a loop reading all of them. Nothing here is large, but this is the shape, and a
   program that reads exactly what it was given has to be allowed to. */
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
    int sum = total(bytes, 16);
    free(bytes);
    return sum == 48 ? 0 : 1;
}
