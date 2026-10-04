/* One half of a program whose other half, two.c, is built by another compiler. Each half saves a
 * place with __builtin_setjmp and calls into the other, which comes back to it with
 * __builtin_longjmp from a few frames down. That is how Postgres's server and its extensions meet
 * on MinGW-w64, where sigsetjmp is the builtin and PG_TRY in a module catches an elog(ERROR) the
 * server raises, so the three words of the buffer are an interface between the two compilers.
 * Each way runs a thousand rounds and adds up the round numbers it caught, so a round that comes
 * back with the wrong answer shows up in the sum and one that never comes back hangs. */
#include <stdio.h>

typedef void *jump_buf[5];

jump_buf *top;

long two_try(int rounds);
void two_raise(int depth);

__attribute__((noinline, noreturn)) void one_raise(int depth) {
    if (depth == 0)
        __builtin_longjmp(*top, 1);
    one_raise(depth - 1);
}

__attribute__((noinline)) long one_try(int rounds) {
    volatile long caught = 0;
    for (volatile int i = 0; i < rounds; i++) {
        jump_buf buf;
        jump_buf *volatile saved = top;
        if (__builtin_setjmp(buf) == 0) {
            top = &buf;
            two_raise(i % 7);
        } else {
            caught += i;
        }
        top = saved;
    }
    return caught;
}

int main(void) {
    long saved_here = one_try(1000);
    long saved_there = two_try(1000);
    printf("%ld %ld\n", saved_here, saved_there);
    return saved_here == 499500 && saved_there == 499500 ? 0 : 1;
}
