// Bulk memory operations over an object whose length the program computes, where the count is an
// operand rather than a number beside the instruction. Both of these are the whole object at once,
// which is the only place a length that is not a number comes from: reaching one element or one
// member is address arithmetic and was already built out of the same sizes.

int use(void *);

// An empty initializer is the only initializer C allows on one of these, and 6.7.11p11 says it
// zeroes every element and the padding, so the fill covers the whole object. The length is the
// same multiplication the `alloca` in front of it used.
long an_empty_initializer_on_an_array(int n) {
  char b[n] = {};
  return use(b);
}

// The same for a record with a member the program measures, where the length comes out of the
// recipe the layout left behind rather than out of one multiplication.
long an_empty_initializer_on_a_record(int n) {
  struct S {
    int a[n];
    int t;
  } s = {};
  return use(&s);
}

// `a = b` between two of them, which is a copy of a length the program works out. The two sides
// were declared from the same `n`, so one length does for both.
long a_whole_record_assigned(int n) {
  struct S {
    int a[n];
    int t;
  } x, y;
  x.t = 1;
  y = x;
  return use(&x) + use(&y);
}

// A fill of a fixed size still says so in the payload, because a constant operand would send a
// zeroing of eight bytes to the runtime's `memset` rather than to the two stores it is worth.
long a_fixed_record_is_unchanged(void) {
  struct F {
    int a;
    int b;
  } f = {1};
  return use(&f);
}
