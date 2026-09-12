// `_Complex`, which is a pair of real values held as one object. What the walk makes of one is
// the point of this case: a complex value has no form a register holds, the way a vector has
// none, so every operator over one comes out as that operator over its two halves and there is
// no complex anything left in the IR below.
//
// `_Complex` on an integer type is here too. It is gcc's extension rather than C's and the halves
// are integers, so the same walk comes out in integer opcodes and the two operators the runtime
// answers for a floating pair are written out instead.

// `_Complex` written on its own, which gcc reads as `_Complex double`. No edition of C wrote a
// rule for it, so `-pedantic` says so and nothing else does, and the type below is the same one
// the rest of this file is about.
_Complex bare(_Complex a) {
  return a;
}

// The two ways one is given a value: a real one, which fills the real half and zeroes the other,
// and another complex one, which is a copy.
_Complex double built(double x) {
  _Complex double a = x;
  _Complex double b = a;
  return b;
}

// Half by half, which is what these mean and what comes out of the walk.
_Complex double added(_Complex double a, _Complex double b) {
  return a + b;
}

// A negation of a half is `fneg` and not zero minus the half, which is the difference `-0.0`
// makes and is the same answer a real operand gets.
_Complex double negated(_Complex double a) {
  return -a;
}

// The conjugate, which gcc reads `~` as on a complex operand. It is the negation above on the
// imaginary half and the real half copied, so there is one `fneg` here and two up there.
_Complex double conjugate(_Complex double a) {
  return ~a;
}

// The same operation under the name `complex.h` gives it, and the two halves under theirs. All
// three are answered here rather than called, because the operators they stand for are in the
// language already and because the math library is not on the link line of a program that wrote
// one and nothing else.
_Complex float named(_Complex float a) {
  return __builtin_conjf(a);
}

double halves(_Complex double a) {
  return __builtin_creal(a) + __builtin_cimag(a);
}

// The compound form, where the object is read after the right side is worked out.
_Complex double compound(_Complex double a, _Complex double b) {
  a -= b;
  return a;
}

// The two halves, which are reached by offset and by nothing else.
double real_half(_Complex double a) {
  return __real__ a;
}

double imaginary_half(_Complex double a) {
  return __imag__ a;
}

// Both of them are lvalues wherever the operand is one, which gcc has always said and which is
// the only way a program can give the imaginary half a value until the imaginary constants
// arrive. So this is how a complex value is built out of two real ones.
_Complex double assembled(double x, double y) {
  _Complex double z;
  __real__ z = x;
  __imag__ z = y;
  return z;
}

// Equality, which is both halves compared and the two answers combined. There is no `<` beside
// it, because a complex operand is not a real one and C says an ordering on two of them is a
// constraint violation.
int equal(_Complex double a, _Complex double b) {
  return a == b;
}

// One as a condition, which is whether either half is not zero.
int nonzero(_Complex float a) {
  return a ? 1 : 0;
}

// The three conversions. A real value to a complex type fills one half and zeroes the other, one
// complex type to another converts each half, and a complex value to a real type keeps the real
// half and drops the imaginary one.
_Complex float narrowed(_Complex double a) {
  return a;
}

double dropped(_Complex double a) {
  return (double)a;
}

// `_Complex float` is one eightbyte of SSE and `_Complex double` is two, which is the psABI's
// answer for both and is what the signatures here show.
_Complex float small(_Complex float a, _Complex float b) {
  return a + b;
}

// A complex member, which is laid out and copied the way any other member of its size is.
struct wrapped {
  _Complex double z;
  int tag;
};

struct wrapped tagged(_Complex double z) {
  struct wrapped w = { z, 1 };
  return w;
}

// An imaginary constant, which gcc reads as a complex value whose real half is a zero. So `1.5 +
// 2.5i` is a sum of two complex values and the folding works it out, which is what a static
// initializer needs: the image below is the two halves and no code at all.
_Complex double pinned = 1.5 + 2.5i;
_Complex float small_pinned = 1.5f + 2.5if;

