/* row: T4 */
/* refuse: J1 */
/* says: in printf_string, over its string argument */
int printf(const char *format, ...);
/* Juliet's CWE-562 cases: a function fills a buffer of its own and returns it, and the caller
   prints it. The read happens inside the C library, so it is the wrapper that has to ask the
   witness whether the frame is still there. Not inlined, so that at -O2 there is still a frame to
   return from. */
__attribute__((noinline)) static char *name(int n) {
    char buf[16];
    int i;
    for (i = 0; i < 15; i++)
        buf[i] = 'a' + (i + n) % 26;
    buf[15] = 0;
    return buf;
}

int main(int argc, char **argv) {
    char *p = name(argc);
    (void)argv;
    printf("%s\n", p);
    return 0;
}
