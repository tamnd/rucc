/* row: S2 */
/* refuse: J1 */
/* says: in printf_string, over its string argument */
int printf(const char *format, ...);
void *malloc(unsigned long size);
/* Juliet's CWE-126 cases: a string filled to the end of its block with no terminator, then printed
   with `%s`, which reads past the end looking for one. */
int main(int argc, char **argv) {
    char *data = malloc(8);
    int i;
    (void)argv;
    if (!data)
        return 1;
    for (i = 0; i < 8; i++)
        data[i] = 'A' + argc;
    printf("%s\n", data);
    return 0;
}