// The same constant inside a function, where the folding has already made the pair and what is
// left is the two stores.
_Complex double shifted(_Complex double z) {
  return z + 2.5i;
}

// The multiply and the divide, which are the two the runtime answers. Neither is the formula
// written out, because C annex G has rules about what each does when a half is an infinity or a
// nan that the formula gets wrong, so both are the call gcc makes of them and both go to libgcc's
// name for the format the halves are in.
_Complex double multiplied(_Complex double a, _Complex double b) {
  return a * b;
}

_Complex float divided(_Complex float a, _Complex float b) {
  return a / b;
}

// The compound form of the multiply, where the object is read, widened to the type the operator
// works in, and the answer narrowed back. `_Complex float *= _Complex double` is the pair that
// makes all three of those visible.
_Complex float scaled(_Complex float a, _Complex double b) {
  a *= b;
  return a;
}

// The same two on constants, which the folding works out rather than the runtime. Smith's method
// is what the divide folds through, which is what the routine does, so the image below is the
// answer the call would have given.
_Complex double product = (1.5 + 2.5i) * (0.5 + 4.0i);
_Complex double quotient = (1.0 + 2.0i) / (3.0 + 4.0i);

// `_Complex` on an integer type, which is gcc's and which the two halves being integers is the
// whole of. The type is two of whatever the half is, so `_Complex char` is two bytes, and the ABI
// asks about the half rather than about the keyword, so this pair is integer registers where the
// one above is SSE ones.
_Complex int integral(_Complex int a, _Complex int b) {
  return a + b;
}

// The negation and the conjugate on integer halves, which are the integer negation the halves
// would get on their own rather than the `fneg` above.
_Complex int flipped(_Complex int a) {
  return -a;
}

_Complex int mirrored(_Complex int a) {
  return ~a;
}

// The multiply, which is the four products written out. There is no routine to call for it: the
// libgcc family is written over the floating formats and has no member for an integer half.
_Complex int scaled_up(_Complex int a, _Complex int b) {
  return a * b;
}

// The divide, which is Smith's method the same way the floating one is, with an integer division
// at every step. gcc writes this one out too and writes it in exactly this shape, and an integer
// division truncates, so the shape is what the answer is and not a detail underneath it.
_Complex int shared(_Complex int a, _Complex int b) {
  return a / b;
}

// The same on an unsigned half, where there is no sign to take off the two halves of the divisor
// before they are compared.
_Complex unsigned unsigned_shared(_Complex unsigned a, _Complex unsigned b) {
  return a / b;
}

// The imaginary suffix on an integer constant, which names the complex type whose half is the type
// the rest of the suffix named. So this is a `_Complex long` and the one below it is a
// `_Complex int`, and both fold into the pair the image holds.
_Complex long wide_pinned = 3l + 4li;
_Complex int integral_pinned = 3 + 4i;

// The two the folding works out on integer halves, which have to agree with the two functions
// above: a program that divides two constants and a program that divides two variables holding
// those constants are the same program.
_Complex int integral_product = (3 + 4i) * (1 + 2i);
_Complex int integral_quotient = (3 + 4i) / (1 + 2i);

// Both halves compared, and one as a condition, on integer halves. `icmp` where the pair above
// gives `fcmp`, and nothing else about either changes.
int integral_equal(_Complex int a, _Complex int b) {
  return a == b;
}

int integral_nonzero(_Complex int a) {
  return a ? 1 : 0;
}

// The conversions across the two families, which convert each half the way the halves convert.
_Complex double widened(_Complex int a) {
  return a;
}

_Complex int truncated(_Complex double a) {
  return a;
}

// And a narrow half, to show that the layout is the half's own size and alignment doubled in size
// and left alone in alignment.
_Complex char narrow(_Complex char a, _Complex char b) {
  return a + b;
}
