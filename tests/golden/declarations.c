// What the front end has decided about each name: its type, its linkage, its storage, and
// how much of a definition the declaration is.

int external_tentative;
static int internal_tentative;
extern int only_declared;

int external_defined = 1;
static int internal_defined = 2;

static int internal_function(int a);
int external_function(int a);

int external_function(int a) {
  static int kept;
  extern int only_declared;
  register int fast = a;
  auto int ordinary = a;
  const int frozen = a;
  // The call is what keeps the internal function below in the output. One nothing refers to is
  // not emitted, since nothing outside this file can name it either, and `unreferenced.c` is
  // where that is shown.
  return kept + only_declared + fast + ordinary + frozen + internal_function(a);
}

static int internal_function(int a) {
  return a;
}

// Every file-scope declaration of this name writes `inline` and none writes `extern`, so what is
// here is an inline definition: nothing is emitted for it and a call goes to the external
// definition some other unit holds. The declaration is still in the module, since the calls need
// something to resolve against, and it is the body that is left out.
inline int always_inline_me(int a) {
  return a;
}

// One declaration without `inline` makes the definition an external one again, whichever side of
// the definition it is written on. This is the shape a file lands in by accident, with a header
// declaring the name plainly and the file defining it inline, and the definition being emitted is
// what stops the program failing to link.
inline int inline_but_also_declared(int a) {
  return a;
}

int inline_but_also_declared(int a);

// `extern` says the same thing on its own, which is how a file asks for one unit out of many to
// hold the external definition of a name every other unit defines inline.
extern inline int inline_and_extern(int a) {
  return a;
}

// The rule is about names with external linkage, so a `static inline` definition is emitted like
// any other static function that something refers to.
static inline int inline_and_static(int a) {
  return a;
}

int calls_the_inline_ones(int a) {
  return always_inline_me(a) + inline_but_also_declared(a) + inline_and_extern(a) +
         inline_and_static(a);
}

_Thread_local int per_thread;

typedef int alias;
alias uses_the_alias;

int redeclared;
int redeclared;

// A qualifier on a parameter belongs to the object and not to the function type, so the two
// declarations below are the same function and `frozen` is read-only inside the body.
void qualified_parameters(const int, int *const, int [const 3], volatile int);
void qualified_parameters(int frozen, int *fixed, int bounded[3], volatile int watched) {
  (void)frozen;
  (void)fixed;
  (void)bounded;
  (void)watched;
}

// `__func__` is declared by the language itself, as if `static const char __func__[] = "who";`
// had been written just inside the brace, so the name is there without anything declaring it and
// the two uses below are one object.
const char *my_name(void) {
  return __func__;
}

unsigned long how_long_my_name_is(void) {
  return sizeof __func__ + sizeof __func__;
}
