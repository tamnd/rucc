// Reading and writing a run of bits, which is a load and a shift one way and a load, a mask
// and a store the other.

struct flags {
  unsigned ready : 1;
  unsigned kind : 5;
  signed level : 10;
  int wide : 24;
  char tail;
};

union overlaid {
  int narrow : 3;
  char whole;
};

int read_signed(struct flags *f) { return f->level; }

unsigned read_unsigned(struct flags *f) { return f->kind; }

// The bits of `wide` are in three bytes and `tail` is in the fourth, so a store that took four
// bytes at once would write over a member the memory model says is separate.
void write_wide(struct flags *f, int value) { f->wide = value; }

int add_to_a_field(struct flags *f) {
  f->kind += 3;
  return f->kind;
}

// What a prefix increment is worth is what is in the field afterwards, which is what fits.
unsigned wrap_around(struct flags *f) { return ++f->kind; }

int through_a_union(union overlaid *u) { return u->narrow; }

int local_initializer(void) {
  struct flags f = { 1, 2, -3, 4, 'x' };
  return f.level;
}

struct flags at_rest = { 1, 2, -3, 4, 'x' };

// Bit-fields named out of order still land in the bytes their offsets ask for, and a field
// named twice takes the last value rather than the two of them put together.
struct flags backwards = { .wide = 4, .kind = 2 };
struct flags twice = { .kind = 2, .kind = 5 };
