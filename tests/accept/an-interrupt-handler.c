/* reject: all */
/* message: 'interrupt' attribute is not supported */
/* An interrupt handler returns with `iret` and saves every register it touches. Compiled as an
   ordinary function it returns with `ret` and clobbers what the interrupted code was using, which
   links, runs and breaks the machine later, so it is refused rather than dropped. */

struct frame;
__attribute__((interrupt)) void handler(struct frame *frame) { (void)frame; }
