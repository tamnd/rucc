/* row: Y6 */
/* refuse: J1 */
/* A local whose address is never taken, so the compiler keeps it in a register and there are no
   bytes for the init plane to keep. It is written only when the program has more than five
   arguments and read either way, which is CWE-457's most common shape. Whether it was written is
   a flag carried beside the value, and with no arguments the flag says no. */
int printf(const char *format, ...);

static int pick(int argc) {
    int data;
    if (argc > 5) {
        data = 3;
    }
    return data;
}

int main(int argc, char **argv) {
    (void)argv;
    printf("%d\n", pick(argc));
    return 0;
}
