/* reject: all */
/* message: a function definition inside a function */
/* GNU's nested function, which gcc takes in its own dialects and this compiler turns down in
   every one of them. The reason is in `spec/13-gnu-compat.md` section 13.3: a call to one goes
   through a trampoline written on the stack, and a stack that can be executed is not something
   to build in now. The name is declared all the same, so the call under it resolves and the
   definition is one error rather than one for itself and one for every call. */

int f(void) {
  int g(void) { return 1; }
  return g();
}
