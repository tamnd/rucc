/* flags: -fno-omit-frame-pointer */
/* __builtin_return_address and __builtin_frame_address above depth zero, which walk the chain of
 * saved frame pointers. On x86_64 Windows the chain only runs through a function that pushes the
 * frame pointer and points it at where it pushed it before it takes the frame, which is what gcc
 * does when the frame pointer is the one register a function saves. tamnd/rucc#2144. */
#include <stdio.h>

static void *ret[3], *frame[3], *up[2], *above[2];

__attribute__((noinline)) static int c(int x) {
    ret[2] = __builtin_return_address(0);
    frame[2] = __builtin_frame_address(0);
    up[0] = __builtin_return_address(1);
    up[1] = __builtin_return_address(2);
    above[0] = __builtin_frame_address(1);
    above[1] = __builtin_frame_address(2);
    return x + 1;
}

__attribute__((noinline)) static int b(int x) {
    ret[1] = __builtin_return_address(0);
    frame[1] = __builtin_frame_address(0);
    volatile int r = c(x);
    return r + 1;
}

__attribute__((noinline)) static int a(int x) {
    ret[0] = __builtin_return_address(0);
    frame[0] = __builtin_frame_address(0);
    volatile int r = b(x);
    return r + 1;
}

int main(void) {
    printf("%d\n", a(1));
    printf("return address one up: %s\n", up[0] == ret[1] ? "same" : "differs");
    printf("return address two up: %s\n", up[1] == ret[0] ? "same" : "differs");
    printf("frame address one up: %s\n", above[0] == frame[1] ? "same" : "differs");
    printf("frame address two up: %s\n", above[1] == frame[0] ? "same" : "differs");
    return 0;
}
