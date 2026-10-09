/* row: T4 */
/* allow */
int printf(const char *format, ...);
/* The callee's buffer is used only while the callee runs, once for each time round the loop. The
   witness is shut where each copy returns and opened again where the next one begins, and
   nothing in between reaches it. */
static void total(int n, int *sum) {
    char buf[16];
    int i;
    for (i = 0; i < 15; i++)
        buf[i] = '0' + (n + i) % 10;
    buf[15] = 0;
    for (i = 0; buf[i]; i++)
        *sum += buf[i] - '0';
    if (n == 3)
        printf("%s\n", buf);
}

int main(int argc, char **argv) {
    int sum = 0;
    int n;
    (void)argv;
    for (n = 0; n < 8 + argc; n++)
        total(n, &sum);
    return sum > 0 ? 0 : 1;
}
