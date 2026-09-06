/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Grow by hand rather than by realloc: allocate the bigger one, copy, free the old one, keep
   using the new one. Two instances are live at once and the old one dies while the new one does
   not, which is the smallest test that the plane tracks instances and not addresses. */
int main(void) {
    int *small = malloc(4 * sizeof(int));
    int *large;
    int i;
    for (i = 0; i < 4; i++) {
        small[i] = i;
    }
    large = malloc(8 * sizeof(int));
    for (i = 0; i < 4; i++) {
        large[i] = small[i];
    }
    free(small);
    for (i = 4; i < 8; i++) {
        large[i] = i;
    }
    if (large[7] != 7) {
        return 1;
    }
    free(large);
    return 0;
}
