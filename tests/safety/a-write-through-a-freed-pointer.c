/* row: T1 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void free(void *p);
/* The same, writing, which is the half an attacker wants. */
int main(void) {
    int *p = malloc(64);
    free(p);
    p[0] = 7;
    return 0;
}
