/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Every element again, from the end. A checker that only understands increasing addresses gets
   this wrong, and reversing a buffer is not a rare thing to do. */
int main(void) {
    int *p = malloc(16 * sizeof(int));
    int sum = 0;
    int i;
    for (i = 0; i < 16; i++) {
        p[i] = i;
    }
    for (i = 15; i >= 0; i--) {
        sum += p[i];
    }
    free(p);
    return sum == 120 ? 0 : 1;
}
