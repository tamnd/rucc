/* row: T5 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void *realloc(void *p, unsigned long size);
void free(void *p);
/* realloc may move the block, and the old pointer is dead whether it did or not. Writing through
   it is the version of this bug that corrupts rather than the one that reads rubbish. */
int main(void) {
    int *p = malloc(4 * sizeof(int));
    int *grown = realloc(p, 64 * sizeof(int));
    p[0] = 1;
    free(grown);
    return 0;
}
