/* accept: c89 gnu89 */
/* reject: c99 c11 c17 c23 gnu99 gnu11 gnu17 gnu23 */
/* message: 'return' with a value, in function returning void */
/* warns: 'return' with a value, in function returning void */
/* There is nowhere for the value to go. C89 let it through with a complaint and C99 removed
   it, which is the split gcc still keeps, so the same sentence is a warning under the old
   dialects and an error under the rest. */

void f(void) {
  return 1;
}
