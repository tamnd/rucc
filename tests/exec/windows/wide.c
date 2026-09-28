/* flags: -municode */
/* A wmain program and a wide string with a character outside the basic plane, which takes two
   UTF-16 code units. */
#include <stdio.h>
#include <wchar.h>

int wmain(int argc, wchar_t **argv) {
    const wchar_t *face = L"a\U0001F600b";
    printf("argc %d\n", argc);
    printf("length %u\n", (unsigned)wcslen(face));
    for (const wchar_t *p = face; *p; p++)
        printf("%04x\n", (unsigned)*p);
    printf("sizeof wchar_t %u\n", (unsigned)sizeof(wchar_t));
    printf("argv[0] ends in exe %d\n", (int)(wcsstr(argv[0], L".exe") != NULL || wcsstr(argv[0], L".EXE") != NULL));
    return 0;
}
