/* row: T5 */
/* allow */
void *malloc(unsigned long size);
void *realloc(void *p, unsigned long size);
void free(void *p);
/* Shrinking is the case where the allocator most likely hands back the same address, so a
   checker that compares addresses sees nothing happen and a version based one sees the instance
   end and a new one begin. Both have to agree that using the new pointer is fine. */
int main(void) {
    int *p = malloc(64 * sizeof(int));
    int *small;
    int i;
    for (i = 0; i < 64; i++) {
        p[i] = i;
    }
    small = realloc(p, 4 * sizeof(int));
    if (small[3] != 3) {
        return 1;
    }
    free(small);
    return 0;
}
