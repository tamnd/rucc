/* gcc skip: the CRT of MinGW GCC 16 from MSYS2 leaves the exception unhandled */
/* not on msvc: the universal CRT raises SIGFPE for a float fault only, not a divide by zero */
/* An integer divided by zero in main with a SIGFPE handler installed, which Windows raises as a
 * structured exception and can only deliver by unwinding main, so main needs a row in .pdata. The
 * listing asks for the same row with .seh_ directives, which crates/rucc/tests/windows_unwind.rs
 * holds to the bytes the object writer puts here. */
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>

volatile int zero;

static void caught(int sig) {
    printf("caught %d\n", sig == SIGFPE);
    fflush(stdout);
    exit(0);
}

int main(void) {
    signal(SIGFPE, caught);
    printf("%d\n", 10 / zero);
    return 1;
}
