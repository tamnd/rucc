/* reject: all */
/* message: section attribute cannot be specified for local variables */
/* An object inside a block with no `static` is a slot in the frame, which no linker places, so gcc
   refuses a section on one rather than dropping it. A `static` one is fine and is below. */

int f(void) {
  static int kept __attribute__((section(".mine"))) = 1;
  __attribute__((section(".mine"))) int slot = 2;
  return kept + slot;
}
