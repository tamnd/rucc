/* reject: all */
/* message: 'no_caller_saved_registers' attribute is not supported */
/* The attribute promises callers that nothing is clobbered, so they keep values in registers the
   convention says a call destroys. Compiled as an ordinary function it destroys them, and the
   caller reads garbage, so it is refused rather than dropped. The armoured spelling is the one a
   header writes, and is refused the same way. */

int counter;
__attribute__((__no_caller_saved_registers__)) void tick(void) { counter++; }
