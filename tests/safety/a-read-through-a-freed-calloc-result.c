/* row: T1 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void free(void *p);
void *calloc(unsigned long count, unsigned long size);
/* An instance is an instance whichever of the four names made it. */
int main(void) {
    int *p = calloc(16, 4);
    free(p);
    return p[0];
}
