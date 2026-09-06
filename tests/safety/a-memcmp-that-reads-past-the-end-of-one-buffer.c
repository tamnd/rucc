/* row: S8 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
int memcmp(const void *a, const void *b, unsigned long count);
/* A read rather than a write, which is the half that leaks instead of corrupting. The answer comes
   back as one integer, and a comparison that ran off the end of one buffer answered partly about
   whatever was next to it. */
int main(void) {
    char *a = malloc(16);
    char *b = malloc(64);
    int same = memcmp(a, b, 64);
    free(a);
    free(b);
    return same;
}
