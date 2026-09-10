/* The second object of the same library, which is what makes the hidden name worth having: it is
 * reached from here and from nowhere outside the library. */

__attribute__((visibility("hidden"))) int hidden_helper(void) { return 9; }
