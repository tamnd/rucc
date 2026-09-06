/* row: T1 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void free(void *p);
/* Use after free, the bug the lifetime plane exists for. */
int main(void) {
    int *p = malloc(64);
    p[0] = 7;
    free(p);
    return p[0];
}
