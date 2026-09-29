/* setjmp and longjmp through a jmp_buf in a struct, in a global and on the stack.
 *
 * mingw-w64 declares jmp_buf as an array of _JBTYPE, which is a sixteen byte struct that a typedef
 * aligns to sixteen, and msvcrt's setjmp stores XMM6 to XMM15 into it with movdqa. An array of that
 * typedef that is only as aligned as the struct behind it lands at a multiple of eight, and the
 * first setjmp into one faults. The one in a struct after a char is how unity holds its jmp_buf,
 * which is where cJSON's tests found it. tamnd/rucc#2151. */
#include <setjmp.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

struct frame {
    char tag;
    jmp_buf env;
    int depth;
};

static char before;
static jmp_buf global;
static struct frame framed;

__attribute__((noinline)) static void down(jmp_buf *env, int n) {
    volatile char pad[24];
    pad[0] = (char)n;
    if (n == 0)
        longjmp(*env, 7 + pad[0]);
    down(env, n - 1);
}

__attribute__((noinline)) static int jump(jmp_buf *env, const char *name) {
    int got = setjmp(*env);
    if (got == 0) {
        down(env, 4);
        return -1;
    }
    printf("%s returned %d, aligned %d\n", name, got, (int)((uintptr_t)env % 16 == 0));
    return got;
}

int main(void) {
    char local_before = 1;
    jmp_buf local;
    before = local_before;
    printf("_Alignof(jmp_buf) %d\n", (int)_Alignof(jmp_buf));
    printf("sizeof(jmp_buf) %d\n", (int)sizeof(jmp_buf));
    printf("offsetof(struct frame, env) %d\n", (int)offsetof(struct frame, env));
    printf("_Alignof(struct frame) %d sizeof %d\n", (int)_Alignof(struct frame),
           (int)sizeof(struct frame));
    int sum = jump(&framed.env, "struct") + jump(&global, "global") + jump(&local, "local");
    printf("sum %d\n", sum + before - 1);
    return sum == 21 ? 0 : 1;
}
