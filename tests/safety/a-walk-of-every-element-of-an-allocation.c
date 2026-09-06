/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The case that has to be silent, and the one there are the most of in a real program. */
int main(void) {
    int *p = malloc(64);
    int i;
    int sum = 0;
    for (i = 0; i < 16; i++) {
        p[i] = i;
    }
    for (i = 0; i < 16; i++) {
        sum += p[i];
    }
    free(p);
    return sum - 120;
}
