/* row: S1 */
/* refuse: J2 */
void *malloc(unsigned long size);
void free(void *p);
/* Underflow is the same bug facing the other way, and it is the one that reads the allocator's
   own bookkeeping rather than the next program object. One past the end is permitted and one
   before the start is not, so the subtraction is refused where the addition would not be. */
int main(void) {
    int *p = malloc(64);
    int seen = p[-1];
    free(p);
    return seen;
}
