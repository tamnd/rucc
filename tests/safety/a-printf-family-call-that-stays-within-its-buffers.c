/* row: S2 */
/* allow */
int printf(const char *format, ...);
int snprintf(char *to, unsigned long size, const char *format, ...);
int sprintf(char *to, const char *format, ...);
/* The two shapes before this one done right, and the loose habits real programs have that do no
   harm: a size larger than the destination with an output that fits, a precision that stops a read
   before the end of a string with no terminator, a null string, which glibc prints as `(null)`,
   and a format with a width taken from an argument. */
int main(int argc, char **argv) {
    char destination[16];
    char word[4] = {'r', 'u', 'c', 'c'};
    char *nothing = 0;
    (void)argv;
    snprintf(destination, sizeof destination * 4, "%d %s", argc, "fits");
    printf("%s\n", destination);
    printf("%.4s %*d\n", word, argc + 2, argc);
    sprintf(destination, "%.4s", word);
    printf("%s %s\n", destination, argc > 5 ? destination : nothing);
    return 0;
}
