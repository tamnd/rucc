// GNU's `vector_size`, which declares a type of several lanes that every operator works on at
// once. What the walk makes of one is the point of this case: a vector is an object rather than
// a value, and each operator over it comes out as that operator over each lane, so the file
// below has no vector in it anywhere past the types.
//
// Two lanes almost everywhere, because the shape of the walk is the same at any lane count and
// four of everything makes a diff nobody reads.

typedef int __attribute__((vector_size(8))) v2si;
typedef float __attribute__((vector_size(8))) v2sf;
typedef short __attribute__((vector_size(4))) v2hi;

// The two ways one is given a value: a braced list, which fills the lanes the way it fills an
// array, and another vector of the same type, which is a copy.
v2si built(void) {
  v2si a = { 1, 2 };
  v2si b = a;
  return b;
}

// Lane by lane, which is what these mean and what comes out of the walk.
v2si arithmetic(v2si a, v2si b) {
  return a * b;
}

// A scalar beside a vector stands for itself in every lane, which GNU calls the broadcast and
// which is a conversion of its own in the tree.
v2si broadcast(v2si a, int n) {
  return a + n;
}

// The compound form, where the object is read after the right side is worked out.
v2si compound(v2si a, v2si b) {
  a -= b;
  return a;
}

// The unary pair. A negation of a float lane is `fneg` and not zero minus the lane, which is
// the difference `-0.0` makes.
v2sf negated(v2sf a) {
  return -a;
}

v2si flipped(v2si a) {
  return ~a;
}

// A lane narrower than an `int` has no promotion to hide behind, so the divide is done at `int`
// and truncated back, which is the only width the back end has a rule for.
v2hi narrow(v2hi a, v2hi b) {
  return a / b;
}

// One lane, which is a subscript and is an lvalue when the vector is one.
int lane(v2si a, int i) {
  return a[i];
}

// A comparison, whose answer is a vector and not an `int`. Each lane is all ones where the
// comparison held and zero where it did not, which comes out as the lane's comparison, then a
// zero extension of the one bit, then zero minus it.
v2si compared(v2si a, v2si b) {
  return a < b;
}

// The same over floats, where the lanes read are floats and the lanes written are the integers
// of that width. That is the pair of types the arm has to keep apart.
v2si compared_floats(v2sf a, v2sf b) {
  return a == b;
}

// A mask used as a mask, which is the whole reason it is all ones rather than one.
v2si selected(v2si a, v2si b) {
  return (a > b) & a;
}

// A narrow lane, where the comparison is done at a word and truncated back for the same reason
// the divide above is.
v2hi compared_narrow(v2hi a, v2hi b) {
  return a >= b;
}

// A cast is a reinterpretation of the bytes rather than a conversion of a value, so the two
// sizes have to be equal and nothing is computed.
v2si reinterpreted(v2sf a) {
  return (v2si)a;
}

// The same read as one scalar, which is what a program writes to get at a whole vector at once.
long long as_one(v2si a) {
  return (long long)a;
}
