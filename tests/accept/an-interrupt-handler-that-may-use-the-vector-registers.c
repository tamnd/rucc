/* reject: all */
/* message: SSE instructions aren't allowed in an interrupt service routine */
/* An interrupt handler saves the general purpose registers it writes and nothing else, so one
   built for a target with the vector registers could change one the interrupted code was using.
   gcc says sorry for it, and so does this. */

struct frame;
__attribute__((interrupt)) void handler(struct frame *frame) { (void)frame; }
