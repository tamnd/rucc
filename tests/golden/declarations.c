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
  return kept + only_declared + fast + ordinary + frozen;
}

static int internal_function(int a) {
  return a;
}

inline int always_inline_me(int a) {
  return a;
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
