// Structures and unions passed and returned by value, which is the one part of a call whose
// answer is the target's rather than C's. Everything here is compiled for x86-64 SysV, where an
// aggregate of at most two eightbytes travels in registers picked by what its members are and
// anything larger travels as bytes in the argument area.

struct pair {
  int a, b;
};

struct mixed {
  double d;
  int i;
};

struct floats {
  float x, y, z;
};

struct odd {
  char b[5];
};

struct big {
  double v[8];
};

struct empty {
};

union both {
  double d;
  long l;
};

struct pair make_pair(int a, int b);
struct mixed make_mixed(void);
struct floats make_floats(void);
struct odd make_odd(void);
struct big make_big(void);
struct empty make_empty(void);
union both make_both(void);

int take_pair(struct pair p);
int take_mixed(struct mixed m);
int take_floats(struct floats f);
int take_odd(struct odd o);
int take_big(struct big b);
int take_empty(struct empty e);

// Two integers in one eightbyte, in and out.
int round_trip_pair(struct pair p) { return take_pair(p); }

// One eightbyte of each class, which is one register of each bank.
int round_trip_mixed(struct mixed m) { return take_mixed(m); }

// Two floats in one register and the third in half of another.
int round_trip_floats(struct floats f) { return take_floats(f); }

// Five bytes read as a whole eightbyte, which the walk does through a buffer so that nothing
// reads past the end of the object.
int round_trip_odd(struct odd o) { return take_odd(o); }

// Over two eightbytes: the bytes go in the argument area and the caller says where to write the
// return value.
struct big round_trip_big(struct big b) { return make_big(); }

// Nothing of it travels and the argument is still evaluated.
int round_trip_empty(struct empty e) { return take_empty(e); }

// A union is classified by everything in it at once, so this is one integer register.
long from_union(void) { return make_both().l; }

// What a call produced has to be somewhere before a member of it can be read.
int member_of_a_call(void) { return make_pair(1, 2).b + make_mixed().i; }

// The object a call writes into is the one the assignment names, when it can be.
struct pair global_pair;

void assign_from_a_call(void) { global_pair = make_pair(3, 4); }

// A call whose value nobody wants still passes somewhere to put it, because the callee writes
// it either way.
void call_and_throw_away(void) {
  make_big();
  make_floats();
  make_empty();
}

// A call through a pointer is classified from the type of the pointer, which is the only thing
// there is to classify it from.
int through_a_pointer(struct pair (*fp)(int, int)) { return fp(5, 6).a; }

// Passing back what was passed in, which arrives in registers, goes into the object, and comes
// out of it again.
struct mixed forward(struct mixed m) { return m; }
