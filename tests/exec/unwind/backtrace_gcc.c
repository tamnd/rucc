#include <execinfo.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int first(int k);

/* Prints the name of each frame up to main. The frames below main are the C library's. */
__attribute__((noinline)) int bottom(int k)
{
    void *pcs[64];
    int n = backtrace(pcs, 64);
    char **names = backtrace_symbols(pcs, n);
    for (int i = 0; i < n; i++) {
        const char *open = strchr(names[i], '(');
        const char *end = open ? strpbrk(open, "+)") : 0;
        if (!end || end == open + 1) {
            printf("?\n");
            continue;
        }
        printf("%.*s\n", (int)(end - open - 1), open + 1);
        if (strncmp(open + 1, "main+", 5) == 0)
            break;
    }
    free(names);
    return k;
}

int main(void)
{
    printf("%d\n", first(1));
    return 0;
}
