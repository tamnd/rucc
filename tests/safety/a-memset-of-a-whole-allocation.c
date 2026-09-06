/* row: S8 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void *memset(void *to, int byte, unsigned long count);
/* Clearing a buffer right after allocating it is the most common libc call in C and it touches
   every byte of the instance, including the ones the granule rounding added. */
int main(void) {
    char *p = malloc(100);
    memset(p, 0, 100);
    if (p[99] != 0) {
        return 1;
    }
    free(p);
    return 0;
}
