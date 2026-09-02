// std: gnu17
// A function declared without a prototype, which is what `int f();` meant in every dialect
// before C23 and is why this case names one. It takes whatever it is given, so a call written
// under it is checked against no parameter list at all, and a prototype further down is what
// the function ends up with.
//
// The call and the function can then disagree about the signature, and a call to a name has to
// be written with the signature the name has. Where the arguments travel the way the settled
// prototype wants, the call goes to the name; where they do not, it goes through the function's
// address, which is the shape a call through a function pointer already needs.

int settled();

// The arguments travel the way `int (int, int)` wants, so this is a call to the name.
int agrees(void) {
  return settled(1, 2);
}

// Three arguments to a function that takes two. It is undefined behaviour if control ever
// arrives here and the file still has to compile, so the call goes through the address.
int disagrees(void) {
  return settled(1, 2, 3);
}

int settled(int a, int b) {
  return a + b;
}

// The other half of the same rule: a call under `int g();` that passes nothing, against a
// `void` parameter list further down. The two signatures differ in whether more arguments may
// follow and nothing travels either way, so this is a call to the name as well.
int empty();

int nothing_passed(void) {
  return empty();
}

int empty(void) {
  return 7;
}
