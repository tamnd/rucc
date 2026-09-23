/* row: Y6 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
void *memcpy(void *dst, const void *src, unsigned long n);
/* The init plane's form of the same thing. The callee copies bytes nobody wrote over an `int` the
   caller has already read, which makes those bytes unwritten again, and a call that frees nothing
   was taken to leave the caller's written range standing. */
static int *junk;

__attribute__((noinline)) void copy_junk(int *into) {
    memcpy(into, junk, sizeof(int));
}

int main(void) {
    int *p = malloc(sizeof(int));
    int first;
    junk = malloc(sizeof(int));
    *p = 1;
    first = *p;
    copy_junk(p);
    first += *p;
    free(p);
    free(junk);
    return first;
}
