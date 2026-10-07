/* row: S1 */
/* flags: -O2 */
/* allow */
void *malloc(unsigned long size);
void free(void *ptr);
/* The loop before this one with a copy that fits: ten bytes into a block of ten, split by the
   optimizer the same way, and every one of them in the half with no checks. */
int main(int argc, char **argv) {
    char *data = malloc(10);
    char source[10] = "AAAAAAAAA";
    unsigned long i, n;
    (void)argv;
    if (!data)
        return 1;
    n = 9 + (unsigned long)argc;
    for (i = 0; i < n; i++)
        data[i] = source[i];
    i = data[9];
    free(data);
    return (int)i;
}
