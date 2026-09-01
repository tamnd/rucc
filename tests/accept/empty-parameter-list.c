/* accept: c89 c99 c11 c17 gnu89 gnu99 gnu11 gnu17 */
/* reject: c23 gnu23 */
/* The one that goes backwards: `int f()` used to mean a function whose parameters are unknown,
   which could be called with anything. In C23 it means `(void)`, so the call below stops being
   allowed exactly when the language gets stricter. */

int f();

int call(void) {
  return f(1, 2);
}
