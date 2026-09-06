/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void *calloc(unsigned long count, unsigned long size);
/* calloc zeroes what it hands out, and every byte of it belongs to the instance. */
int main(void) {
    int *p = calloc(16, 4);
    int i;
    int sum = 0;
    for (i = 0; i < 16; i++) {
        sum += p[i];
    }
    free(p);
    return sum;
}
