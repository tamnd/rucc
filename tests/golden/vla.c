// Arrays whose length is worked out while the program runs, which are the one kind of object
// whose slot cannot be an `alloca` at the top of the function. Each one is an `alloca` where the
// declaration is, and the stack it took is given back with a `stackrestore` at the end of the
// scope it was declared in.

int use(int *);
int side(void);

// The length is read where the declaration is and remembered, so `sizeof a` afterwards answers
// with what `n` was then and not with what it is now.
long one_dimension(int n) {
  int a[n];
  a[0] = 1;
  n = 0;
  return use(a) + (long)sizeof a;
}

// Two lengths, and a step over a row is a multiplication rather than a constant.
int two_dimensions(int n, int m) {
  int a[n][m];
  a[1][2] = 3;
  return (int)sizeof a + (int)sizeof a[0];
}

// A pointer to one of these is an ordinary pointer whose slot is ordinary, and what is variable
// about it is how far one step over it moves.
int through_a_pointer(int n, int (*p)[n]) {
  p[1][2] = 4;
  p += 2;
  return (int)(&p[3] - &p[1]) + p[0][0];
}

// A parameter written as an array of a variable length is a pointer, and the length in it is
// still evaluated on entry because the body can ask how far a row is.
int a_parameter(int n, int a[][n]) { return a[2][1]; }

// One in a loop is a new object each time round, so the stack is given back at the end of every
// iteration however the iteration ends.
void in_a_loop(int n) {
  for (int i = 0; i < n; i++) {
    int a[n + i];
    if (a[0]) {
      continue;
    }
    if (a[1]) {
      break;
    }
    use(a);
  }
}

// The length is evaluated once, so a call in it happens once however many times the array is
// mentioned afterwards.
int evaluated_once(void) {
  int a[side()];
  return (int)sizeof a + use(a);
}

// A scope that ends by leaving the function gives nothing back, since returning gives back the
// whole frame.
int returned_from(int n) {
  {
    int a[n];
    return use(a);
  }
}
