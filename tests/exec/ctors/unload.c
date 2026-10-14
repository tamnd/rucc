/* shared */
/* This half is a shared object. The GCC half opens it, calls it and closes it, twice. Each
 * dlopen runs the constructors in the order of their priorities. Each dlclose runs the function
 * given to atexit and the destructors before it returns, since atexit in a shared object is tied
 * to the object through __dso_handle. */
#include <stdio.h>
#include <stdlib.h>

static int count;

static void gone(void)
{
    printf("atexit, count %d\n", count);
}

__attribute__((constructor(150))) static void first(void)
{
    count = 1;
    puts("ctor 150");
}

__attribute__((constructor)) static void second(void)
{
    count++;
    atexit(gone);
    puts("ctor");
}

__attribute__((destructor(150))) static void last(void)
{
    puts("dtor 150");
}

__attribute__((destructor)) static void early(void)
{
    printf("dtor, count %d\n", count);
}

int used(void)
{
    return ++count;
}
