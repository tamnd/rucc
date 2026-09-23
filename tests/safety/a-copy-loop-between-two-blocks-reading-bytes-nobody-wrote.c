/* row: Y6 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
/* A copy loop between two blocks from two calls to malloc, where only the first half of the source
   was written. The loop writes the planes over the destination only, so the init check on the
   source comes out in front of the loop at -O2 and has to refuse there what the check inside would
   have refused at the thirty third byte. */
int main(void) {
    char *from = malloc(64);
    char *to = malloc(64);
    long i;
    int sum = 0;
    for (i = 0; i < 32; i++) {
        from[i] = (char)i;
    }
    for (i = 0; i < 64; i++) {
        to[i] = from[i];
    }
    for (i = 0; i < 64; i++) {
        sum += to[i];
    }
    free(from);
    free(to);
    return sum == 0;
}
