/* row: S1 */
/* refuse: J1 */
/* The same loop reading a slot that is not there. Above -O0 the check that catches it is the one in
   front of the loop rather than the one inside it, since the address never moved and the pass took
   the inside one out, and the report is the same either way. This is the program that says the
   check came out of the loop rather than went missing. */
void *malloc(unsigned long size);
void free(void *p);

int total(unsigned char *a, int rounds) {
    int sum = 0;
    int i;
    for (i = 0; i < rounds; i++) {
        sum += a[4];
        sum += a[16];
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
