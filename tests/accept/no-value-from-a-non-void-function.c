/* accept: c89 gnu89 */
/* reject: c99 c11 c17 c23 gnu99 gnu11 gnu17 gnu23 */
/* message: 'return' with no value, in function returning non-void */
/* The other half of the C89 rule, and the half that is silent rather than a warning: falling
   out of a function that promised a value was how a great deal of C was written. */

int f(void) {
  return;
}
