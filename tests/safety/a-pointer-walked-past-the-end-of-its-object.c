/* row: S5 */
/* refuse: J2 */
void *malloc(unsigned long size);
void free(void *p);
/* A loop with the wrong bound, which is what most heap overflows actually look like. The report
   names the iteration that left the object rather than whatever eventually read through it. */
int main(void) {
    int *p = malloc(64);
    int *q = p;
    int i;
    for (i = 0; i < 100; i++) {
        q = p + i;
    }
    return *q;
}
