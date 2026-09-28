/* The program reads its own import table and says whether it names one C runtime or a mix.
 *
 * A program that imports from both msvcrt.dll and the UCRT has two heaps and two sets of stdio
 * buffers, and a pointer from one freed by the other. It links and it runs until it does not. What
 * the table should name follows the sysroot, so the check is that there is one. Document 08.3. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    unsigned char *base = (unsigned char *)GetModuleHandleA(NULL);
    IMAGE_NT_HEADERS *nt = (IMAGE_NT_HEADERS *)(base + ((IMAGE_DOS_HEADER *)base)->e_lfanew);
    IMAGE_DATA_DIRECTORY dir = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT];
    IMAGE_IMPORT_DESCRIPTOR *d = (IMAGE_IMPORT_DESCRIPTOR *)(base + dir.VirtualAddress);
    int msvcrt = 0, ucrt = 0, kernel32 = 0;
    for (; d->Name; d++) {
        const char *name = (const char *)(base + d->Name);
        fprintf(stderr, "imports %s\n", name);
        if (!_stricmp(name, "msvcrt.dll"))
            msvcrt = 1;
        else if (!_strnicmp(name, "api-ms-win-crt-", 15) || !_stricmp(name, "ucrtbase.dll"))
            ucrt = 1;
        else if (!_stricmp(name, "kernel32.dll"))
            kernel32 = 1;
    }
    printf("kernel32 %s\n", kernel32 ? "yes" : "no");
    printf("%s\n", msvcrt + ucrt == 1 ? "one crt" : msvcrt ? "two crts" : "no crt");
    return 0;
}
