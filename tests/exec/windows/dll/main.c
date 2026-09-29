/* Builds plain.c and listed.c as DLLs with -shared, links this against their import libraries, and
 * checks what each one exports by asking the loader. */

#include <stdio.h>
#include <windows.h>

int twice(int);
int big(int);
int add(int, int);
__declspec(dllimport) extern int counter;

static const char *found(HMODULE dll, const char *name)
{
    return GetProcAddress(dll, name) ? "exported" : "not exported";
}

int main(void)
{
    printf("twice %d big %d\n", twice(21), big(1));
    printf("add %d counter %d\n", add(2, 3), counter);
    counter += 2;
    printf("counter %d\n", counter);

    HMODULE plain = GetModuleHandleA("plain.dll");
    HMODULE listed = GetModuleHandleA("listed.dll");
    printf("loaded %d %d\n", plain != NULL, listed != NULL);
    printf("plain twice %s\n", found(plain, "twice"));
    printf("plain ___chkstk_ms %s\n", found(plain, "___chkstk_ms"));
    printf("listed add %s\n", found(listed, "add"));
    printf("listed counter %s\n", found(listed, "counter"));
    printf("listed unlisted %s\n", found(listed, "unlisted"));
    return 0;
}
