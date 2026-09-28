/* args: *.c "two words"
 *
 * A Windows program gets one command line and splits it itself, in the CRT, before main. mingw-w64
 * does not expand wildcards there unless the program asks for it or the CRT was configured with
 * --enable-wildcard, and rucc's sysroot is not. WinLibs' GCC is, so this .out is written by hand
 * rather than blessed from it. Document 09.3. */
#include <stdio.h>

int main(int argc, char **argv) {
    printf("argc %d\n", argc);
    for (int i = 1; i < argc; i++)
        printf("argv[%d] %s\n", i, argv[i]);
    return 0;
}
