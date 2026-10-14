/* The GCC half of order.c, with priorities between the ones of the rucc half. */
#include <stdio.h>
#include <stdlib.h>

static void late(void)
{
    puts("atexit gcc");
}

__attribute__((constructor(200))) static void ctor200(void)
{
    puts("ctor 200 gcc");
}

__attribute__((constructor(400))) static void ctor400(void)
{
    puts("ctor 400 gcc");
}

__attribute__((constructor)) static void ctor(void)
{
    puts("ctor gcc");
    atexit(late);
}

__attribute__((destructor(200))) static void dtor200(void)
{
    puts("dtor 200 gcc");
}

__attribute__((destructor(400))) static void dtor400(void)
{
    puts("dtor 400 gcc");
}

__attribute__((destructor)) static void dtor(void)
{
    puts("dtor gcc");
}

int main(void)
{
    puts("main");
    return 0;
}
