/* accept: c23 gnu23 */
/* reject: c89 c99 c11 c17 gnu89 gnu99 gnu11 gnu17 */
/* Being usable where a constant is required is the whole reason the keyword exists. A named
   constant of an arithmetic type is one, and so is a member of one of a structure type, which
   is the pair C23 6.6p8 lists. A subscript of one is not on that list and is a variably
   modified type in gcc 16 too, so there is no case for it here. */

constexpr int side = 4;
int square[side * side];

constexpr int wider = side + 1;
int rectangle[wider];

/* A floating constant counts only as the immediate operand of a cast, 6.6p8 again, so the
   multiplication is on the integer side of it. */
constexpr double half = 1.5;
int rounded[(int)half * 2];

struct point {
  int x;
  int y;
};
constexpr struct point origin = {5, 6};
int across[origin.y];

enum named { four = side };

_Static_assert(side == 4, "a named constant is a constant expression");
_Static_assert(sizeof(square) == 64, "sixteen of them");
_Static_assert(sizeof(across) == 24, "a member of one is as well");
_Static_assert(sizeof(rounded) == 8, "a floating one under a cast counts too");
