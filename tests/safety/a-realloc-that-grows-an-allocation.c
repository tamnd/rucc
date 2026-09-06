/* row: T5 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void *realloc(void *p, unsigned long size);
/* The contents survive and the new instance is as big as it was asked for. */
int main(void) {
    int *p = malloc(64);
    int *q;
    p[0] = 11;
    q = realloc(p, 256);
    q[63] = 22;
    free(q);
    return 0;
}
