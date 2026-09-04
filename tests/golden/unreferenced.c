// Which functions the file has a reason to emit. A function with internal linkage that nothing
// in the file refers to cannot be referred to from outside it either, so it is a definition of
// something that can never run and it is not emitted. What is here is the shapes that decide it:
// a call, an address, a mention in an image, an attribute that keeps one, and a pair of them that
// only call each other.
//
// Every function below that ends up in the `.ir` beside this is one something reaches. A reader
// checking this case should read the two lists against each other: `never_called`, `unreachable`
// and `also_unreachable` are the ones that are not there.

static int never_called(void) {
  return 1;
}

static int called(void) {
  return 2;
}

static int through_a_pointer(void) {
  return 3;
}

static int in_an_image(void) {
  return 4;
}

// Neither of these is reached, and each one refers to the other, which is why the answer is a
// worklist from the roots rather than a count of the references to each name.
static int unreachable(void);

static int also_unreachable(void) {
  return unreachable();
}

static int unreachable(void) {
  return also_unreachable();
}

// `used` says something outside the file reaches it, which is the whole reason a program writes
// the attribute, so this one stays without anything here naming it.
__attribute__((used)) static int kept_by_an_attribute(void) {
  return 5;
}

// An image that names a function is a reference to it, the same as a call is.
static int (*table[1])(void) = {in_an_image};

// Reached only from `called`, which is reached only from `main`, so the walk has to follow the
// chain rather than look one level down from the roots.
static int deeper(void) {
  return 6;
}

static int reaches_deeper(void) {
  return deeper();
}

int main(void) {
  int (*by_address)(void) = through_a_pointer;
  return called() + by_address() + table[0]() + reaches_deeper();
}
