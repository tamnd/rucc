/* An interpreter whose jumps go to a step by number, as the jump steps of Postgres'
 * ExecInterpExpr do. A step is 64 bytes, so the step a jump goes to is the steps plus the number
 * shifted by six, and the number is read off the step the jump leaves. */
#include <stdio.h>

struct step {
    int kind;
    int jumpdone;
    const long *value;
    long pad[6];
};

struct state {
    struct step *steps;
    long calls;
};

enum { DONE, ADD, JUMP_IF_ZERO, JUMP_IF_NOT_ZERO };

long run(struct state *state, long acc) {
    static void *const ops[] = { &&done, &&add, &&jump_if_zero, &&jump_if_not_zero };
    struct step *op = state->steps;
    state->calls++;
    goto *ops[op->kind];
add:
    acc += *op->value;
    op++;
    goto *ops[op->kind];
jump_if_zero:
    if (*op->value == 0) {
        op = &state->steps[op->jumpdone];
        goto *ops[op->kind];
    }
    op++;
    goto *ops[op->kind];
jump_if_not_zero:
    if (*op->value != 0) {
        op = &state->steps[op->jumpdone];
        goto *ops[op->kind];
    }
    op++;
    goto *ops[op->kind];
done:
    return acc;
}

#ifdef RUN
int main(void) {
    static const long zero = 0, one = 1, a = 1, b = 10, c = 100, d = 1000;
    struct step steps[] = {
        { ADD, 0, &a },
        { JUMP_IF_ZERO, 3, &zero },
        { ADD, 0, &b },
        { JUMP_IF_NOT_ZERO, 5, &one },
        { ADD, 0, &c },
        { JUMP_IF_ZERO, 7, &one },
        { ADD, 0, &d },
        { JUMP_IF_NOT_ZERO, 0, &zero },
        { DONE, 0, 0 },
    };
    struct state state = { steps, 0 };
    long sum = 0;
    for (int i = 0; i < 1000; i++)
        sum += run(&state, i);
    printf("%ld %ld\n", sum, state.calls);
    return 0;
}
#endif
