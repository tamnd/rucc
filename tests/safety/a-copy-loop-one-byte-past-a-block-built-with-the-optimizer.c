/* row: S1 */
/* flags: -O2 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *ptr);
unsigned long strlen(const char *s);
/* Juliet's CWE-122 CWE193 loop cases: eleven bytes copied one at a time into a block of ten. At -O2
   the loop is split into a half with no checks in it and a half with all of them, divided at how
   much of the block is left. The allocator rounds a block of ten up to sixteen, and the answer used
   to count the rounding, so all eleven bytes went through the half with no checks. */
int main(int argc, char **argv) {
    char *data = malloc(10);
    char source[11] = "AAAAAAAAAA";
    unsigned long i, n;
    (void)argv;
    if (!data)
        return 1;
    n = strlen(source) + (unsigned long)argc - 1;
    for (i = 0; i < n + 1; i++)
        data[i] = source[i];
    i = data[0];
    free(data);
    return (int)i == 'A' ? 0 : 1;
}
