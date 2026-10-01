/* accept: all */
/* The attribute promises callers that nothing is clobbered, so they keep values in registers the
   convention says a call destroys, and the function saves every one it writes. Built without the
   vector registers and the x87 stack it compiles. The armoured spelling is the one a header
   writes. */

int counter;
__attribute__((__no_caller_saved_registers__, target("general-regs-only"))) void tick(void)
{
    counter++;
}
