/* row: T2 */
/* refuse: J6 */
void *malloc(unsigned long size);
void free(void *p);
/* The second free is a free of something that is not an allocation any more. */
int main(void) {
    int *p = malloc(64);
    free(p);
    free(p);
    return 0;
}
