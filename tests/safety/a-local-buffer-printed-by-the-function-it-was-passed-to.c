/* row: T4 */
/* allow */
int printf(const char *format, ...);
unsigned long strlen(const char *s);
/* The other direction, which is how nearly every program hands a local to the C library: the
   frame that owns the buffer is still running while a callee prints it, so its witness still
   says it is there and nothing may be refused. */
__attribute__((noinline)) static unsigned long show(const char *s) {
    printf("%s\n", s);
    return strlen(s);
}

int main(int argc, char **argv) {
    char buf[16];
    int i;
    (void)argv;
    for (i = 0; i < 15; i++)
        buf[i] = 'a' + (i + argc) % 26;
    buf[15] = 0;
    return show(buf) == 15 ? 0 : 1;
}
