/* Names reached through the pointer the loader fills in, which is dllimport, a variable only
 * declared reached through the pointer of this file's own the runtime fills in, and a name this
 * program offers through dllexport and looks up again by name. */
#include <stdio.h>

__declspec(dllimport) unsigned long GetCurrentProcessId(void);
__declspec(dllimport) void *GetModuleHandleA(const char *name);
__declspec(dllimport) void *GetProcAddress(void *module, const char *name);
__declspec(dllimport) extern int __argc;
__declspec(dllimport) extern int _fmode;
extern char **_environ;
extern int later;

__declspec(dllexport) int offered(int x) { return x * 3; }
__declspec(dllexport) int shared = 42;

static int through(unsigned long (*get)(void)) { return get() == GetCurrentProcessId(); }

int main(int argc, char **argv) {
    (void)argv;
    printf("pid is not zero %d\n", GetCurrentProcessId() != 0);
    printf("pid through a pointer agrees %d\n", through(GetCurrentProcessId));
    printf("argc agrees %d\n", __argc == argc);
    int *mode = &_fmode;
    printf("fmode through its address agrees %d\n", *mode == _fmode);
    _fmode = 0x8000;
    printf("fmode written %x\n", *mode);
    printf("environ has entries %d\n", _environ != 0 && _environ[0] != 0);
    printf("later %d\n", later);
    void *self = GetModuleHandleA(0);
    int (*found)(int) = (int (*)(int))GetProcAddress(self, "offered");
    printf("offered found %d, says %d\n", found != 0, found ? found(5) : -1);
    int *value = (int *)GetProcAddress(self, "shared");
    printf("shared found %d, holds %d\n", value != 0, value ? *value : -1);
    printf("not offered %d\n", GetProcAddress(self, "through") == 0);
    return 0;
}

int later = 7;
