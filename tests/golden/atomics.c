// `_Atomic`, which is a qualifier that changes what an access to the object is. A plain read of
// one is a sequentially consistent load and a plain write is a sequentially consistent store,
// which C11 6.5.16p3 and 5.1.2.4 say between them, and a compound assignment or a `++` on one is
// a single read modify write rather than the three steps it is written as.
//
// What the walk makes of one is the point of this case. The whole difference is in the
// instruction that reaches memory, so every function below is the ordinary function beside it
// with the qualifier added, and the diff between the two is the feature.

_Atomic int g;
_Atomic char c;
_Atomic double d;
int * _Atomic p;
int room[8];

// The two plain accesses, which are the ones with no operator on them at all.
void put(int v) {
  g = v;
}

int get(void) {
  return g;
}

// A compound assignment the machine has an instruction for, which is the five operators an
// `atomic_rmw` names, at the width the object has.
void add(int v) {
  g += v;
}

void narrow(int v) {
  c += v;
}

void bits(int v) {
  g &= v;
  g |= v;
  g ^= v;
}

// One it has no instruction for, which is a load and then a compare and exchange until the value
// found was the value the operation started from.
void multiply(int v) {
  g *= v;
}

// The same, on a float, where the object is exchanged as the integer of its width and the
// arithmetic happens on the value that integer holds.
void grow(double v) {
  d += v;
}

// `++` and `--`, in both positions, which differ only in which of the two values the step
// produced is the value of the expression.
int steps(void) {
  int before = g++;
  int after = ++g;
  g--;
  --g;
  return before + after;
}

// An atomic pointer, whose step is a number of elements and reaches memory as the number of
// bytes those elements are.
void walk(void) {
  p = room;
  p += 3;
  p++;
}

// The qualifier on a member and on an element, which are places rather than whole objects and
// are reached the same way.
struct Holder {
  int pad;
  _Atomic int m;
};

int member(struct Holder *h) {
  h->m = 5;
  h->m += 2;
  return h->m;
}

_Atomic int table[4];

int element(int i) {
  table[i] = 1;
  table[i] += 2;
  return table[i];
}

// Both words at once, which C allows and which say different things: the ordering is what other
// threads see and the qualifier is what the compiler may leave out.
volatile _Atomic int both;

void mixed(void) {
  both = 1;
  both += 2;
}

// A local, which gets a slot whether or not the program takes its address, because what makes an
// access atomic is the instruction that reaches memory and a variable in a register has none.
int local(int v) {
  _Atomic int n = 0;
  n += v;
  return n;
}

// A width no machine reaches in one go, which is a call into the runtime's table of locks rather
// than an instruction. The object travels through its address and the value through a slot of our
// own, so a read is the call and then an ordinary load of the slot, and a compound assignment is
// the same compare and exchange loop as above with both halves of it being calls.
_Atomic __int128 wide;

void widen(__int128 v) {
  wide = v;
}

__int128 taken(void) {
  return wide;
}

void raise(__int128 v) {
  wide += v;
}

// The builtins at that width, which go to the same four routines. A load and a store are the call
// with a slot of ours on the value side, an exchange is the routine of that name, and the twelve
// that read and write back are the loop, since the library has nothing for those. The ordering the
// program named travels to the call rather than being replaced by the strongest, which is where
// these differ from the operators above.
__int128 fetch(void) {
  return __atomic_load_n(&wide, __ATOMIC_ACQUIRE);
}

__int128 swap(__int128 v) {
  return __atomic_exchange_n(&wide, v, __ATOMIC_ACQ_REL);
}

__int128 bump(__int128 v) {
  return __atomic_add_fetch(&wide, v, __ATOMIC_SEQ_CST);
}

// The expected value arrives through the program's own pointer here, and the routine writes what
// was really there back over it when the exchange did not happen, so there is no branch to write.
_Bool swing(__int128 *want, __int128 v) {
  return __atomic_compare_exchange_n(&wide, want, v, 0, __ATOMIC_SEQ_CST, __ATOMIC_RELAXED);
}
