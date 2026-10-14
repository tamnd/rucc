/* The GCC half of unload.c. It gets the path of the shared object as its argument. */
#include <dlfcn.h>
#include <stdio.h>

int main(int argc, char **argv)
{
    if (argc < 2)
        return 2;
    for (int round = 0; round < 2; round++) {
        printf("open %d\n", round);
        void *lib = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
        if (!lib) {
            printf("dlopen: %s\n", dlerror());
            return 1;
        }
        int (*used)(void) = (int (*)(void))dlsym(lib, "used");
        if (!used) {
            printf("dlsym: %s\n", dlerror());
            return 1;
        }
        printf("used %d\n", used());
        puts("close");
        if (dlclose(lib) != 0) {
            printf("dlclose: %s\n", dlerror());
            return 1;
        }
        puts("closed");
    }
    return 0;
}
