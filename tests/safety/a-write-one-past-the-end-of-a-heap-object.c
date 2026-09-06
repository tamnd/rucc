/* row: S1 */
/* refuse: J1 */
/* says: which no instance owns */
void *malloc(unsigned long size);
void free(void *p);
/* The same overflow the other way round, which is the one that corrupts something. */
int main(void) {
    int *p = malloc(64);
    p[16] = 1;
    return 0;
}
