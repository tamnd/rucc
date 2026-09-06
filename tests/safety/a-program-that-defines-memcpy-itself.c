/* row: S8 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* A program with its own memcpy means its own. Redirecting these calls to a wrapper around the C
   library's would be a miscompilation rather than a monitor, so a name the file defines is left
   alone. The accesses inside it are instrumented like any others. */
void *memcpy(void *to, const void *from, unsigned long count) {
    char *out = to;
    const char *in = from;
    unsigned long i;
    for (i = 0; i < count; i++) {
        out[i] = in[i];
    }
    return to;
}

int main(void) {
    char *from = malloc(16);
    char *to = malloc(16);
    from[0] = 7;
    memcpy(to, from, 16);
    if (to[0] != 7) {
        return 1;
    }
    free(from);
    free(to);
    return 0;
}
