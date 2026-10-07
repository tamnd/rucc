/* row: S2 */
/* refuse: J1 */
/* says: in printf_output, over its dst argument */
int snprintf(char *to, unsigned long size, const char *format, ...);
/* Juliet's CWE-121 and CWE-122 snprintf cases: the size passed is the source's and not the
   destination's, so the output runs past the end of the destination. The family is variadic, so
   there is no wrapper for it, and the judgement is made in front of the call on what the call is
   about to write. */
int main(int argc, char **argv) {
    char source[100];
    char destination[50];
    int i;
    (void)argv;
    for (i = 0; i < 99; i++)
        source[i] = 'C';
    source[99] = 0;
    snprintf(destination, argc + 99, "%s", source);
    return destination[0] == 'C' ? 0 : 1;
}
