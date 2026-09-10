/* The program that reads the library, built by the system compiler so that what is being checked
 * is the library rather than agreement between two halves of the same compiler.
 *
 * One line per case, in the shape `what: answer`, which is what xtask/src/dso.rs compares. */

#include <dlfcn.h>
#include <stdio.h>

extern int shared_var;
int reads_shared(void);
int reads_other(void);
int calls_hidden(void);
int calls_interposed(void);

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <library>\n", argv[0]);
        return 2;
    }

    /* Already loaded, since the program is linked against it, so this hands back the same object
     * and the question is only what its dynamic symbol table has in it. That is the check that
     * tamnd/rucc#733 needed: a static link never reads the field it got wrong. */
    void *lib = dlopen(argv[1], RTLD_NOW);
    if (!lib) {
        fprintf(stderr, "dlopen: %s\n", dlerror());
        return 2;
    }

    int (*found)(void) = (int (*)(void))dlsym(lib, "exported");
    printf("exported through dlsym: %d\n", found ? found() : -1);
    printf("hidden_helper through dlsym: %s\n", dlsym(lib, "hidden_helper") ? "found" : "absent");

    /* The library defines this and the executable refers to it, so the linker makes room for it
     * here and every reference in the process has to reach this copy, the library's own included.
     * A library that worked the address out from where its instruction was would read the copy in
     * the library and see 1. */
    shared_var = 42;
    printf("shared variable the library reads: %d\n", reads_shared());

    printf("other library's variable: %d\n", reads_other());
    printf("hidden helper called inside the library: %d\n", calls_hidden());
    printf("interposed: %d\n", calls_interposed());
    return 0;
}
