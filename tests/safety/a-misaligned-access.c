/* row: S7 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
/* An int read one byte into an allocation. The machine allows it and the standard does not, and
   catching it needs the access alignment beside the check, which the capability carries. */
int main(void) {
    char *p = malloc(64);
    int *q;
    int i;
    /* Written first, so that the only thing wrong with the read below is where it starts. */
    for (i = 0; i < 64; i++) {
        p[i] = 0;
    }
    q = (int *)(p + 1);
    return *q;
}
