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
