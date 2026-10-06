/* The start of a wasm32-none program under the test host of tests/wasm-none/host.c, for
   tests/rung0/run.sh (tamnd/rucc#3137). rucc links it with each program and names `_start` as the
   entry with -Wl,--entry=_start.

   It gives the few functions of the C library that the programs of the subset call, and the
   functions that rucc and the builtins call for a copy or a fill of a block. Each one is a plain
   loop over bytes, because the test is of the code that rucc writes for the program and not of
   these. `putchar` goes to the host, and so does the exit status of `main`.

   rucc names `main` `__main_void` when it has no parameters and `__main_argc_argv` when it has
   two, as clang does on wasm. Both are weak here, so the one that the program defines is not null.
   The file is C89, so that it builds under every standard that a program of the suite names. */
typedef __SIZE_TYPE__ size_t;

__attribute__((import_module("env"), import_name("exit"), noreturn)) void host_exit(int status);
__attribute__((import_module("env"), import_name("putchar"))) int host_putchar(int c);

__attribute__((weak)) int __main_void(void);
__attribute__((weak)) int __main_argc_argv(int argc, char **argv);

int putchar(int c) {
    return host_putchar(c);
}

size_t strlen(const char *s) {
    size_t n = 0;
    while (s[n])
        n++;
    return n;
}

void *memcpy(void *to, const void *from, size_t n) {
    unsigned char *t = to;
    const unsigned char *f = from;
    while (n--)
        *t++ = *f++;
    return to;
}

void *memmove(void *to, const void *from, size_t n) {
    unsigned char *t = to;
    const unsigned char *f = from;
    if (t < f)
        while (n--)
            *t++ = *f++;
    else
        while (n--)
            t[n] = f[n];
    return to;
}

void *memset(void *to, int c, size_t n) {
    unsigned char *t = to;
    while (n--)
        *t++ = (unsigned char)c;
    return to;
}

int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *x = a;
    const unsigned char *y = b;
    for (; n; n--, x++, y++)
        if (*x != *y)
            return *x - *y;
    return 0;
}

void _start(void) {
    static char name[] = "main";
    static char *argv[] = {name, 0};
    host_exit(__main_void ? __main_void() : __main_argc_argv(1, argv));
}
