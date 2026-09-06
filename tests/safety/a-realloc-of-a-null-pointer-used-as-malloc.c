/* row: T5 */
/* allow */
void *realloc(void *p, unsigned long size);
void free(void *p);
/* The growable array whose first append has nothing to grow. C says realloc of a null pointer is
   malloc, so this is an allocation and not a use of a pointer that names nothing. */
int main(void) {
    int *p = 0;
    int i;
    p = realloc(p, 16 * sizeof(int));
    for (i = 0; i < 16; i++) {
        p[i] = i;
    }
    if (p[15] != 15) {
        return 1;
    }
    free(p);
    return 0;
}
