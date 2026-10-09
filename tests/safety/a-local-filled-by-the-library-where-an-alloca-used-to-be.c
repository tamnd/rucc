/* row: Y6 */
/* allow */
void *alloca(unsigned long size);
struct utsname {
    char field[6][65];
};
int uname(struct utsname *name);
/* A function takes a few hundred bytes off the stack, writes half of them and returns, and the
   next call at the same depth has a local of its own in the same place that the C library fills.
   The library records nothing it writes, so if the half the alloca left unwritten were still said
   to be unwritten once its function returned, the read of the name would be refused. It is not:
   what the alloca took goes when the frame does. */
static int __attribute__((noinline)) taken(int n) {
    char *bytes = alloca(n);
    int i;
    for (i = 0; i < n / 2; i++) {
        bytes[i] = (char)i;
    }
    return bytes[n / 2 - 1];
}

static int __attribute__((noinline)) named(void) {
    struct utsname name;
    if (uname(&name) != 0) {
        return 1;
    }
    return name.field[0][0] != 'L';
}

int main(int argc, char **argv) {
    int n = argc + 399;
    (void)argv;
    if (taken(n) != (char)(n / 2 - 1)) {
        return 1;
    }
    return named();
}
