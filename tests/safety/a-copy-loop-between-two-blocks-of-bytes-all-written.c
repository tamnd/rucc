/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The same copy with every byte of the source written first. Taking the source's plane checks out
   of the loop must not turn this into a refusal, and the destination's bytes are initialized by
   the time they are read back even though the plane writes over them moved to after the loop. */
int main(void) {
    char *from = malloc(64);
    char *to = malloc(64);
    long i;
    int sum = 0;
    for (i = 0; i < 64; i++) {
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
