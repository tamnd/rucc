/* row: T5 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void *realloc(void *p, unsigned long size);
/* A resize down to nothing, which glibc treats as a free. The program kept the pointer because
   the resize looked like every other resize in the same function. */
int main(void) {
    int *p = malloc(4 * sizeof(int));
    p[0] = 3;
    realloc(p, 0);
    return p[0];
}
