/* accept: all */
/* An interrupt handler returns with `iretq` and saves every register it touches. Built without the
   vector registers and the x87 stack, which it would have no way to put back, it compiles the way
   gcc compiles it. */

struct frame;
__attribute__((interrupt, target("general-regs-only"))) void handler(struct frame *frame)
{
    (void)frame;
}
