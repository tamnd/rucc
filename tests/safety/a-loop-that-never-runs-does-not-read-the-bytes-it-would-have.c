/* row: S1 */
/* allow */
/* The same bad slot in the same loop, with a count of zero, so the loop never runs and the read
   never happens. A program that returns has to keep returning, and this is the case that says the
   check taken out of the loop went to a block the program only reaches when the loop is going to
   run at least once. Get that wrong and this traps on a read nothing performed, which document 02
   calls a release blocking bug rather than a conservative answer. */
void *malloc(unsigned long size);
void free(void *p);

int total(unsigned char *a, int rounds) {
    int sum = 0;
    int i;
    for (i = 0; i < rounds; i++) {
        sum += a[16];
    }
    return sum;
}

int main(void) {
    unsigned char *bytes = malloc(16);
    int sum = total(bytes, 0);
    free(bytes);
    return sum == 0 ? 0 : 1;
}
