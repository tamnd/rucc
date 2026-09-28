/* environ and _timezone are data in the C runtime DLL, reached without a dllimport in the source. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

extern char **environ;

int main(void) {
    int n = 0, path = 0;
    for (char **e = environ; *e; e++) {
        n++;
        if (_strnicmp(*e, "PATH=", 5) == 0)
            path = 1;
    }
    printf("environ has entries %d\n", n > 0);
    printf("environ has PATH %d\n", path);
    _tzset();
    long zone = _timezone;
    printf("timezone in range %d\n", zone > -86400 && zone < 86400);
    printf("getenv agrees %d\n", getenv("PATH") != NULL);
    return 0;
}
