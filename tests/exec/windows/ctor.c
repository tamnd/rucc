/* Constructors run before main in priority order, then the ones with no priority, and destructors
   run after main returns in the opposite order. */
#include <stdio.h>

__attribute__((constructor(200))) static void second(void) { puts("ctor 200"); }
__attribute__((constructor(101))) static void first(void) { puts("ctor 101"); }
__attribute__((constructor)) static void plain(void) { puts("ctor plain"); }
__attribute__((destructor(101))) static void last(void) { puts("dtor 101"); fflush(stdout); }
__attribute__((destructor(200))) static void early(void) { puts("dtor 200"); }
__attribute__((destructor)) static void plain_dtor(void) { puts("dtor plain"); }

int main(void) {
    puts("main");
    return 0;
}
