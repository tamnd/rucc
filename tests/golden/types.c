// The types the front end builds, printed the way a declaration would spell them.

struct point { int x; int y; };
union either { int as_int; float as_float; };
enum colour { red, green = 10, blue };

struct bits {
  unsigned flag : 1;
  unsigned rest : 7;
  signed wide : 20;
  int : 0;
  int after;
};

struct self_referential {
  struct self_referential *next;
  int value;
};

typedef int (*callback)(int, long);
typedef int matrix[2][3];

struct point by_value;
struct point *by_pointer;
const int constant;
volatile int changing;
const volatile int both;
int *const pointer_that_cannot_move;
const int *pointer_to_something_frozen;
_Atomic int shared;
_BitInt(12) narrow;
unsigned _BitInt(128) very_wide;
callback held;
matrix grid;
int unsized[];
float single;
double twice;
long double widest;
_Float16 half;
_Float128 quadruple;
long long biggest;
unsigned long long biggest_unsigned;
char signedness_is_the_target_s_business;
signed char definitely_signed;
unsigned char definitely_unsigned;

int use(struct bits *b, enum colour c, callback f, matrix m) {
  return b->flag + b->wide + (int)c + f(1, 2) + m[1][2];
}
