// The order the operands of an expression are evaluated in, where C says what the order is.

unsigned int x[1];
unsigned int side(void);

// `E1 op= E2` is `E1 = E1 op E2` with `E1` evaluated once, and C11 6.5.16.2 sequences the read of
// `E1` after the evaluation of `E2`. The address of `x[0]` is computed before the call and the
// load from it is after, so a call that writes `x[0]` is not overwritten by a value read before
// it ran.
void compound_reads_after_the_right_side(void) {
  x[0] |= side();
}

// The same shape one level up, where the left side is a pointer and the right side is a count.
void compound_on_a_pointer_reads_after_the_right_side(int **p) {
  *p += (int) side();
}

// The increments have no second operand to be sequenced against, so their read stays where it is
// and this is here to say that it does. They are written apart because both of them in one
// expression would be two writes to `x[0]` with no sequence point between them, which is
// undefined and so is not a thing to pin an expectation on.
unsigned int postfix(void) {
  return x[0]++;
}

unsigned int prefix(void) {
  return ++x[0];
}
