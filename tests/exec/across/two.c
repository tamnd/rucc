/* The other half of one.c, built by the other compiler. */

typedef void *jump_buf[5];

extern jump_buf *top;

void one_raise(int depth);

__attribute__((noinline, noreturn)) void two_raise(int depth) {
    if (depth == 0)
        __builtin_longjmp(*top, 1);
    two_raise(depth - 1);
}

__attribute__((noinline)) long two_try(int rounds) {
    volatile long caught = 0;
    for (volatile int i = 0; i < rounds; i++) {
        jump_buf buf;
        jump_buf *volatile saved = top;
        if (__builtin_setjmp(buf) == 0) {
            top = &buf;
            one_raise(i % 5);
        } else {
            caught += i;
        }
        top = saved;
    }
    return caught;
}
