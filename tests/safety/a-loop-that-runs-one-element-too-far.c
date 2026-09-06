/* row: S1 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
/* The classic. The bound is the element count and the comparison should have been less than,
   so the last iteration reads the element that is not there. */
int main(void) {
    int *p = malloc(16 * sizeof(int));
    int sum = 0;
    int i;
    for (i = 0; i <= 16; i++) {
        sum += p[i];
    }
    free(p);
    return sum;
}
