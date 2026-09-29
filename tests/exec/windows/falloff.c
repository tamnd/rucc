/* A function that says it returns an int and falls off the end, called for what it does.
 *
 * C only makes that undefined when the caller uses the value, so the call has to come back and
 * what follows it has to run. At -O2 rucc used to take the end of such a function as a place
 * control never reaches, and everything after the first call to one went with it, the exit
 * included. execute/20020404-1.c in the gcc torture tests is where this was found. */
#include <stdio.h>

static int calls;

__attribute__((noinline)) static int no_answer(int x) {
    calls += x;
}

int main(void) {
    no_answer(1);
    no_answer(2);
    printf("%d\n", calls);
    return 0;
}
