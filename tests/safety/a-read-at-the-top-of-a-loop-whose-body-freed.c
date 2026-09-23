/* row: T1 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void free(void *p);
/* The loop form of the read after a join. The header's immediate dominator is the block in front
   of the loop, which read the same pointer, and the second time round the header is reached from
   the bottom of the body, after the free. */
volatile int rounds = 2;

int main(void) {
    int *p = malloc(64);
    int total = 0;
    int i;
    p[0] = 7;
    total = p[0];
    for (i = 0; i < rounds; i++) {
        total += p[0];
        if (i == 0) {
            free(p);
        }
    }
    return total;
}
