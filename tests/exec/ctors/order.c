/* Constructors and destructors with priorities in this half and in the GCC half. The linker sorts
 * .init_array.NNNNN and .fini_array.NNNNN by name across both objects, so they run in the order of
 * their priorities, whichever compiler built them. The ones with no priority run last, in the
 * order of the objects on the link line. */
#include <stdio.h>
#include <stdlib.h>

static void late(void)
{
    puts("atexit rucc");
}

__attribute__((constructor(101))) static void ctor101(void)
{
    puts("ctor 101 rucc");
}

__attribute__((constructor(300))) static void ctor300(void)
{
    puts("ctor 300 rucc");
}

__attribute__((constructor)) static void ctor(void)
{
    puts("ctor rucc");
    atexit(late);
}

__attribute__((destructor(101))) static void dtor101(void)
{
    puts("dtor 101 rucc");
}

__attribute__((destructor(300))) static void dtor300(void)
{
    puts("dtor 300 rucc");
}

__attribute__((destructor)) static void dtor(void)
{
    puts("dtor rucc");
}
