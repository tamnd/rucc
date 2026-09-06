/* row: S5 */
/* refuse: J2 */
void *malloc(unsigned long size);
void free(void *p);
/* The other end. One past the end is allowed because C allows it; one before the start is not,
   and never was. */
int main(void) {
    int *p = malloc(64);
    int *q = p - 1;
    return *q;
}
